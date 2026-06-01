//! Zero-copy load helper for `ChainRegistrationLayout`.
//!
//! Mirrors `state::pending` and `state::digest`. The chain-registration PDA
//! is read-only on the submit paths (`submit_observations`, `submit_vaas`)
//! and only written by the `register_chain` governance instruction, so a
//! `store` companion lives there rather than here.

use pinocchio::{account::Ref, error::ProgramError, AccountView};

use crate::definitions::{ChainRegistrationLayout, GlobalAccountantError};
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
