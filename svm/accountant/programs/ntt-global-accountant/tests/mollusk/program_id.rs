//! The `.so` executes only at its `declare_id!` address. Anchor's entry returns
//! `DeclaredProgramIdMismatch` for any other program id before dispatch.

use anchor_lang::error::ErrorCode;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

use crate::common::*;

#[test]
fn rejects_execution_under_another_program_id() {
    let foreign_id = Pubkey::new_from_array([8u8; 32]);
    assert_ne!(foreign_id, program_id());
    let mollusk = mollusk_with_fixtures(&foreign_id, PROGRAM_NAME);

    let vaa = SignedVaa::new(vec![0u8; 51]);
    let ix = Instruction::new_with_bytes(
        foreign_id,
        &register_relayer_chain_ix_data(vaa.guardian_set_bump, &vaa.body),
        vaa.shim_metas(),
    );
    let result = mollusk.process_instruction(&ix, &vaa.shim_accounts());
    assert_error(
        &result,
        ErrorCode::DeclaredProgramIdMismatch as u64,
        "foreign program id",
    );
}
