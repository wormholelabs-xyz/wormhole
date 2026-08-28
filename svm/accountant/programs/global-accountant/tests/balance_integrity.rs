use global_accountant::instructions::transfer::derive_balance_account_pda;
use global_accountant_definitions::{GlobalAccountantError, Uint256};

mod common;
use common::*;

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
