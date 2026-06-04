use pinocchio::{account::Ref, error::ProgramError, AccountView};

use crate::definitions::{DigestAccountLayout, GlobalAccountantError};
use crate::err;

/// Read a `DigestAccountLayout`. The layout is `Pod`, so copy out by value to
/// release the borrow before the caller mutates.
pub fn load(account: &AccountView) -> Result<DigestAccountLayout, ProgramError> {
    let data: Ref<'_, [u8]> = account.try_borrow()?;
    if data.len() != DigestAccountLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(*bytemuck::from_bytes::<DigestAccountLayout>(&data))
}

/// Write a `DigestAccountLayout` into an account's data buffer.
pub fn store(account: &mut AccountView, value: &DigestAccountLayout) -> Result<(), ProgramError> {
    let mut data = account.try_borrow_mut()?;
    if data.len() != DigestAccountLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    data.copy_from_slice(bytemuck::bytes_of(value));
    Ok(())
}
