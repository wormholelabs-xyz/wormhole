use pinocchio::{account::Ref, error::ProgramError, AccountView};

use crate::definitions::{DigestAccountLayout, GlobalAccountantError};
use crate::err;

/// Read a `DigestAccountLayout` out of an account's data. The layout is `Pod`,
/// so the cheapest correct thing is to copy it out by value — that releases
/// the underlying borrow before the caller mutates anything else.
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
