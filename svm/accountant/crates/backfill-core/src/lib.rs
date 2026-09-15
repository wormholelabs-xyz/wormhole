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
