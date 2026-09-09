//! The `.so` executes only at its `declare_id!` address. Anchor's entry returns
//! `DeclaredProgramIdMismatch` for any other program id before dispatch.

use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

use crate::common::*;

/// `anchor_lang::error::ErrorCode::DeclaredProgramIdMismatch`.
const DECLARED_PROGRAM_ID_MISMATCH: u64 = 4100;

#[test]
fn rejects_execution_under_another_program_id() {
    let foreign_id: Pubkey = Pubkey::new_unique();
    assert_ne!(foreign_id, program_id());
    let mollusk = mollusk_with_fixtures(&foreign_id, PROGRAM_NAME);

    let scenario = ObsScenario::attest(GUARDIAN_COUNT, GUARDIAN_SET_INDEX, 0x50);
    let ix =
        Instruction::new_with_bytes(foreign_id, &scenario.ix_data(0), scenario.account_metas());
    let result = mollusk.process_instruction(&ix, &scenario.initial_accounts());
    assert_error(&result, DECLARED_PROGRAM_ID_MISMATCH, "foreign program id");
}
