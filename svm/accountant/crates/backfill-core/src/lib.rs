//! Migration-only backfill handlers shared by the accountant backfill programs:
//! `BackfillNoReplay`, `BackfillBalance`, `BackfillModifyBalance`,
//! `BackfillChainRegistration`, the bulk NoReplay CPI, and the backfill authority check.
//!
//! Layering: `definitions <- operational-core <- backfill-core <- backfill program shells`.
//! Every PDA write goes through `operational-core`, so backfilled bytes equal the bytes the
//! operational program writes. Do not add this crate to an operational program's dependencies;
//! its handlers write state without governance proof.

pub mod cpi;
pub mod instructions;
pub mod support;

pub use global_accountant_definitions as definitions;
