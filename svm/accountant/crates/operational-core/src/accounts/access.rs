use anchor_lang::prelude::*;

use crate::definitions::{AccountLayout, GlobalAccountantError};
use crate::{err, ProgramCoreResult, ProgramResult};

pub fn load<L: AccountLayout>(account: &AccountInfo) -> ProgramCoreResult<L> {
    let data = account.try_borrow_data()?;
    let layout: &L =
        bytemuck::try_from_bytes(&data).map_err(|_| err(GlobalAccountantError::InvalidPda))?;
    if layout.tag() != L::TAG {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(*layout)
}

pub fn store<L: AccountLayout>(account: &AccountInfo, value: &L) -> ProgramResult {
    let mut data = account.try_borrow_mut_data()?;
    if data.len() != L::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    data.copy_from_slice(bytemuck::bytes_of(value));
    Ok(())
}
