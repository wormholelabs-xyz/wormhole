use global_accountant_definitions::{GlobalAccountantError, Uint256};
use solana_account::Account;
use solana_pubkey::Pubkey;

use crate::common::*;

#[test]
fn transfer_commits_and_marks_noreplay() {
    let mollusk = mollusk();
    let scenario = VaaScenario::transfer(Transfer::new(0xA0, ETHEREUM, SOLANA, 500_000));

    assert_eq!(
        scenario
            .account_metas()
            .iter()
            .map(|m| (m.is_signer, m.is_writable))
            .collect::<Vec<_>>(),
        vec![
            (true, true),
            (false, false),
            (false, false),
            (false, false),
            (false, true),
            (false, false),
            (false, false),
            (false, true),
            (false, true),
            (false, false),
            (false, false),
        ]
    );

    let result = scenario.submit(&mollusk, scenario.accounts());
    assert_success(&result, "transfer");
    let after = &result.resulting_accounts;
    assert_bucket_marked(
        find_account(after, &scenario.noreplay_bucket),
        scenario.sequence,
    );
    assert_balance(after, &scenario.source_account, Uint256::from_u128(500_000));
    assert_balance(after, &scenario.dest_account, Uint256::from_u128(500_000));
}

#[test]
fn rejects() {
    let mollusk = mollusk();
    type Mutate = fn(&mut VaaScenario, &mut Vec<(Pubkey, Account)>) -> Option<Vec<u8>>;
    let cases: [(&str, VaaScenario, Mutate, GlobalAccountantError); 6] = [
        (
            "pre-marked noreplay leaves state untouched",
            VaaScenario::transfer(Transfer::new(0xA1, ETHEREUM, SOLANA, 100)),
            |s, accounts| {
                replace_account(
                    accounts,
                    &s.noreplay_bucket,
                    noreplay_bucket_marked(s.sequence),
                );
                None
            },
            GlobalAccountantError::AlreadyAccounted,
        ),
        (
            "wrapped source underflow",
            VaaScenario::transfer(Transfer::new(0xA2, SOLANA, ETHEREUM, 1_000)),
            |_, _| None,
            GlobalAccountantError::BalanceUnderflow,
        ),
        (
            "missing chain registration",
            VaaScenario::transfer(Transfer::new(0xA3, ETHEREUM, SOLANA, 100)),
            |s, accounts| {
                replace_account(accounts, &s.chain_registration, uninitialised_pda_account());
                None
            },
            GlobalAccountantError::MissingChainRegistration,
        ),
        (
            "unregistered emitter",
            VaaScenario::transfer(Transfer::new(0xA4, ETHEREUM, SOLANA, 100)),
            |s, accounts| {
                replace_account(
                    accounts,
                    &s.chain_registration,
                    chain_registration_account(s.chain, [0xCC; 32]),
                );
                None
            },
            GlobalAccountantError::UnregisteredEmitter,
        ),
        (
            "body_len mismatch",
            VaaScenario::transfer(Transfer::new(0xA5, ETHEREUM, SOLANA, 100)),
            |s, _| {
                Some(submit_vaas_ix_data_with_len(
                    s.guardian_set_bump,
                    s.body.len() as u16 + 1,
                    &s.body,
                ))
            },
            GlobalAccountantError::InvalidInstructionData,
        ),
        (
            "truncated transfer payload",
            VaaScenario::transfer(Transfer::new(0xA6, ETHEREUM, SOLANA, 100)),
            |s, accounts| {
                s.body.truncate(60);
                *accounts = s.accounts();
                None
            },
            GlobalAccountantError::InvalidInstructionData,
        ),
    ];

    for (label, mut scenario, mutate, expected) in cases {
        let mut accounts = scenario.accounts();
        let ix_data = mutate(&mut scenario, &mut accounts);
        let before = accounts.clone();
        let result = match ix_data {
            Some(data) => scenario.submit_with(&mollusk, accounts, data),
            None => scenario.submit(&mollusk, accounts),
        };
        assert_error(&result, expected as u64, label);
        assert_eq!(
            result.resulting_accounts, before,
            "{label}: state untouched"
        );
    }
}
