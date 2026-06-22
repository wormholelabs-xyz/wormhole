//! Wormhole NTT Global Accountant — backfill program (Pinocchio).
//!
//! Temporary one-shot migration `.so`. Reuses `accountant-backfill-core`'s
//! `BackfillNoReplay` / `BackfillBalance` (program-ID-agnostic, so they seed the
//! NTT program's NoReplay bits and Balance PDAs unchanged) and adds three
//! NTT-native instructions that seed the relayer-registration, transceiver-hub,
//! and transceiver-peer maps. Deployed under the NTT program ID, then replaced
//! in-place via `solana program upgrade` once the operational program is ready.
//!
//! ## Authority model
//!
//! Every backfill ix requires the [`BACKFILL_AUTHORITY`] pubkey as the tx
//! signer — its own operator key, distinct from the WTT backfill program's.
//! Same one-shot, single-operator rationale as the WTT program (see
//! `accountant-backfill-core`).

#![cfg_attr(any(target_os = "solana", target_arch = "bpf"), no_std)]
#![allow(unexpected_cfgs)]

pub mod entrypoint;
pub mod instructions;

pub use accountant_backfill_core::BackfillError;
pub use global_accountant_definitions as definitions;

use crate::definitions::Pubkey;

/// Pubkey that must sign every backfill ix.
///
/// **CHANGE THIS BEFORE MAINNET DEPLOY.** Default value is the deterministic
/// test keypair derived from `Keypair::new_from_array([1u8; 32])` — present to
/// keep the mollusk/surfpool suites reproducible. A mainnet `.so` built with
/// this value would let anyone with knowledge of the seed write fake state.
/// Replace with the NTT migration operator's pubkey before `cargo build-sbf`.
pub const BACKFILL_AUTHORITY: Pubkey = [
    // === REPLACE BEFORE MAINNET ===
    // Test default: pubkey of Keypair::new_from_array([1u8; 32]).
    0x8a, 0x88, 0xe3, 0xdd, 0x74, 0x09, 0xf1, 0x95, 0xfd, 0x52, 0xdb, 0x2d, 0x3c, 0xba, 0x5d, 0x72,
    0xca, 0x67, 0x09, 0xbf, 0x1d, 0x94, 0x12, 0x1b, 0xf3, 0x74, 0x88, 0x01, 0xb4, 0x0f, 0x6f,
    0x5c,
    // ===============================
];

/// Instruction discriminators. Single-byte prefix on instruction data.
/// `0`/`1` dispatch into `accountant-backfill-core`; `2`/`3`/`4` are the
/// NTT-native map handlers in [`crate::instructions`].
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Instruction {
    BackfillNoReplay = 0,
    BackfillBalance = 1,
    BackfillRelayerRegistration = 2,
    BackfillTransceiverHub = 3,
    BackfillTransceiverPeer = 4,
}

impl Instruction {
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::BackfillNoReplay),
            1 => Some(Self::BackfillBalance),
            2 => Some(Self::BackfillRelayerRegistration),
            3 => Some(Self::BackfillTransceiverHub),
            4 => Some(Self::BackfillTransceiverPeer),
            _ => None,
        }
    }
}
