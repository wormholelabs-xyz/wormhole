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
//!
//! Ported from pinocchio to anchor-lang 1.1.2 (see
//! `.claude/tasks/anchor-migration-plan-v1.1.2.md`). Anchor is used purely for
//! framework ergonomics at the `#[program]`/`#[derive(Accounts)]` layer in the
//! `global-accountant` program crate; this crate's own functions are plain
//! Rust taking `&AccountInfo`/`&[u8]`, unaware of `Context`/`Accounts` — the
//! same shape as before, just built on `anchor_lang::solana_program` types
//! instead of pinocchio's.

pub mod account_util;
pub mod hash;
pub mod instructions;
pub mod state;

pub use global_accountant_definitions as definitions;

use anchor_lang::solana_program::program_error::ProgramError;

use crate::definitions::GlobalAccountantError;

/// Mirrors pinocchio's `ProgramResult` alias so call sites are unchanged.
pub type ProgramResult = Result<(), ProgramError>;

/// `Result<T, ProgramError>` for helpers whose success value isn't `()`.
/// Spelled out (rather than a bare `Result<T, E>`) at call sites because
/// `anchor_lang::prelude::*` glob-imports `anchor_lang::Result<T>` — a
/// single-generic-parameter alias for `Result<T, anchor_lang::error::Error>`
/// — which shadows `core::result::Result` wherever that glob is in scope.
pub type ProgramCoreResult<T> = Result<T, ProgramError>;

/// Convert a `GlobalAccountantError` into a `ProgramError::Custom`. A free
/// function rather than a `From` impl because orphan rules forbid the impl
/// (both types are foreign) and `definitions` must stay anchor-free.
#[inline]
pub fn err(e: GlobalAccountantError) -> ProgramError {
    ProgramError::Custom(e as u32)
}
