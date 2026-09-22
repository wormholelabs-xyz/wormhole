//! `BackfillModifyBalance` — write `ModifyBalanceLayout` records for the wormchain
//! governance modifications the snapshot's balances already reflect.
//!
//! Arms `modify_balance`'s PDA-existence replay guard for the historical sequences.
//! The snapshot balances already carry each delta, so this handler writes records only.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::support::pda;
use accountant_operational_core::{err, ProgramResult};

use crate::definitions::{
    GlobalAccountantError, ModificationKind, ModifyBalanceBatch, ModifyBalanceLayout,
};
use crate::support::authority::require_authority;

pub fn process(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
    expected_authority: &[u8; 32],
) -> ProgramResult {
    let batch = ModifyBalanceBatch::parse(data).map_err(err)?;

    // Accounts: [WRITE, SIGNER] payer, [] system program (required for
    // `create_pda_allow_prefund`'s CPI), then one `ModifyBalance` record PDA per entry in order.
    let [payer, _system_program, record_pdas @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if record_pdas.len() != batch.entries().len() {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }

    require_authority(payer, expected_authority)?;

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
        pda::create(program_id, payer, record_pda, &record.key(), bump, &record)?;
    }

    Ok(())
}
