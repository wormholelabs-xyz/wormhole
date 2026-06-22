//! Wormhole Global Accountant — backfill program (Pinocchio).
//!
//! Temporary one-shot migration `.so` that seeds `solana-noreplay` bits and
//! `BalanceAccountLayout` PDAs from a deterministic wormchain snapshot, then
//! is replaced in-place via `solana program upgrade` once the operational
//! program is ready.
//!
//! ## Authority model
//!
//! Every backfill ix requires the [`BACKFILL_AUTHORITY`] pubkey as the
//! transaction signer. The check is a constant-time `memcmp` against the
//! compile-time constant — no on-chain authority PDA, no lazy-init, no
//! kill-switch. Rationale:
//!
//! - The program is one-shot. Exactly one mainnet deployment, one operator.
//! - After `solana program upgrade` replaces the bytecode, the constant
//!   is gone — no orphaned PDA to clean up post-migration.
//! - The "retired" semantic is achieved implicitly: the upgraded operational
//!   program doesn't have these instructions at all, so any leftover caller
//!   gets `InvalidInstruction`.
//!
//! Operators MUST replace [`BACKFILL_AUTHORITY`] with their deployer's
//! pubkey before `cargo build-sbf` for a real deployment. The default value
//! is the test-suite keypair derived from secret seed `[1u8; 32]` — useful
//! for the surfpool e2e suite, NEVER for mainnet.
//!
//! ## Trust model
//!
//! The program does NOT verify VAA signatures. The audit chain is lazily
//! verifiable by any third party with the Solana ledger archive and a VAA
//! archive — see the master plan `accountant-migration-backfill.md`. Every
//! `BackfillNoReplay` entry emits a canonical `ACCDGST\0` commit-log; that
//! is the audit primitive.

#![cfg_attr(any(target_os = "solana", target_arch = "bpf"), no_std)]
#![allow(unexpected_cfgs)]

pub mod entrypoint;

pub use accountant_backfill_core::BackfillError;
pub use global_accountant_definitions as definitions;

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
    0xca, 0x67, 0x09, 0xbf, 0x1d, 0x94, 0x12, 0x1b, 0xf3, 0x74, 0x88, 0x01, 0xb4, 0x0f, 0x6f,
    0x5c,
    // ===============================
];

/// Instruction discriminators. Single-byte prefix on instruction data.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Instruction {
    BackfillNoReplay = 0,
    BackfillBalance = 1,
}

impl Instruction {
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::BackfillNoReplay),
            1 => Some(Self::BackfillBalance),
            _ => None,
        }
    }
}
