//! Load / store / lazy-init for [`BalanceAccountLayout`] at
//! `(b"account", chain_be, token_chain_be, token_address)`. Load copies by value.

use anchor_lang::prelude::*;

use crate::definitions::{
    BalanceAccountLayout, GlobalAccountantError, Uint256, ACCOUNT_SEED_PREFIX,
};
use crate::err;
use crate::instructions::pda_init::init_or_upgrade_pda;
use crate::ProgramResult;

/// `InvalidPda` if the length or tag is wrong.
pub fn load(account: &AccountInfo) -> crate::ProgramCoreResult<BalanceAccountLayout> {
    let data = account.try_borrow_data()?;
    if data.len() != BalanceAccountLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let layout = bytemuck::from_bytes::<BalanceAccountLayout>(&data);
    if layout.tag != BalanceAccountLayout::TAG {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(*layout)
}

pub fn store(account: &AccountInfo, value: &BalanceAccountLayout) -> crate::ProgramResult {
    let mut data = account.try_borrow_mut_data()?;
    if data.len() != BalanceAccountLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    data.copy_from_slice(bytemuck::bytes_of(value));
    Ok(())
}

/// Create the balance PDA if absent. Caller checks `canonical_bump` first.
pub fn init_if_needed<'info>(
    program_id: &Pubkey,
    payer: &AccountInfo<'info>,
    account_pda: &AccountInfo<'info>,
    chain: u16,
    token_chain: u16,
    token_address: &[u8; 32],
    canonical_bump: u8,
) -> ProgramResult {
    if account_pda.owner != &anchor_lang::solana_program::system_program::ID
        && account_pda.data_len() == BalanceAccountLayout::LEN
    {
        return Ok(());
    }

    let layout = BalanceAccountLayout::new(chain, token_chain, *token_address, Uint256::ZERO);
    create(program_id, payer, account_pda, canonical_bump, &layout)
}

/// Allocate the balance PDA for `layout`'s key and write `layout`. Seeds derive from the
/// layout so address and contents cannot disagree.
pub fn create<'info>(
    program_id: &Pubkey,
    payer: &AccountInfo<'info>,
    account_pda: &AccountInfo<'info>,
    canonical_bump: u8,
    layout: &BalanceAccountLayout,
) -> ProgramResult {
    let chain_be = layout.chain.to_be_bytes();
    let token_chain_be = layout.token_chain.to_be_bytes();
    let bump_seed = [canonical_bump];
    let seeds: &[&[u8]] = &[
        ACCOUNT_SEED_PREFIX,
        &chain_be,
        &token_chain_be,
        &layout.token_address,
        &bump_seed,
    ];
    init_or_upgrade_pda(
        payer,
        account_pda,
        program_id,
        seeds,
        BalanceAccountLayout::LEN as u64,
    )?;
    store(account_pda, layout)
}
