//! Wormhole Global Accountant — backfill program (Pinocchio).
//!
//! Temporary one-shot migration `.so` that seeds `solana-noreplay` bits and
//! `BalanceAccountLayout` PDAs from a deterministic wormchain snapshot, then is
//! replaced in-place via `solana program upgrade` once the operational program
//! is ready. Trust is moved off the hot path into a public audit primitive: every
//! `BackfillNoReplay` entry emits the canonical 86-byte `ACCDGST\0` commit-log
//! payload, so any third party with a Solana ledger archive and a VAA archive
//! can recompute `keccak256(keccak256(body))` and verify the operator did not
//! lie about which sequences were observed by wormchain.

#![cfg_attr(any(target_os = "solana", target_arch = "bpf"), no_std)]
#![allow(unexpected_cfgs)]

pub mod entrypoint;
pub mod instructions;
pub mod state;

pub use global_accountant_definitions as definitions;

use pinocchio::error::ProgramError;

/// Instruction discriminators. Single-byte prefix on instruction data. Stable
/// across the (brief) lifetime of this program.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Instruction {
    BackfillNoReplay = 0,
    BackfillBalance = 1,
    Retire = 2,
}

impl Instruction {
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::BackfillNoReplay),
            1 => Some(Self::BackfillBalance),
            2 => Some(Self::Retire),
            _ => None,
        }
    }
}

/// Custom error codes returned via `ProgramError::Custom(u32)`. Distinct from
/// the operational program's `GlobalAccountantError` numbering so log
/// inspection unambiguously identifies which program raised the error.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackfillError {
    InvalidInstruction = 0,
    InvalidInstructionData = 1,
    InvalidPda = 2,
    /// Signer does not match the pubkey recorded in the backfill authority PDA.
    AuthorityMismatch = 3,
    /// Authority has been retired via the `Retire` ix; further calls reject.
    AuthorityRetired = 4,
    /// `solana-noreplay` `MarkUsed` CPI returned an error.
    NoReplayCpiFailed = 5,
}

impl From<BackfillError> for u32 {
    fn from(e: BackfillError) -> Self {
        e as u32
    }
}

/// Convert a `BackfillError` into a `ProgramError::Custom`. Free function rather
/// than a `From` impl: orphan rules forbid the impl across foreign types.
#[inline]
pub(crate) fn err(e: BackfillError) -> ProgramError {
    ProgramError::Custom(e as u32)
}
