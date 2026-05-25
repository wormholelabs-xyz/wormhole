//! Zero-copy load/store helpers for `PendingObservationsLayout`.
//!
//! Mirrors `state::digest` — both layouts are `Pod`, so the cheapest correct
//! thing is to copy out by value, drop the borrow, then mutate.

use pinocchio::{account::Ref, error::ProgramError, AccountView};

use crate::definitions::{GlobalAccountantError, PendingObservationsLayout};
use crate::err;

/// Read a [`PendingObservationsLayout`] out of an account's data. Returns
/// `InvalidPda` if the buffer is not exactly `LEN` bytes.
pub fn load(account: &AccountView) -> Result<PendingObservationsLayout, ProgramError> {
    let data: Ref<'_, [u8]> = account.try_borrow()?;
    if data.len() != PendingObservationsLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(*bytemuck::from_bytes::<PendingObservationsLayout>(&data))
}

/// Write a [`PendingObservationsLayout`] into an account's data buffer.
pub fn store(
    account: &mut AccountView,
    value: &PendingObservationsLayout,
) -> Result<(), ProgramError> {
    let mut data = account.try_borrow_mut()?;
    if data.len() != PendingObservationsLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    data.copy_from_slice(bytemuck::bytes_of(value));
    Ok(())
}
