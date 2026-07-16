//! Shared backfill core for the accountant programs.

#![cfg_attr(any(target_os = "solana", target_arch = "bpf"), no_std)]
#![allow(unexpected_cfgs)]

pub mod instructions;

pub use global_accountant_definitions as definitions;

use pinocchio::error::ProgramError;

use crate::definitions::Pubkey;

/// Pubkey that must sign every backfill ix.
///
/// **CHANGE THIS BEFORE MAINNET DEPLOY.** Default value is the deterministic
/// test keypair derived from `Keypair::new_from_array([1u8; 32])` — present
/// to keep the surfpool e2e suite reproducible. A mainnet `.so` built with
/// this value would let anyone with knowledge of the seed sign backfill
/// txs, which means anyone could write fake balance/noreplay state.
///
/// To replace:
/// 1. Pick the operator keypair (hardware wallet, Squads multisig, etc.)
/// 2. Run `solana-keygen pubkey --keypair <path>` and convert base58 → 32
///    bytes (one-liner: `python3 -c "import base58; print(list(base58.b58decode('<base58>')))"`)
/// 3. Paste the byte array here
/// 4. `cargo build-sbf --features bpf-entrypoint` produces the deploy-ready `.so`
pub const BACKFILL_AUTHORITY: Pubkey = [
    // === REPLACE BEFORE MAINNET ===
    // Test default: pubkey of Keypair::new_from_array([1u8; 32]).
    // Verified by `tests/backfill_noreplay.rs::backfill_authority_const_matches_test_keypair`.
    0x8a, 0x88, 0xe3, 0xdd, 0x74, 0x09, 0xf1, 0x95, 0xfd, 0x52, 0xdb, 0x2d, 0x3c, 0xba, 0x5d, 0x72,
    0xca, 0x67, 0x09, 0xbf, 0x1d, 0x94, 0x12, 0x1b, 0xf3, 0x74, 0x88, 0x01, 0xb4, 0x0f, 0x6f, 0x5c,
    // ===============================
];

/// Custom error codes returned via `ProgramError::Custom(u32)`. Distinct from
/// the operational program's `GlobalAccountantError` numbering so log
/// inspection unambiguously identifies which program raised the error.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackfillError {
    InvalidInstruction = 0,
    InvalidInstructionData = 1,
    InvalidPda = 2,
    /// Signer does not match the compile-time [`BACKFILL_AUTHORITY`] pubkey.
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
