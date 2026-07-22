//! Balance-mutation helpers shared by the quorum-completing branch of
//! `submit_observations` and the signed-VAA backfill in `submit_vaas`.

use anchor_lang::prelude::*;

use crate::definitions::{
    parse_token_bridge_payload, GlobalAccountantError, TokenBridgeAction, Uint256,
    ACCOUNT_SEED_PREFIX,
};
use crate::err;
use accountant_operational_core::state::account as account_state;
use accountant_operational_core::ProgramResult;

/// Mutate the source and destination balance account PDAs for a Token Bridge transfer:
/// source-side `lock_or_burn`, then destination-side `unlock_or_mint`.
/// Same-chain self-transfers (source == dest PDA) are collapsed onto one
/// in-memory layout so the second mutation observes the first.
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
    // ----- Source side -----
    let (src_expected, src_bump) =
        derive_balance_account_pda(program_id, source_chain, token_chain, token_address);
    if source_account.key != &src_expected {
        return Err(err(GlobalAccountantError::InvalidAccountPda));
    }
    account_state::init_if_needed(
        program_id,
        payer,
        source_account,
        source_chain,
        token_chain,
        token_address,
        src_bump,
    )?;
    let mut src = account_state::load(source_account)?;
    src.lock_or_burn(amount).map_err(err)?;

    // Same-chain self-transfer collapse: apply both ops to one in-memory layout
    // in order. This pins the transient arithmetic (burn-then-mint must underflow
    // when the wrapped balance is below `amount`, even though the net is zero) and
    // guards against a lost-update if this function is ever refactored to a
    // load-both-then-mutate shape.
    let same_pda = source_account.key == dest_account.key;
    if same_pda {
        src.unlock_or_mint(amount).map_err(err)?;
        account_state::store(source_account, &src)?;
        return Ok(());
    }

    // Distinct destination: flush source, then operate on the destination.
    account_state::store(source_account, &src)?;

    // ----- Destination side -----
    let (dst_expected, dst_bump) =
        derive_balance_account_pda(program_id, recipient_chain, token_chain, token_address);
    if dest_account.key != &dst_expected {
        return Err(err(GlobalAccountantError::InvalidAccountPda));
    }
    account_state::init_if_needed(
        program_id,
        payer,
        dest_account,
        recipient_chain,
        token_chain,
        token_address,
        dst_bump,
    )?;
    let mut dst = account_state::load(dest_account)?;
    dst.unlock_or_mint(amount).map_err(err)?;
    account_state::store(dest_account, &dst)
}

/// Parse Token Bridge payload from the verified VAA body and apply balance mutations.
/// Called from both `submit_observations` (quorum-completing branch) and `submit_vaas`
/// (signed-VAA backfill path). Attest payloads are no-ops; unknown payloads are rejected
/// so the NoReplay mark can roll back with the tx.
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
        TokenBridgeAction::Attest => {
            // No balance work; the source / dest slots are untouched.
            Ok(())
        }
        TokenBridgeAction::Other => Err(err(GlobalAccountantError::UnknownTokenBridgePayload)),
    }
}

/// Re-derive the canonical balance account PDA address + bump from `(chain,
/// token_chain, token_address)`.
pub fn derive_balance_account_pda(
    program_id: &Pubkey,
    chain: u16,
    token_chain: u16,
    token_address: &[u8; 32],
) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    let token_chain_be = token_chain.to_be_bytes();
    Pubkey::find_program_address(
        &[
            ACCOUNT_SEED_PREFIX,
            &chain_be,
            &token_chain_be,
            token_address,
        ],
        program_id,
    )
}
