//! Balance movement shared by every path that applies a transfer: WTT with the Token Bridge
//! token identity, NTT with the hub token identity.

use anchor_lang::prelude::*;

use crate::accounts::{self, balance};
use crate::definitions::{BalanceAccountLayout, GlobalAccountantError, Uint256};
use crate::{err, ProgramResult};

/// Source `lock_or_burn`, then destination `unlock_or_mint`. Both PDAs are derived from
/// `(chain, token_chain, token_address)` and checked before any write. A same-chain
/// transfer applies both operations to one in-memory layout.
///
/// The caller supplies the accounted token identity: the Token Bridge token for WTT, the
/// hub token for NTT.
#[allow(clippy::too_many_arguments)]
pub fn apply_transfer<'info>(
    program_id: &Pubkey,
    payer: &AccountInfo<'info>,
    source_account: &AccountInfo<'info>,
    dest_account: &AccountInfo<'info>,
    source_chain: u16,
    recipient_chain: u16,
    token_chain: u16,
    token_address: &[u8; 32],
    amount: Uint256,
) -> ProgramResult {
    let (src_expected, src_bump) =
        balance::derive_pda(program_id, source_chain, token_chain, token_address);
    if source_account.key != &src_expected {
        return Err(err(GlobalAccountantError::InvalidAccountPda));
    }
    balance::init_if_needed(
        program_id,
        payer,
        source_account,
        source_chain,
        token_chain,
        token_address,
        src_bump,
    )?;
    let mut src = accounts::load::<BalanceAccountLayout>(source_account)?;
    src.lock_or_burn(amount).map_err(err)?;

    // Destination PDA is derived from the payload; the check runs on every path.
    let (dst_expected, dst_bump) =
        balance::derive_pda(program_id, recipient_chain, token_chain, token_address);
    if dest_account.key != &dst_expected {
        return Err(err(GlobalAccountantError::InvalidAccountPda));
    }

    // Same chain: both PDAs are one account. Burn-then-mint must still underflow when the
    // balance is below `amount`, so apply both to one in-memory layout.
    if source_chain == recipient_chain {
        if source_account.key != dest_account.key {
            return Err(err(GlobalAccountantError::InvalidAccountPda));
        }
        src.unlock_or_mint(amount).map_err(err)?;
        accounts::store(source_account, &src)?;
        return Ok(());
    }
    if source_account.key == dest_account.key {
        return Err(err(GlobalAccountantError::InvalidAccountPda));
    }

    accounts::store(source_account, &src)?;

    balance::init_if_needed(
        program_id,
        payer,
        dest_account,
        recipient_chain,
        token_chain,
        token_address,
        dst_bump,
    )?;
    let mut dst = accounts::load::<BalanceAccountLayout>(dest_account)?;
    dst.unlock_or_mint(amount).map_err(err)?;
    accounts::store(dest_account, &dst)
}
