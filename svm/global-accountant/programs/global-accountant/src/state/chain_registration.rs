//! Zero-copy load helper for `ChainRegistrationLayout`.
//!
//! Mirrors `state::pending` and `state::digest`. The chain-registration PDA
//! is read-only on the submit paths (`submit_observations`, `submit_vaas`)
//! and only written by the `register_chain` governance instruction, so a
//! `store` companion lives there rather than here.

use pinocchio::{account::Ref, error::ProgramError, AccountView, Address, ProgramResult};

use crate::definitions::{
    ChainRegistrationLayout, GlobalAccountantError, CHAIN_REGISTRATION_SEED_PREFIX,
};
use crate::err;

/// Read a [`ChainRegistrationLayout`] out of an account's data. Returns
/// `MissingChainRegistration` if the supplied account is system-owned (a
/// signal that no `register_chain` governance VAA has landed for this chain)
/// or `InvalidPda` if the buffer length is wrong for our layout.
///
/// Caller is responsible for verifying the account address against the
/// canonical PDA derivation BEFORE calling — this helper checks the on-disk
/// data shape only.
pub fn load(account: &AccountView) -> Result<ChainRegistrationLayout, ProgramError> {
    if account.owner() == &pinocchio_system::ID {
        return Err(err(GlobalAccountantError::MissingChainRegistration));
    }
    let data: Ref<'_, [u8]> = account.try_borrow()?;
    if data.len() != ChainRegistrationLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(*bytemuck::from_bytes::<ChainRegistrationLayout>(&data))
}

/// Write a [`ChainRegistrationLayout`] into the account's data buffer. Used
/// by the `register_chain` governance instruction after the PDA has been
/// allocated (or upgraded in place over a stale registration).
pub fn store(
    account: &mut AccountView,
    value: &ChainRegistrationLayout,
) -> Result<(), ProgramError> {
    let mut data = account.try_borrow_mut()?;
    if data.len() != ChainRegistrationLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    data.copy_from_slice(bytemuck::bytes_of(value));
    Ok(())
}

/// Two-stage chain-registration cross-check used by both `submit_observations`
/// and `submit_vaas` after parsing a body header.
///
/// Mirrors the CosmWasm `CHAIN_REGISTRATIONS` lookup at
/// `cosmwasm/contracts/global-accountant/src/contract.rs:158-166`
/// (observations path) and `:446-454` (VAA backfill path):
///
/// 1. The supplied account must live at the canonical address
///    `(b"chain_registration", body_chain.to_be_bytes())`. Without this
///    check a caller could pass a foreign account masquerading as the
///    registration PDA and route the data-read past `load`'s length check.
/// 2. The on-disk `emitter_address` must equal the body header's emitter.
///    Mirrors CosmWasm's "unknown emitter address" `ensure!` assertion.
///
/// [`load`] returns `MissingChainRegistration` if the supplied account is
/// system-owned (no registration VAA has landed yet); surfaces through the
/// `?` operator as the documented error code.
///
/// Cost: one `find_program_address` (~1.5K CU) + one 64-byte data read +
/// one 32-byte memcmp. Acceptable on the hot path.
pub fn verify(
    program_id: &Address,
    registration_pda: &AccountView,
    body_chain: u16,
    body_emitter: &[u8; 32],
) -> ProgramResult {
    let chain_be = body_chain.to_be_bytes();
    let (expected, _bump) =
        Address::find_program_address(&[CHAIN_REGISTRATION_SEED_PREFIX, &chain_be], program_id);
    if registration_pda.address() != &expected {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let layout = load(registration_pda)?;
    if layout.emitter_address != *body_emitter {
        return Err(err(GlobalAccountantError::UnregisteredEmitter));
    }
    Ok(())
}
