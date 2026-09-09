use global_accountant_definitions::GlobalAccountantError;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use crate::common::*;

const CLOSER: Pubkey = Pubkey::new_from_array([0x22u8; 32]);
const NOW: i64 = 1_800_000_000;

/// `close_pending` must read expiry from the Core Bridge set at the recorded index only.
/// Any other genuine set is rejected; the recorded set closes only once expired, or once
/// NoReplay is marked.
#[test]
fn closes_only_on_recorded_set_expiry_or_noreplay_mark() {
    let mut mollusk = mollusk();
    mollusk.sysvars.clock.unix_timestamp = NOW;
    let scenario = ObsScenario::attest(GUARDIAN_COUNT, GUARDIAN_SET_INDEX, 0x71);

    type Setup = fn(&ObsScenario, &mut Vec<(Pubkey, Account)>) -> Pubkey;
    let cases: [(&str, Setup, Option<GlobalAccountantError>); 5] = [
        (
            "other epoch set",
            |s, accounts| {
                let keys = guardian_keys(&s.guardians);
                let set =
                    derive_guardian_set_pda(GUARDIAN_SET_INDEX + 1, &core_bridge_program_id()).0;
                accounts.push((
                    set,
                    guardian_set_account(
                        GUARDIAN_SET_INDEX + 1,
                        &keys,
                        0,
                        0,
                        &core_bridge_program_id(),
                    ),
                ));
                set
            },
            Some(GlobalAccountantError::InvalidPda),
        ),
        (
            "recorded set active",
            |s, _| s.guardian_set,
            Some(GlobalAccountantError::CannotCleanup),
        ),
        (
            "recorded set superseded, inside expiry window",
            |s, accounts| {
                let keys = guardian_keys(&s.guardians);
                replace_account(
                    accounts,
                    &s.guardian_set,
                    guardian_set_account(
                        GUARDIAN_SET_INDEX,
                        &keys,
                        0,
                        NOW as u32 + 100,
                        &core_bridge_program_id(),
                    ),
                );
                s.guardian_set
            },
            Some(GlobalAccountantError::CannotCleanup),
        ),
        (
            "recorded set expired",
            |s, accounts| {
                let keys = guardian_keys(&s.guardians);
                replace_account(
                    accounts,
                    &s.guardian_set,
                    guardian_set_account(
                        GUARDIAN_SET_INDEX,
                        &keys,
                        0,
                        NOW as u32 - 1,
                        &core_bridge_program_id(),
                    ),
                );
                s.guardian_set
            },
            None,
        ),
        (
            "noreplay marked",
            |s, accounts| {
                replace_account(
                    accounts,
                    &s.noreplay_bucket,
                    noreplay_bucket_marked(s.sequence),
                );
                s.guardian_set
            },
            None,
        ),
    ];

    for (label, setup, expected) in cases {
        let mut accounts = scenario.submit_n(&mollusk, 3);
        accounts.push((CLOSER, system_owned_account(1_000_000_000)));
        let guardian_set = setup(&scenario, &mut accounts);
        let pending_lamports = find_account(&accounts, &scenario.pending_pda).lamports;
        let payer_lamports = find_account(&accounts, &SUBMITTER).lamports;
        assert!(pending_lamports > 0, "{label}: pending funded");

        let ix = Instruction::new_with_bytes(
            program_id(),
            &close_pending_ix_data(scenario.emitter, scenario.sequence),
            vec![
                AccountMeta::new(CLOSER, true),
                AccountMeta::new(scenario.pending_pda, false),
                AccountMeta::new(SUBMITTER, false),
                AccountMeta::new_readonly(guardian_set, false),
                AccountMeta::new_readonly(scenario.noreplay_bucket, false),
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
                    find_account(after, &scenario.pending_pda).lamports,
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
