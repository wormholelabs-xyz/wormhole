//! `BackfillRegisterChainRecord` — write the `RegisterChain` record PDA for each superseded
//! wormchain registration sequence.
//!
//! `register_chain` accepts any sequence without a record and overwrites the registration.
//! `BackfillChainRegistration` records only the installing sequence of each chain, so an
//! earlier `RegisterChain` VAA that wormchain also applied would restore a retired emitter
//! after the cutover. This handler arms the per-sequence replay guard for those sequences and
//! leaves `ChainRegistration` unchanged.
//!
//! SECURITY: `require_authority` is the sole authentication for every byte written here. It
//! runs before the parser, so the parser sees operator-supplied data only. `pda::check`
//! re-derives the record address from the layout's own sequence, so a substituted account
//! fails with `InvalidPda` ahead of the write. `pda::create` is create-only, so a replayed
//! batch fails at its first entry.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::support::pda;
use accountant_operational_core::{err, ProgramResult};

use crate::definitions::{GlobalAccountantError, RegisterChainLayout, RegisterChainRecordBatch};
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
    //   2.. `[WRITE]`         `RegisterChain` record PDA, one per entry, in wire order
    let [payer, _system_program, record_pdas @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    require_authority(payer, expected_authority)?;

    let batch = RegisterChainRecordBatch::parse(data).map_err(err)?;
    if record_pdas.len() != batch.entries().len() {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    msg!(
        "BackfillRegisterChainRecord: {} entries",
        batch.entries().len()
    );

    for (entry, record_pda) in batch.entries().iter().zip(record_pdas) {
        let record = RegisterChainLayout::new(entry.chain(), entry.emitter, entry.sequence());
        let bump = pda::check(program_id, record_pda, &record.key())?;
        pda::create(program_id, payer, record_pda, &record.key(), bump, &record)?;
    }

    Ok(())
}
