use anchor_lang::prelude::*;

use crate::definitions::MODIFY_BALANCE_SEED_PREFIX;

/// `ModifyBalance` record PDA `(address, bump)` for a governance modification sequence.
/// Its existence is the replay guard, so the backfill and the governance handler must
/// derive the same address.
pub fn derive_pda(program_id: &Pubkey, sequence: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[MODIFY_BALANCE_SEED_PREFIX, &sequence.to_be_bytes()],
        program_id,
    )
}
