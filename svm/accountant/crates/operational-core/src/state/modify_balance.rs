//! Store helper for `ModifyBalanceLayout`.

use anchor_lang::prelude::*;

use crate::definitions::{GlobalAccountantError, ModifyBalanceLayout};
use crate::err;

/// Caller checks the address and allocates `LEN` bytes first.
pub fn store(account: &AccountInfo, value: &ModifyBalanceLayout) -> crate::ProgramResult {
    let mut data = account.try_borrow_mut_data()?;
    if data.len() != ModifyBalanceLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    data.copy_from_slice(bytemuck::bytes_of(value));
    Ok(())
}
