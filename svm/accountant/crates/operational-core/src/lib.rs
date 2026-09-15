//! Shared operational core for the accountant programs: quorum tracking, NoReplay
//! and Shim CPIs, PDA init, commit log, hashing, and state helpers.
//!
//! All PDAs derive from the runtime `program_id`, so the WTT and NTT programs share
//! this code under separate program IDs. Each program supplies the token-payload
//! parser and balance mutation as an `apply` callback.
//!
//! Functions take `&AccountInfo` / `&[u8]`; Anchor `Context` types stay in the program crates.
//! The one exception is `flatten_accounts!`: a syntactic bridge from a program crate's
//! `Context` into that `&[AccountInfo]` shape, shared because both programs need the
//! identical mechanical translation and one needs it with `remaining_accounts` appended.

pub mod account_util;
pub mod accounts;
pub mod cpi;
pub mod hash;
pub mod instructions;
pub mod raw_ix_data;
pub mod support;

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

/// Flatten a `Context`'s `#[derive(Accounts)]` fields, and optionally
/// `ctx.remaining_accounts`, into the positional `Vec<AccountInfo>` handlers take.
/// Field order must match the handler's account list. See the module-level doc:
/// the one Anchor-`Context`-aware item in this crate.
#[macro_export]
macro_rules! flatten_accounts {
    ($ctx:expr, [$($field:ident),+ $(,)?]) => {
        vec![$($ctx.accounts.$field.to_account_info()),+]
    };
    ($ctx:expr, [$($field:ident),+ $(,)?], remaining) => {{
        let mut accounts: ::std::vec::Vec<::anchor_lang::prelude::AccountInfo> =
            vec![$($ctx.accounts.$field.to_account_info()),+];
        accounts.extend($ctx.remaining_accounts.iter().cloned());
        accounts
    }};
}
