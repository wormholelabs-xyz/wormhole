//! Zero-copy load/store helpers for `PendingObservationsLayout`. The layout is
//! `Pod`, so load/store copy by value to release the borrow before mutating.

use anchor_lang::prelude::*;

use crate::definitions::{GlobalAccountantError, PendingObservationsLayout};
use crate::err;

/// Read a [`PendingObservationsLayout`]. `InvalidPda` if the buffer is not `LEN`.
pub fn load(account: &AccountInfo) -> crate::ProgramCoreResult<PendingObservationsLayout> {
    let data = account.try_borrow_data()?;
    if data.len() != PendingObservationsLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let layout = bytemuck::from_bytes::<PendingObservationsLayout>(&data);
    // Defense-in-depth: offset-0 tag must match (tag 0 = uninitialised).
    if layout.tag != PendingObservationsLayout::TAG {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(*layout)
}

/// Write a [`PendingObservationsLayout`] into an account's data buffer.
pub fn store(
    account: &AccountInfo,
    value: &PendingObservationsLayout,
) -> crate::ProgramResult {
    let mut data = account.try_borrow_mut_data()?;
    if data.len() != PendingObservationsLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    data.copy_from_slice(bytemuck::bytes_of(value));
    Ok(())
}
