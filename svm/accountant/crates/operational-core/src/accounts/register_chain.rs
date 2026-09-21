use anchor_lang::prelude::*;

use crate::definitions::{RegisterChainKey, RegisterChainLayout};
use crate::support::pda;
use crate::ProgramResult;

/// `RegisterChain` record PDA `(address, bump)` for a governance VAA sequence. Its
/// existence is the replay guard, so the backfill and the governance handler must derive
/// the same address.
pub fn derive_pda(program_id: &Pubkey, sequence: u64) -> (Pubkey, u8) {
    pda::derive(program_id, &RegisterChainKey::new(sequence))
}

/// Allocate the `RegisterChain` record PDA for `layout`'s sequence and write `layout`.
/// Seeds derive from the layout so address and contents cannot disagree. The governance
/// handler and the backfill both call this, so the record they produce is byte-identical.
pub fn create<'info>(
    program_id: &Pubkey,
    payer: &AccountInfo<'info>,
    record_pda: &AccountInfo<'info>,
    canonical_bump: u8,
    layout: &RegisterChainLayout,
) -> ProgramResult {
    pda::create(
        program_id,
        payer,
        record_pda,
        &layout.key(),
        canonical_bump,
        layout,
    )
}
