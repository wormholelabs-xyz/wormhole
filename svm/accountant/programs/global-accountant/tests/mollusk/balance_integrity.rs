use global_accountant::instructions::transfer::derive_balance_account_pda;
use global_accountant_definitions::{GlobalAccountantError, Uint256};

use crate::common::*;

#[test]
fn dest_failure_rolls_back_source() {
    let mollusk = mollusk();
    type Setup = fn(&mut VaaScenario) -> Option<Uint256>;
    let cases: [(&str, Transfer, Setup, GlobalAccountantError); 2] = [
        (
            "dest overflow",
            Transfer::new(0xB2, ETHEREUM, SOLANA, 500),
            |_| Some(Uint256::MAX),
            GlobalAccountantError::BalanceOverflow,
        ),
        (
            "wrong dest pda",
            Transfer::new(0xB3, ETHEREUM, SOLANA, 500),
            |s| {
                s.dest_account =
                    derive_balance_account_pda(&program_id(), SOLANA, 9, &TOKEN_ADDRESS).0;
                None
            },
            GlobalAccountantError::InvalidAccountPda,
        ),
    ];
    for (label, transfer, setup, expected) in cases {
        let source_initial = Uint256::from_u128(1_000);
        let mut scenario = VaaScenario::transfer(transfer);
        let dest_prefund = setup(&mut scenario);
        let mut accounts = scenario.accounts();
        replace_account(
            &mut accounts,
            &scenario.source_account,
            balance_account(
                transfer.chain,
                transfer.token_chain,
                transfer.token_address,
                source_initial,
            ),
        );
        if let Some(prefund) = dest_prefund {
            replace_account(
                &mut accounts,
                &scenario.dest_account,
                balance_account(
                    transfer.recipient_chain,
                    transfer.token_chain,
                    transfer.token_address,
                    prefund,
                ),
            );
        }
        assert_ne!(scenario.source_account, scenario.dest_account, "{label}");
        let result = scenario.submit(&mollusk, accounts);
        assert_error(&result, expected as u64, label);
        assert_eq!(
            balance_of(find_account(
                &result.resulting_accounts,
                &scenario.source_account
            )),
            source_initial,
            "{label}: source unchanged"
        );
        assert_bucket_unmarked(find_account(
            &result.resulting_accounts,
            &scenario.noreplay_bucket,
        ));
    }
}

/// Source PDA in both balance slots. Legitimate only when `recipient_chain == chain`;
/// a cross-chain transfer must derive the destination from `recipient_chain` and reject.
#[test]
fn same_account_in_both_slots() {
    let mollusk = mollusk();
    let funded = Some(Uint256::from_u128(1_000));
    type Expected = Result<Uint256, GlobalAccountantError>;
    let cases: [(&str, Transfer, Option<Uint256>, Expected); 4] = [
        (
            "same-chain wrapped, funded: burn then mint nets to zero",
            Transfer::new(0xB4, SOLANA, SOLANA, 500),
            funded,
            Ok(Uint256::from_u128(1_000)),
        ),
        (
            "same-chain wrapped, unfunded: burn underflows",
            Transfer::new(0xB5, SOLANA, SOLANA, 500),
            None,
            Err(GlobalAccountantError::BalanceUnderflow),
        ),
        (
            "cross-chain wrapped source: source pda in dest slot",
            Transfer::new(0xB6, SOLANA, ETHEREUM, 500),
            funded,
            Err(GlobalAccountantError::InvalidAccountPda),
        ),
        (
            "cross-chain native source: source pda in dest slot",
            Transfer::new(0xB7, ETHEREUM, SOLANA, 500),
            None,
            Err(GlobalAccountantError::InvalidAccountPda),
        ),
    ];
    for (label, transfer, prefund, expected) in cases {
        let mut scenario = VaaScenario::transfer(transfer);
        scenario.dest_account = scenario.source_account;
        let mut accounts = scenario.accounts();
        if let Some(balance) = prefund {
            replace_account(
                &mut accounts,
                &scenario.source_account,
                balance_account(
                    transfer.chain,
                    transfer.token_chain,
                    transfer.token_address,
                    balance,
                ),
            );
        }
        let source_before = find_account(&accounts, &scenario.source_account).clone();
        let result = scenario.submit(&mollusk, accounts);
        let source_after = find_account(&result.resulting_accounts, &scenario.source_account);
        let bucket = find_account(&result.resulting_accounts, &scenario.noreplay_bucket);
        match expected {
            Ok(balance) => {
                assert_success(&result, label);
                assert_eq!(balance_of(source_after), balance, "{label}: balance");
                assert_bucket_marked(bucket, scenario.sequence);
            }
            Err(code) => {
                assert_error(&result, code as u64, label);
                assert_eq!(*source_after, source_before, "{label}: source untouched");
                assert_bucket_unmarked(bucket);
            }
        }
    }
}
