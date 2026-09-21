use global_accountant_definitions::GlobalAccountantError;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use crate::common::*;

const CLOSER: Pubkey = Pubkey::new_from_array([0x22u8; 32]);

/// The recorded set is active and NoReplay is unmarked: the pending PDA stays. Once NoReplay
/// is marked, anyone closes it and the recorded payer is refunded. Expiry handling is the
/// shared handler's and is covered by the WTT suite.
#[test]
fn closes_once_noreplay_is_marked() {
    let mollusk = mollusk();
    let s = ObsScenario::new(
        Observation::direct(SOLANA, HUB, 6, ETHEREUM, 6, 1_500_000),
        SPOKE,
        (SOLANA, HUB),
    );

    type Setup = fn(&ObsScenario, &mut Vec<(Pubkey, Account)>);
    let cases: [(&str, Setup, Option<GlobalAccountantError>); 2] = [
        (
            "recorded set active, noreplay unmarked",
            |_, _| {},
            Some(GlobalAccountantError::CannotCleanup),
        ),
        (
            "noreplay marked",
            |s, accounts| {
                replace_account(
                    accounts,
                    &s.noreplay_bucket,
                    noreplay_bucket_marked(s.obs.sequence),
                );
            },
            None,
        ),
    ];

    for (label, setup, expected) in cases {
        let mut accounts = s.submit_n(&mollusk, 3);
        accounts.push((CLOSER, system_owned_account(1_000_000_000)));
        setup(&s, &mut accounts);
        let pending_lamports = find_account(&accounts, &s.pending_pda).lamports;
        let payer_lamports = find_account(&accounts, &SUBMITTER).lamports;
        assert!(pending_lamports > 0, "{label}: pending funded");

        let ix = Instruction::new_with_bytes(
            program_id(),
            &close_pending_ix_data(s.obs.emitter, s.obs.sequence),
            vec![
                AccountMeta::new(CLOSER, true),
                AccountMeta::new(s.pending_pda, false),
                AccountMeta::new(SUBMITTER, false),
                AccountMeta::new_readonly(s.guardian_set, false),
                AccountMeta::new_readonly(s.noreplay_bucket, false),
            ],
        );
        let before = accounts.clone();
        let result = mollusk.process_instruction(&ix, &accounts);
        let after = &result.resulting_accounts;
        match expected {
            Some(code) => {
                assert_error(&result, code as u64, label);
                assert_eq!(*after, before, "{label}: state untouched");
            }
            None => {
                assert_success(&result, label);
                assert_eq!(
                    find_account(after, &s.pending_pda).lamports,
                    0,
                    "{label}: closed"
                );
                assert_eq!(
                    find_account(after, &SUBMITTER).lamports,
                    payer_lamports + pending_lamports,
                    "{label}: payer refunded"
                );
            }
        }
    }
}
