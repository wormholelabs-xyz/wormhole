//! Small `AccountInfo` helpers that pinocchio's `AccountView` provided as
//! inherent methods but `solana_program::account_info::AccountInfo` does not:
//! closing an account and checked lamport transfers. Kept in one place so the
//! exact semantics (see [`close_account`]) are pinned once, not re-derived at
//! each call site.

use anchor_lang::prelude::*;

use crate::ProgramResult;

/// Close an account: reassign its owner to the System Program, shrink its
/// data to zero length, and zero its lamports.
///
/// Mirrors pinocchio's `AccountView::close()` exactly (see
/// `solana-account-view` v2.0's doc: "Zero out the account's data length,
/// lamports and owner fields, effectively closing the account"). Any lamports
/// must be moved out *before* calling this (as every caller here already
/// does), since this function itself zeroes them rather than transferring
/// them anywhere.
pub fn close_account(info: &AccountInfo) -> ProgramResult {
    info.assign(&anchor_lang::solana_program::system_program::ID);
    info.resize(0)?;
    **info.try_borrow_mut_lamports()? = 0;
    Ok(())
}

/// `recipient.lamports += amount`, checked. Replaces pinocchio's
/// `set_lamports(lamports() + amount)` call sites.
pub fn add_lamports(info: &AccountInfo, amount: u64) -> ProgramResult {
    let mut lamports = info.try_borrow_mut_lamports()?;
    **lamports = (**lamports)
        .checked_add(amount)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    Ok(())
}
