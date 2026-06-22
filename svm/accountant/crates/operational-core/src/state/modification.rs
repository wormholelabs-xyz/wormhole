//! Zero-copy store helper for `ModificationLayout`, written by
//! `modify_balance`. Existence of this PDA enforces governance-path replay
//! protection. No production path reads it back, so there is no `load`.

use pinocchio::{error::ProgramError, AccountView};

use crate::definitions::{GlobalAccountantError, ModificationLayout};
use crate::err;

/// Write a [`ModificationLayout`] into the account's data buffer. Caller
/// must verify the canonical address and pre-allocate to `LEN` bytes.
pub fn store(account: &mut AccountView, value: &ModificationLayout) -> Result<(), ProgramError> {
    let mut data = account.try_borrow_mut()?;
    if data.len() != ModificationLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    data.copy_from_slice(bytemuck::bytes_of(value));
    Ok(())
}
