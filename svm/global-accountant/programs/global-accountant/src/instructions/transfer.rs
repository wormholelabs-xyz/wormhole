//! Balance-mutation helpers shared between the quorum-completing branch of
//! `submit_observations` and the signed-VAA backfill path in `submit_vaas`.
//!
//! Both callers need the same routine — the CosmWasm reference
//! (`handle_tokenbridge_vaa`) drives `accountant::commit_transfer` for both
//! the observation-quorum path and the signed-VAA path. Living in a sibling
//! module keeps both callers honest (same canonical-bump enforcement, same
//! same-PDA-collapse, same overflow semantics) without forcing one instruction
//! module to depend on another's private items.

use pinocchio::{AccountView, Address, ProgramResult};

use crate::definitions::{GlobalAccountantError, Uint256, ACCOUNT_SEED_PREFIX};
use crate::err;
use crate::state::account as account_state;

/// Mutate the source and destination Account PDAs to reflect a Token Bridge
/// transfer. Port of CosmWasm `commit_transfer`
/// (`cosmwasm/packages/accountant/src/contract.rs:109-126`):
///
/// 1. Source-side `lock_or_burn` — chain == token_chain ⇒ credit (native
///    lock), chain != token_chain ⇒ debit (wrapped burn).
/// 2. Destination-side `unlock_or_mint` — chain == token_chain ⇒ debit
///    (native unlock), chain != token_chain ⇒ credit (wrapped mint).
///
/// Same-chain self-transfers (source == destination PDA) are collapsed onto
/// one in-memory layout so the second mutation observes the first — matching
/// CosmWasm's `if src.key == dst.key { src.unlock_or_mint(...) }` path.
#[allow(clippy::too_many_arguments)]
pub fn apply_transfer(
    program_id: &Address,
    payer: &AccountView,
    source_account: &mut AccountView,
    dest_account: &mut AccountView,
    source_chain: u16,
    recipient_chain: u16,
    token_chain: u16,
    token_address: &[u8; 32],
    amount: Uint256,
) -> ProgramResult {
    // ----- Source side -----
    let (src_expected, src_bump) = derive_account_pda(
        program_id,
        source_chain,
        token_chain,
        token_address,
    );
    if source_account.address() != &src_expected {
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

    // Same-chain self-transfer detection — see CosmWasm
    // `cosmwasm/packages/accountant/src/contract.rs:158-161`. When source ==
    // destination, apply both ops to the same in-memory layout before storing
    // once, so the second op observes the first.
    let same_pda = source_account.address() == dest_account.address();
    if same_pda {
        src.unlock_or_mint(amount).map_err(err)?;
        account_state::store(source_account, &src)?;
        return Ok(());
    }

    // Distinct destination: flush source, then operate on the destination.
    account_state::store(source_account, &src)?;

    // ----- Destination side -----
    let (dst_expected, dst_bump) = derive_account_pda(
        program_id,
        recipient_chain,
        token_chain,
        token_address,
    );
    if dest_account.address() != &dst_expected {
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

/// Re-derive the canonical Account PDA address + bump from `(chain,
/// token_chain, token_address)`. Mirrors the canonical-bump pattern used in
/// `open_digest_inner` and `close_pending`'s pending-PDA check.
pub fn derive_account_pda(
    program_id: &Address,
    chain: u16,
    token_chain: u16,
    token_address: &[u8; 32],
) -> (Address, u8) {
    let chain_be = chain.to_be_bytes();
    let token_chain_be = token_chain.to_be_bytes();
    Address::find_program_address(
        &[ACCOUNT_SEED_PREFIX, &chain_be, &token_chain_be, token_address],
        program_id,
    )
}
