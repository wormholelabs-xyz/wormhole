//! Balance PDA helpers: derivation and lazy creation.

use anchor_lang::prelude::*;

use crate::definitions::{BalanceAccountLayout, BalanceKey, GlobalAccountantError, Uint256};
use crate::support::pda;
use crate::{err, ProgramResult};

/// Balance PDA `(address, bump)` for `(chain, token_chain, token_address)`.
pub fn derive_pda(
    program_id: &Pubkey,
    chain: u16,
    token_chain: u16,
    token_address: &[u8; 32],
) -> (Pubkey, u8) {
    pda::derive(
        program_id,
        &BalanceKey::new(chain, token_chain, *token_address),
    )
}

/// Create a zero balance when the PDA is absent; an initialised PDA must have the layout's
/// exact length.
pub fn init_if_needed<'info>(
    program_id: &Pubkey,
    payer: &AccountInfo<'info>,
    account_pda: &AccountInfo<'info>,
    chain: u16,
    token_chain: u16,
    token_address: &[u8; 32],
    canonical_bump: u8,
) -> ProgramResult {
    if pda::is_initialised(program_id, account_pda)? {
        if account_pda.data_len() != BalanceAccountLayout::LEN {
            return Err(err(GlobalAccountantError::InvalidPda));
        }
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
    pda::create(
        program_id,
        payer,
        account_pda,
        &layout.key(),
        canonical_bump,
        layout,
    )
}
