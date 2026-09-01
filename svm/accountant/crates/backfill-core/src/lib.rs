//! Shared backfill core for the accountant programs.
//!
//! Anchor is used only for the `#[program]`/`#[derive(Accounts)]` layer in
//! the consuming crate (`global-accountant-backfill`). Functions here take
//! plain `&AccountInfo`/`&[u8]` + explicit params. Lets the WTT backfill
//! program (this branch) and a future NTT variant share this crate.

#![allow(unexpected_cfgs)]

pub mod instructions;

pub use global_accountant_definitions as definitions;

use anchor_lang::solana_program::program_error::ProgramError;

use crate::definitions::Pubkey;

pub type ProgramResult = Result<(), ProgramError>;
pub type ProgramCoreResult<T> = Result<T, ProgramError>;

/// Pubkey that must sign every backfill ix.
///
/// **CHANGE THIS BEFORE MAINNET DEPLOY.** Default is the deterministic test
/// keypair from `Keypair::new_from_array([1u8; 32])`, kept for surfpool e2e
/// reproducibility. Deploying to mainnet with this value lets anyone who
/// knows the seed sign backfill txs and write fake balance/noreplay state.
///
/// To replace:
/// 1. Pick the operator keypair (hardware wallet, Squads multisig, etc.)
/// 2. Run `solana-keygen pubkey --keypair <path>` and convert base58 → 32
///    bytes (one-liner: `python3 -c "import base58; print(list(base58.b58decode('<base58>')))"`)
/// 3. Paste the byte array here
/// 4. `anchor build` (or `cargo build-sbf`) produces the deploy-ready `.so`
pub const BACKFILL_AUTHORITY: Pubkey = [
    // === REPLACE BEFORE MAINNET ===
    // Test default: pubkey of Keypair::new_from_array([1u8; 32]).
    // Verified by `tests/backfill_noreplay.rs::backfill_authority_const_matches_test_keypair`.
    0x8a, 0x88, 0xe3, 0xdd, 0x74, 0x09, 0xf1, 0x95, 0xfd, 0x52, 0xdb, 0x2d, 0x3c, 0xba, 0x5d, 0x72,
    0xca, 0x67, 0x09, 0xbf, 0x1d, 0x94, 0x12, 0x1b, 0xf3, 0x74, 0x88, 0x01, 0xb4, 0x0f, 0x6f, 0x5c,
    // ===============================
];

/// Custom error codes via `ProgramError::Custom(u32)`. Numbered separately
/// from the operational program's `GlobalAccountantError` so logs identify
/// which program raised the error. Plain `ProgramError::Custom`, not
/// Anchor's `#[error_code]` (which adds anchor's own `+6000` offset) —
/// keeps the numeric ABI stable.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackfillError {
    InvalidInstruction = 0,
    InvalidInstructionData = 1,
    InvalidPda = 2,
    /// Signer does not match the compile-time [`BACKFILL_AUTHORITY`] pubkey.
    UnauthorizedCaller = 3,
}

/// Convert a `BackfillError` into a `ProgramError::Custom`.
#[inline]
pub fn err(e: BackfillError) -> ProgramError {
    ProgramError::Custom(e as u32)
}
