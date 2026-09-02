//! `BackfillModifyBalance` — write `ModifyBalanceLayout` records for the wormchain
//! governance modifications the snapshot's balances already reflect.
//!
//! Arms `modify_balance`'s PDA-existence replay guard for the historical sequences.
//! Do not apply any balance delta here: the snapshot balances already carry it.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use crate::accounts;
use crate::definitions::{
    GlobalAccountantError, ModificationKind, ModifyBalanceBatch, ModifyBalanceLayout,
    MODIFY_BALANCE_SEED_PREFIX,
};
use crate::support::authority::require_authority;
use crate::support::pda_init::init_or_upgrade_pda;
use crate::{err, ProgramResult};

pub fn process(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
    expected_authority: &[u8; 32],
) -> ProgramResult {
    let batch = ModifyBalanceBatch::parse(data).map_err(err)?;

    // Accounts: [WRITE, SIGNER] payer, [] system program (required for
    // `init_or_upgrade_pda`'s CPI), then one `ModifyBalance` record PDA per entry in order.
    let [payer, _system_program, record_pdas @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if record_pdas.len() != batch.entries().len() {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }

    require_authority(payer, expected_authority)?;

    for (entry, record_pda) in batch.entries().iter().zip(record_pdas) {
        let kind = ModificationKind::from_u8(entry.kind)
            .ok_or(err(GlobalAccountantError::InvalidModificationKind))?;

        let (expected, canonical_bump) =
            accounts::modify_balance::derive_pda(program_id, entry.sequence());
        if record_pda.key != &expected {
            return Err(err(GlobalAccountantError::InvalidPda));
        }

        let bump_seed = [canonical_bump];
        let seeds: &[&[u8]] = &[MODIFY_BALANCE_SEED_PREFIX, &entry.sequence, &bump_seed];

        init_or_upgrade_pda(
            payer,
            record_pda,
            program_id,
            seeds,
            ModifyBalanceLayout::LEN as u64,
        )?;

        let record = ModifyBalanceLayout::new(
            kind,
            entry.chain_id(),
            entry.token_chain(),
            entry.sequence(),
            entry.token_address,
            entry.amount(),
            entry.reason,
        );

        // Record only. Do not touch a balance account here.
        let mut data_mut = record_pda.try_borrow_mut_data()?;
        if data_mut.len() != ModifyBalanceLayout::LEN {
            return Err(err(GlobalAccountantError::InvalidPda));
        }
        data_mut.copy_from_slice(bytemuck::bytes_of(&record));
    }

    Ok(())
}
