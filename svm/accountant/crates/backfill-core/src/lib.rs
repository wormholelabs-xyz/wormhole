//! Shared backfill core for the accountant migration programs.
//!
//! The `BackfillNoReplay` and `BackfillBalance` handlers seed `solana-noreplay`
//! bits and `BalanceAccountLayout` PDAs from a deterministic wormchain snapshot.
//! Both are **program-ID-agnostic** (all PDAs derive from the runtime
//! `program_id`) and **authority-parameterised** (the caller passes the
//! operator pubkey that must sign), so the same logic backs both the
//! `global-accountant` (WTT) and `ntt-global-accountant` backfill programs —
//! each deployed under its own program ID with its own `BACKFILL_AUTHORITY`.
//!
//! These programs do NOT verify VAA signatures. The audit chain is lazily
//! verifiable from the Solana ledger archive + a VAA archive; every
//! `BackfillNoReplay` entry emits a canonical `ACCDGST\0` commit-log — that is
//! the audit primitive. See the master plan `accountant-migration-backfill.md`.

#![cfg_attr(any(target_os = "solana", target_arch = "bpf"), no_std)]
#![allow(unexpected_cfgs)]

pub mod authority;
pub mod backfill_balance;
pub mod backfill_noreplay;
pub(crate) mod commit_log;
pub(crate) mod noreplay;
pub mod pda_init;

pub use global_accountant_definitions as definitions;

use pinocchio::error::ProgramError;

/// Custom error codes returned via `ProgramError::Custom(u32)`. Distinct from
/// the operational program's `GlobalAccountantError` numbering so log
/// inspection unambiguously identifies which program raised the error. Shared
/// across every backfill program built on this core.
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

/// Convert a `BackfillError` into a `ProgramError::Custom`. Free function
/// rather than a `From` impl: orphan rules forbid the impl across foreign
/// types.
#[inline]
pub fn err(e: BackfillError) -> ProgramError {
    ProgramError::Custom(e as u32)
}
