//! `BackfillTransceiverHub` — write the `TransceiverHub` PDA for each wormchain
//! `transceiver_to_hub` row, through the same `operational-core` helpers `register_hub` and
//! `register_peer` use, so a backfilled entry is byte-identical to a VAA-written one.
//! Wired by the NTT backfill shell only.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::support::pda;
use accountant_operational_core::{err, ProgramResult};

use crate::definitions::{GlobalAccountantError, TransceiverHubBatch};
use crate::support::authority::require_authority;

pub fn process(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
    expected_authority: &[u8; 32],
) -> ProgramResult {
    let batch = TransceiverHubBatch::parse(data).map_err(err)?;

    // Accounts: [WRITE, SIGNER] payer, [] system program (required for
    // `create_pda_allow_prefund`'s CPI), then one `TransceiverHub` PDA per entry in wire
    // order.
    let [payer, _system_program, pdas @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if pdas.len() != batch.entries().len() {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }

    require_authority(payer, expected_authority)?;

    for (entry, pda_account) in batch.entries().iter().zip(pdas) {
        let layout = entry.layout();
        let bump = pda::check(program_id, pda_account, &layout.key())?;
        pda::create(program_id, payer, pda_account, &layout.key(), bump, &layout)?;
    }

    Ok(())
}
