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

use crate::accounts;
use crate::definitions::{
    BalanceAccountLayout, BalanceBatch, GlobalAccountantError, ACCOUNT_SEED_PREFIX,
};
use crate::support::authority::require_authority;
use crate::support::pda_init::create_pda_allow_prefund;
use crate::{err, ProgramResult};

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
        let (expected, canonical_bump) = accounts::balance::derive_pda(
            program_id,
            entry.chain(),
            entry.token_chain(),
            &entry.token_address,
        );
        if balance_pda.key != &expected {
            return Err(err(GlobalAccountantError::InvalidPda));
        }

        let bump_seed = [canonical_bump];
        let seeds: &[&[u8]] = &[
            ACCOUNT_SEED_PREFIX,
            entry.chain.as_slice(),
            entry.token_chain.as_slice(),
            entry.token_address.as_slice(),
            &bump_seed,
        ];

        create_pda_allow_prefund(
            payer,
            balance_pda,
            program_id,
            seeds,
            BalanceAccountLayout::LEN as u64,
        )?;

        let layout = BalanceAccountLayout::new(
            entry.chain(),
            entry.token_chain(),
            entry.token_address,
            entry.balance(),
        );

        let mut data_mut = balance_pda.try_borrow_mut_data()?;
        if data_mut.len() != BalanceAccountLayout::LEN {
            return Err(err(GlobalAccountantError::InvalidPda));
        }
        data_mut.copy_from_slice(bytemuck::bytes_of(&layout));
    }

    Ok(())
}
