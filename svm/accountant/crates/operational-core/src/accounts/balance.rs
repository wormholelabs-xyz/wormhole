use anchor_lang::prelude::*;

use crate::definitions::{BalanceAccountLayout, Uint256, ACCOUNT_SEED_PREFIX};
use crate::support::pda_init::create_pda_allow_prefund;
use crate::ProgramResult;

/// Balance PDA `(address, bump)` for `(chain, token_chain, token_address)`.
pub fn derive_pda(
    program_id: &Pubkey,
    chain: u16,
    token_chain: u16,
    token_address: &[u8; 32],
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            ACCOUNT_SEED_PREFIX,
            &chain.to_be_bytes(),
            &token_chain.to_be_bytes(),
            token_address,
        ],
        program_id,
    )
}

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
    create_pda_allow_prefund(
        payer,
        account_pda,
        program_id,
        seeds,
        BalanceAccountLayout::LEN as u64,
    )?;
    super::store(account_pda, layout)
}
