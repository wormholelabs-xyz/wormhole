//! `BackfillBalance` — write `BalanceAccountLayout` PDAs directly from a
//! wormchain `query_all_accounts` row.
//!
//! Audit chain is transitive: the wormchain snapshot is the source of
//! truth for balances, and every transfer VAA landing on the operational
//! program post-upgrade is independently log-audited via
//! `BackfillNoReplay`'s `ACCDGST\0` entries plus the operational program's
//! own emissions.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::accounts;
use accountant_operational_core::support::pda;
use accountant_operational_core::{err, ProgramResult};

use crate::definitions::{BalanceAccountLayout, BalanceBatch, GlobalAccountantError};
use crate::support::authority::require_authority;

pub fn process(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
    expected_authority: &[u8; 32],
) -> ProgramResult {
    let batch = BalanceBatch::parse(data).map_err(err)?;

    // Accounts: [WRITE, SIGNER] payer, [] system program (required for
    // `create_pda_allow_prefund`'s CPI), then one balance PDA per entry in order.
    let [payer, _system_program, balance_pdas @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if balance_pdas.len() != batch.entries().len() {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }

    require_authority(payer, expected_authority)?;

    for (entry, balance_pda) in batch.entries().iter().zip(balance_pdas) {
        let layout = BalanceAccountLayout::new(
            entry.chain(),
            entry.token_chain(),
            entry.token_address,
            entry.balance(),
        );

        let bump = pda::check(program_id, balance_pda, &layout.key())?;
        accounts::balance::create(program_id, payer, balance_pda, bump, &layout)?;
    }

    Ok(())
}
