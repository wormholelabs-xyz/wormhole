//! Shared operational core for the accountant programs: quorum tracking, NoReplay
//! and Shim CPIs, PDA init, commit log, hashing, and state helpers.
//!
//! All PDAs derive from the runtime `program_id`, so the WTT and NTT programs share
//! this code under separate program IDs. Each program supplies the token-payload
//! parser and balance mutation as an `apply` callback.
//!
//! Functions take `&AccountInfo` / `&[u8]`; Anchor `Context` types stay in the program crates.

pub mod account_util;
pub mod hash;
pub mod instructions;
pub mod state;

pub use global_accountant_definitions as definitions;

use anchor_lang::solana_program::program_error::ProgramError;

use crate::definitions::GlobalAccountantError;

pub type ProgramResult = Result<(), ProgramError>;

/// `Result<T, ProgramError>`. Use where `anchor_lang::prelude::*` shadows `core::result::Result`.
pub type ProgramCoreResult<T> = Result<T, ProgramError>;

/// `GlobalAccountantError` to `ProgramError::Custom`. Orphan rules prevent a `From` impl.
#[inline]
pub fn err(e: GlobalAccountantError) -> ProgramError {
    ProgramError::Custom(e as u32)
}
