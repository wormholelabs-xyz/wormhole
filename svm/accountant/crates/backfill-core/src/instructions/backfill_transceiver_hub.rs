//! `BackfillTransceiverHub` — write the `TransceiverHub` PDA for each wormchain
//! `transceiver_to_hub` row, through the same `operational-core` helpers `register_hub` and
//! `register_peer` use, so a backfilled entry is byte-identical to a VAA-written one.
//! Wired by the NTT backfill shell only.
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

use crate::definitions::{GlobalAccountantError, TransceiverHubBatch};
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
    //   2.. `[WRITE]`         `TransceiverHub` PDA, one per entry, in wire order
    let [payer, _system_program, hub_pdas @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    require_authority(payer, expected_authority)?;

    let batch = TransceiverHubBatch::parse(data).map_err(err)?;
    if hub_pdas.len() != batch.entries().len() {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    msg!("BackfillTransceiverHub: {} entries", batch.entries().len());

    for (entry, hub_pda) in batch.entries().iter().zip(hub_pdas) {
        let layout = entry.layout();
        let bump = pda::check(program_id, hub_pda, &layout.key())?;
        pda::create(program_id, payer, hub_pda, &layout.key(), bump, &layout)?;
    }

    Ok(())
}
