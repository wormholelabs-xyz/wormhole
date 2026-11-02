//! `BackfillBalance` — write `BalanceAccountLayout` PDAs directly from a
//! wormchain `query_all_accounts` row.
//!
//! Audit chain is transitive: the wormchain snapshot is the source of
//! truth for balances, and every transfer VAA landing on the operational
//! program post-upgrade is independently log-audited via
//! `BackfillNoReplay`'s `ACCDGST\0` entries plus the operational program's
//! own emissions.
//!
//! SECURITY: `require_authority` is the sole authentication for every byte written here. It
//! runs before the parser, so the parser sees operator-supplied data only. `pda::check`
//! re-derives each target address from the entry's own key, so a substituted account fails
//! with `InvalidPda` ahead of the write. `accounts::balance::create` is create-only, so a
//! replayed batch fails at its first entry.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::accounts;
use accountant_operational_core::support::pda;
use accountant_operational_core::{err, ProgramResult};

use crate::definitions::{BalanceAccountLayout, BalanceBatch, GlobalAccountantError};
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
    //   2.. `[WRITE]`         `Balance` PDA, one per entry, in wire order
    let [payer, _system_program, balance_pdas @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    require_authority(payer, expected_authority)?;

    let batch = BalanceBatch::parse(data).map_err(err)?;
    if balance_pdas.len() != batch.entries().len() {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    msg!("BackfillBalance: {} entries", batch.entries().len());

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
