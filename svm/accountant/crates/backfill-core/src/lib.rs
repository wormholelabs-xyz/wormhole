//! Shared backfill core for the accountant programs.

#![cfg_attr(any(target_os = "solana", target_arch = "bpf"), no_std)]
#![allow(unexpected_cfgs)]

pub mod instructions;

pub use global_accountant_definitions as definitions;

use pinocchio::error::ProgramError;

/// Custom error codes returned via `ProgramError::Custom(u32)`. Distinct from
/// the operational program's `GlobalAccountantError` numbering so log
/// inspection unambiguously identifies which program raised the error.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackfillError {
    InvalidInstruction = 0,
    InvalidInstructionData = 1,
    InvalidPda = 2,
    /// Signer does not match the program's compile-time `BACKFILL_AUTHORITY`.
    UnauthorizedCaller = 3,
}

impl From<BackfillError> for u32 {
    fn from(e: BackfillError) -> Self {
        e as u32
    }
}

/// Convert a `BackfillError` into a `ProgramError::Custom`.
#[inline]
pub fn err(e: BackfillError) -> ProgramError {
    ProgramError::Custom(e as u32)
}
