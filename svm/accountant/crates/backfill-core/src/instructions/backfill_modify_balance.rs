//! `BackfillModifyBalance` — write `ModifyBalanceLayout` records for the wormchain
//! governance modifications the snapshot's balances already reflect.
//!
//! Arms `modify_balance`'s PDA-existence replay guard for the historical sequences.
//! The snapshot balances already carry each delta, so this handler writes records only.
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

use crate::definitions::{
    GlobalAccountantError, ModificationKind, ModifyBalanceBatch, ModifyBalanceLayout,
};
use crate::support::authority::require_authority;

/// Order: account framing, authority, wire parse, PDA count, then per entry kind check, PDA
/// check and create.
pub fn process(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
    expected_authority: &[u8; 32],
) -> ProgramResult {
    // Accounts:
    //   0.  `[WRITE, SIGNER]` payer
    //   1.  `[]`              system program (`create_pda_allow_prefund`'s CPI target)
    //   2.. `[WRITE]`         `ModifyBalance` record PDA, one per entry, in wire order
    let [payer, _system_program, record_pdas @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    require_authority(payer, expected_authority)?;

    let batch = ModifyBalanceBatch::parse(data).map_err(err)?;
    if record_pdas.len() != batch.entries().len() {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    msg!("BackfillModifyBalance: {} entries", batch.entries().len());

    for (entry, record_pda) in batch.entries().iter().zip(record_pdas) {
        let kind = ModificationKind::from_u8(entry.kind)
            .ok_or_else(|| err(GlobalAccountantError::InvalidModificationKind))?;

        let record = ModifyBalanceLayout::new(
            kind,
            entry.chain_id(),
            entry.token_chain(),
            entry.sequence(),
            entry.token_address,
            entry.amount(),
            entry.reason,
        );

        let bump = pda::check(program_id, record_pda, &record.key())?;
        // Record only. Do not touch a balance account here.
        pda::create(program_id, payer, record_pda, &record.key(), bump, &record)?;
    }

    Ok(())
}
