//! Wormhole Global Accountant — Solana port (Pinocchio).
//!
//! Scope: digest-account open/close lifecycle. Balance accounting,
//! quorum logic, NoReplay, and Verify VAA Shim CPI live in later slices.
//! See `.claude/tasks/accountant-migration.md`.

#![cfg_attr(target_os = "solana", no_std)]
// `target_os = "solana"` is provided by the SBF toolchain; the host toolchain
// flags it as an unexpected cfg value.
#![allow(unexpected_cfgs)]

pub mod entrypoint;
pub mod instructions;
pub mod state;

pub use global_accountant_definitions as definitions;

use pinocchio::error::ProgramError;

use crate::definitions::GlobalAccountantError;

/// Convert a `GlobalAccountantError` into a `ProgramError::Custom`. Lives here
/// (rather than `impl From`) because the orphan rules forbid the impl: both
/// types are foreign to the program crate.
#[inline]
pub(crate) fn err(e: GlobalAccountantError) -> ProgramError {
    ProgramError::Custom(e as u32)
}
