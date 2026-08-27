//! Load / store helpers for `PendingObservationsLayout`. Load copies by value.

use anchor_lang::prelude::*;

use crate::definitions::{GlobalAccountantError, PendingObservationsLayout};
use crate::err;

/// `InvalidPda` if the length or tag is wrong.
pub fn load(account: &AccountInfo) -> crate::ProgramCoreResult<PendingObservationsLayout> {
    let data = account.try_borrow_data()?;
    if data.len() != PendingObservationsLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let layout = bytemuck::from_bytes::<PendingObservationsLayout>(&data);
    if layout.tag != PendingObservationsLayout::TAG {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(*layout)
}

pub fn store(account: &AccountInfo, value: &PendingObservationsLayout) -> crate::ProgramResult {
    let mut data = account.try_borrow_mut_data()?;
    if data.len() != PendingObservationsLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    data.copy_from_slice(bytemuck::bytes_of(value));
    Ok(())
}
