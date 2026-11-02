//! `BackfillTransceiverPeer` — write the `TransceiverPeer` PDA for each wormchain
//! `transceiver_peers` row, through the same `operational-core` helpers `register_peer` uses,
//! so a backfilled entry is byte-identical to a VAA-written one. Wired by the NTT backfill
//! shell only.
//!
//! SECURITY: `require_authority` is the sole authentication for every byte written here. It
//! runs before the parser, so the parser sees operator-supplied data only. `pda::check`
//! re-derives the target address from the layout's own key, so a substituted account fails
//! with `InvalidPda` ahead of the write. `pda::create` is create-only, so a replayed batch
//! fails at its first entry.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::support::pda;
use accountant_operational_core::{err, ProgramResult};

use crate::definitions::{GlobalAccountantError, TransceiverPeerBatch};
use crate::support::authority::require_authority;

/// Order: account framing, authority, wire parse, PDA count, then per entry PDA check and
/// create.
pub fn process(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
    expected_authority: &[u8; 32],
) -> ProgramResult {
    // Accounts:
    //   0.  `[WRITE, SIGNER]` payer
    //   1.  `[]`              system program (`create_pda_allow_prefund`'s CPI target)
    //   2.. `[WRITE]`         `TransceiverPeer` PDA, one per entry, in wire order
    let [payer, _system_program, peer_pdas @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    require_authority(payer, expected_authority)?;

    let batch = TransceiverPeerBatch::parse(data).map_err(err)?;
    if peer_pdas.len() != batch.entries().len() {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    msg!("BackfillTransceiverPeer: {} entries", batch.entries().len());

    for (entry, peer_pda) in batch.entries().iter().zip(peer_pdas) {
        let layout = entry.layout();
        let bump = pda::check(program_id, peer_pda, &layout.key())?;
        pda::create(program_id, payer, peer_pda, &layout.key(), bump, &layout)?;
    }

    Ok(())
}
