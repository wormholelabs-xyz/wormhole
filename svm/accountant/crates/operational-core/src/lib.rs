//! Shared operational core for the accountant programs.
//!
//! Hosts the product-neutral machinery behind the operational instruction set:
//! the [`submit_observations`] quorum tracker, the [`submit_vaas`] signed-VAA
//! backfill, and [`close_pending`] cleanup, plus the supporting NoReplay / Shim
//! CPI helpers, PDA-init helper, commit-log emit, keccak hashing, and the
//! zero-copy state layouts.
//!
//! All PDAs derive from the runtime `program_id`, so the same logic backs both
//! the WTT (`global-accountant`) and NTT operational programs — each deployed
//! under its own program ID.
//!
//! The one product-specific seam — parsing the verified VAA body's token
//! payload and mutating balances — is injected by the consuming program. Both
//! [`submit_observations::process`] and [`submit_vaas::process`] take an `apply`
//! callback invoked at the seam, after all quorum / signature / NoReplay /
//! commit-log machinery has run. The token-payload parser and the
//! balance-mutation logic stay in the consuming program.

#![cfg_attr(any(target_os = "solana", target_arch = "bpf"), no_std)]
#![allow(unexpected_cfgs)]

pub mod hash;
pub mod instructions;
pub mod state;

pub use global_accountant_definitions as definitions;

use pinocchio::error::ProgramError;

use crate::definitions::GlobalAccountantError;

/// Convert a `GlobalAccountantError` into a `ProgramError::Custom`. A free
/// function rather than a `From` impl because orphan rules forbid the impl
/// (both types are foreign) and `definitions` must stay pinocchio-free.
#[inline]
pub fn err(e: GlobalAccountantError) -> ProgramError {
    ProgramError::Custom(e as u32)
}
