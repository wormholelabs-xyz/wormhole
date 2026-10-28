//! `BackfillChainRegistration` — write the `ChainRegistration` PDA and the `RegisterChain`
//! record PDA for each wormchain chain registration row.
//!
//! Both PDAs are the accounts the operational `register_chain` writes, created through the
//! same `operational-core` helpers, so a backfilled registration is byte-identical to a
//! governance-written one. The record PDA arms `register_chain`'s per-sequence replay
//! guard, so the installing VAA cannot apply a second time after the cutover.
//!
//! SECURITY: `require_authority` is the sole authentication for every byte written here. It
//! runs before the parser, so the parser sees operator-supplied data only. `pda::check`
//! re-derives both target addresses from the layouts' own keys, so a substituted account
//! fails with `InvalidPda` ahead of either write. `pda::create` is create-only, so a
//! replayed batch fails at its first entry.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::support::pda;
use accountant_operational_core::{err, ProgramResult};

use crate::definitions::{
    ChainRegistrationBatch, ChainRegistrationLayout, GlobalAccountantError, RegisterChainLayout,
};
use crate::support::authority::require_authority;

/// Order: account framing, authority, wire parse, PDA count, then per entry both PDA checks
/// and both creates.
pub fn process(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
    expected_authority: &[u8; 32],
) -> ProgramResult {
    // Accounts:
    //   0.  `[WRITE, SIGNER]` payer
    //   1.  `[]`              system program (`create_pda_allow_prefund`'s CPI target)
    //   2.. `[WRITE]`         per entry in wire order: `ChainRegistration` PDA, then
    //                         `RegisterChain` record PDA
    let [payer, _system_program, pdas @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    require_authority(payer, expected_authority)?;

    let batch = ChainRegistrationBatch::parse(data).map_err(err)?;
    if pdas.len() != 2 * batch.entries().len() {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    msg!(
        "BackfillChainRegistration: {} entries",
        batch.entries().len()
    );

    for (entry, pair) in batch.entries().iter().zip(pdas.chunks_exact(2)) {
        let (registration_pda, record_pda) = (&pair[0], &pair[1]);
        let (chain, sequence, emitter) = (entry.chain(), entry.sequence(), entry.emitter);

        let registration = ChainRegistrationLayout::new(chain, emitter, sequence);
        let record = RegisterChainLayout::new(chain, emitter, sequence);

        let registration_bump = pda::check(program_id, registration_pda, &registration.key())?;
        let record_bump = pda::check(program_id, record_pda, &record.key())?;

        pda::create(
            program_id,
            payer,
            registration_pda,
            &registration.key(),
            registration_bump,
            &registration,
        )?;
        pda::create(
            program_id,
            payer,
            record_pda,
            &record.key(),
            record_bump,
            &record,
        )?;
    }

    Ok(())
}
