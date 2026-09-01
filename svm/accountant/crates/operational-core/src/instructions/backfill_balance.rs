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

use crate::definitions::{
    BalanceAccountLayout, BalanceBatch, GlobalAccountantError, ACCOUNT_SEED_PREFIX,
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
    let batch = BalanceBatch::parse(data).map_err(err)?;

    // Accounts: [WRITE, SIGNER] payer, [] system program (required for
    // `init_or_upgrade_pda`'s CPI), then one balance PDA per entry in order.
    let [payer, _system_program, balance_pdas @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if balance_pdas.len() != batch.entries().len() {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }

    require_authority(payer, expected_authority)?;

    for (entry, balance_pda) in batch.entries().iter().zip(balance_pdas) {
        // `find_program_address` over `create_program_address`+bump: caller is fully trusted.
        let (expected, canonical_bump) = Pubkey::find_program_address(
            &[
                ACCOUNT_SEED_PREFIX,
                entry.chain.as_slice(),
                entry.token_chain.as_slice(),
                entry.token_address.as_slice(),
            ],
            program_id,
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

        init_or_upgrade_pda(
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
