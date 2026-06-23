//! Shared backfill core for the accountant programs.

#![cfg_attr(any(target_os = "solana", target_arch = "bpf"), no_std)]
#![allow(unexpected_cfgs)]

pub mod instructions;

pub use global_accountant_definitions as definitions;

use pinocchio::error::ProgramError;
use crate::definitions::GlobalAccountantError;

#[inline]
pub fn err(e: GlobalAccountantError) -> ProgramError {
    ProgramError::Custom(e as u32)
}
