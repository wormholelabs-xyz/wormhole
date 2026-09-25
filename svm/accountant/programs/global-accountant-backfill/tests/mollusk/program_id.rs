//! The `.so` executes only at its `declare_id!` address. Anchor's entry returns
//! `DeclaredProgramIdMismatch` for any other program id before dispatch.

use accountant_operational_core::accounts::balance;
use anchor_lang::error::ErrorCode;
use global_accountant_definitions::global_accountant_backfill::Instruction;
use global_accountant_definitions::Uint256;
use mollusk_svm::program::keyed_account_for_system_program;
use solana_instruction::{AccountMeta, Instruction as SolanaInstruction};
use solana_pubkey::Pubkey;

use crate::common::*;

#[test]
fn rejects_execution_under_another_program_id() {
    let foreign_id = Pubkey::new_from_array([8u8; 32]);
    assert_ne!(foreign_id, program_id());
    let mollusk = mollusk_with_fixtures(&foreign_id, PROGRAM_NAME);

    let entry = wire::balance_entry(2, 2, [0x11u8; 32], Uint256::from_u128(1).0);
    let (pda, _) = balance::derive_pda(&foreign_id, 2, 2, &[0x11u8; 32]);
    let (system_program, system_account) = keyed_account_for_system_program();
    let payer = test_authority_pubkey();

    let ix = SolanaInstruction::new_with_bytes(
        foreign_id,
        &wire::encode_balance_batch(Instruction::BackfillBalance as u8, &[entry]),
        vec![
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(system_program, false),
            AccountMeta::new(pda, false),
        ],
    );
    let accounts = vec![
        (payer, system_owned_account(10_000_000_000)),
        (system_program, system_account),
        (pda, uninitialised_pda_account()),
    ];
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_error(
        &result,
        ErrorCode::DeclaredProgramIdMismatch as u64,
        "foreign program id",
    );
}
