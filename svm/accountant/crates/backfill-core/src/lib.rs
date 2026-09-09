//! Migration-only backfill handlers shared by the accountant backfill programs:
//! `BackfillBalance`, `BackfillModifyBalance`, `BackfillNoReplay`, the bulk
//! NoReplay CPI, and the backfill authority check.
//!
//! Depends on `accountant-operational-core` for accounts, CPI, and support
//! helpers. The backfill programs are its only consumers.

pub mod cpi;
pub mod instructions;
pub mod support;

pub use global_accountant_definitions as definitions;

/// Flatten a `#[derive(Accounts)]` struct plus `ctx.remaining_accounts` into a
/// positional `Vec<AccountInfo>`. Field order must match the handler's account list.
#[macro_export]
macro_rules! flatten_accounts {
    ($ctx:expr, [$($field:ident),+ $(,)?], remaining) => {{
        let mut accounts: ::std::vec::Vec<::anchor_lang::prelude::AccountInfo> =
            vec![$($ctx.accounts.$field.to_account_info()),+];
        accounts.extend($ctx.remaining_accounts.iter().cloned());
        accounts
    }};
}
