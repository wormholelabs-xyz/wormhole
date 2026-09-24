//! Token Bridge balance mutation, shared by `submit_observations` and `submit_vaas`.

use anchor_lang::prelude::*;

use crate::definitions::{
    parse_token_bridge_payload, BalanceAccountLayout, GlobalAccountantError, TokenBridgeAction,
    Uint256,
};
use crate::err;
use accountant_operational_core::accounts::{self, balance};
use accountant_operational_core::ProgramResult;

/// Source `lock_or_burn`, then destination `unlock_or_mint`. Both PDAs are derived from the
/// payload and checked before any write. A same-chain transfer applies both operations to
/// one in-memory layout.
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
        derive_balance_account_pda(program_id, source_chain, token_chain, token_address);
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
        derive_balance_account_pda(program_id, recipient_chain, token_chain, token_address);
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

/// Parse the Token Bridge payload and apply it. Attest is a no-op; an unknown action
/// fails the transaction, which rolls back the NoReplay mark.
pub fn apply_from_body<'info>(
    program_id: &Pubkey,
    submitter: &AccountInfo<'info>,
    source_account_pda: &AccountInfo<'info>,
    dest_account_pda: &AccountInfo<'info>,
    source_chain: u16,
    body_bytes: &[u8],
) -> ProgramResult {
    match parse_token_bridge_payload(body_bytes).map_err(err)? {
        TokenBridgeAction::Transfer {
            amount,
            token_chain,
            token_address,
            recipient_chain,
        } => apply_transfer(
            program_id,
            submitter,
            source_account_pda,
            dest_account_pda,
            source_chain,
            recipient_chain,
            token_chain,
            &token_address,
            amount,
        ),
        TokenBridgeAction::Attest => Ok(()),
        TokenBridgeAction::Other(_) => Err(err(GlobalAccountantError::UnknownTokenBridgePayload)),
    }
}

pub use accountant_operational_core::accounts::balance::derive_pda as derive_balance_account_pda;
