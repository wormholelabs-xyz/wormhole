//! Zero-copy load / store / lazy-init helpers for [`BalanceAccountLayout`].
//!
//! Mirrors `state::digest` and `state::pending`. Each `(chain, token_chain,
//! token_address)` triple has a canonical PDA at
//! `(b"account", chain.to_be_bytes(), token_chain.to_be_bytes(), token_address)`
//! — the CosmWasm `Account` record's Solana home. The on-chain layout is
//! `Pod`, so load/store copy by value to release the underlying borrow before
//! the caller mutates anything else.
//!
//! Lazy init (via [`init_or_upgrade`]) lets the destination Account PDA come
//! into existence on the quorum-completing tx — the same payer-pays-rent
//! contract `submit_observations` already uses for the pending PDA and the
//! Digest PDA.

use pinocchio::{
    account::Ref,
    cpi::{Seed, Signer},
    error::ProgramError,
    AccountView, Address, ProgramResult,
};

use crate::definitions::{
    BalanceAccountLayout, GlobalAccountantError, Uint256, ACCOUNT_SEED_PREFIX,
};
use crate::err;
use crate::instructions::pda_init::init_or_upgrade_pda;

/// Read a [`BalanceAccountLayout`] out of an account's data. Returns
/// `InvalidPda` if the buffer is not exactly `LEN` bytes.
pub fn load(account: &AccountView) -> Result<BalanceAccountLayout, ProgramError> {
    let data: Ref<'_, [u8]> = account.try_borrow()?;
    if data.len() != BalanceAccountLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(*bytemuck::from_bytes::<BalanceAccountLayout>(&data))
}

/// Write a [`BalanceAccountLayout`] into an account's data buffer.
pub fn store(account: &mut AccountView, value: &BalanceAccountLayout) -> Result<(), ProgramError> {
    let mut data = account.try_borrow_mut()?;
    if data.len() != BalanceAccountLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    data.copy_from_slice(bytemuck::bytes_of(value));
    Ok(())
}

/// Lazy-init the canonical Account PDA at
/// `(b"account", chain_be, token_chain_be, token_address)`. Idempotent on the
/// already-initialised path: if the PDA already exists and is owned by the
/// program, this is a no-op and the caller's later `load` reads through.
///
/// The caller is responsible for canonical-bump enforcement before calling
/// this helper — see `verify_account_pda` in `submit_observations`.
pub fn init_if_needed(
    program_id: &Address,
    payer: &AccountView,
    account_pda: &mut AccountView,
    chain: u16,
    token_chain: u16,
    token_address: &[u8; 32],
    canonical_bump: u8,
) -> ProgramResult {
    // Already-initialised path: program-owned + full-length data ⇒ no-op.
    // `init_or_upgrade_pda` would reject these accounts as `InvalidPda` (its
    // contract is "system-owned, empty data only"), so we short-circuit here.
    if account_pda.owner() != &pinocchio_system::ID
        && account_pda.data_len() == BalanceAccountLayout::LEN
    {
        return Ok(());
    }

    let chain_be = chain.to_be_bytes();
    let token_chain_be = token_chain.to_be_bytes();
    let bump_seed = [canonical_bump];
    let seeds_with_bump = [
        Seed::from(ACCOUNT_SEED_PREFIX),
        Seed::from(chain_be.as_slice()),
        Seed::from(token_chain_be.as_slice()),
        Seed::from(token_address.as_slice()),
        Seed::from(bump_seed.as_slice()),
    ];
    let signer = Signer::from(&seeds_with_bump);

    init_or_upgrade_pda(
        payer,
        account_pda,
        program_id,
        signer,
        BalanceAccountLayout::LEN as u64,
    )?;

    // Stamp the freshly-zeroed layout with the keying triple so subsequent
    // reads see a self-describing record (matches the CosmWasm `Account.key`
    // field). Balance starts at `Uint256::ZERO`; the caller's `lock_or_burn`
    // / `unlock_or_mint` performs the credit/debit in-place.
    let mut layout: BalanceAccountLayout = bytemuck::Zeroable::zeroed();
    layout.chain = chain;
    layout.token_chain = token_chain;
    layout.token_address = *token_address;
    layout.balance = Uint256::ZERO;
    store(account_pda, &layout)
}
