//! `AccountInfo` helpers: account close and checked lamport credit.

use anchor_lang::prelude::*;

use crate::ProgramResult;

/// Assign the account to the System Program, resize to 0, and zero its lamports.
/// Move lamports out before the call; this function discards them.
pub fn close_account(info: &AccountInfo) -> ProgramResult {
    info.assign(&anchor_lang::solana_program::system_program::ID);
    info.resize(0)?;
    **info.try_borrow_mut_lamports()? = 0;
    Ok(())
}

/// `info.lamports += amount`, checked.
pub fn add_lamports(info: &AccountInfo, amount: u64) -> ProgramResult {
    let mut lamports = info.try_borrow_mut_lamports()?;
    **lamports = (**lamports)
        .checked_add(amount)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    Ok(())
}
