use anchor_lang::prelude::*;

use crate::definitions::{RegisterChainLayout, REGISTER_CHAIN_SEED_PREFIX};
use crate::support::pda_init::create_pda_allow_prefund;
use crate::ProgramResult;

/// `RegisterChain` record PDA `(address, bump)` for a governance VAA sequence. Its
/// existence is the replay guard, so the backfill and the governance handler must derive
/// the same address.
pub fn derive_pda(program_id: &Pubkey, sequence: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[REGISTER_CHAIN_SEED_PREFIX, &sequence.to_be_bytes()],
        program_id,
    )
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
    let sequence_be = layout.sequence.to_be_bytes();
    let bump_seed = [canonical_bump];
    let seeds: &[&[u8]] = &[REGISTER_CHAIN_SEED_PREFIX, &sequence_be, &bump_seed];
    create_pda_allow_prefund(
        payer,
        record_pda,
        program_id,
        seeds,
        RegisterChainLayout::LEN as u64,
    )?;
    super::store(record_pda, layout)
}
