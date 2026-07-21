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
/// test keypair derived from `Keypair::new_from_array([2u8; 32])` — present to
/// keep the mollusk/surfpool suites reproducible. A mainnet `.so` built with
/// this value would let anyone with knowledge of the seed write fake state.
/// Replace with the NTT migration operator's pubkey before `cargo build-sbf`.
///
/// Deliberately distinct from WTT's own `BACKFILL_AUTHORITY` test default
/// (seed `[1u8; 32]`) so the two programs' operator keys aren't
/// interchangeable — see `cross_program_authority_rejected` in
/// `tests/backfill_ntt_maps.rs`.
pub const BACKFILL_AUTHORITY: Pubkey = [
    // === REPLACE BEFORE MAINNET ===
    // Test default: pubkey of Keypair::new_from_array([2u8; 32]).
    0x81, 0x39, 0x77, 0x0e, 0xa8, 0x7d, 0x17, 0x5f, 0x56, 0xa3, 0x54, 0x66, 0xc3, 0x4c, 0x7e, 0xcc,
    0xcb, 0x8d, 0x8a, 0x91, 0xb4, 0xee, 0x37, 0xa2, 0x5d, 0xf6, 0x0f, 0x5b, 0x8f, 0xc9, 0xb3, 0x94,
    // ===============================
];

/// Fixed reference copy of [`BACKFILL_AUTHORITY`]'s current test-placeholder
/// byte value, so the `mainnet`-feature guard below has something fixed to
/// compare the (eventually operator-replaced) const against.
#[allow(dead_code)]
const TEST_PLACEHOLDER_AUTHORITY: Pubkey = [
    0x81, 0x39, 0x77, 0x0e, 0xa8, 0x7d, 0x17, 0x5f, 0x56, 0xa3, 0x54, 0x66, 0xc3, 0x4c, 0x7e, 0xcc,
    0xcb, 0x8d, 0x8a, 0x91, 0xb4, 0xee, 0x37, 0xa2, 0x5d, 0xf6, 0x0f, 0x5b, 0x8f, 0xc9, 0xb3, 0x94,
];

/// `const fn` byte-equality for 32-byte arrays; a plain indexing loop works
/// in `const` context without depending on `PartialEq::eq` being usable there.
#[allow(dead_code)]
const fn pubkey_eq(a: &Pubkey, b: &Pubkey) -> bool {
    let mut i = 0;
    while i < 32 {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Deploy-time safeguard: building with `--features mainnet` fails the build
/// if [`BACKFILL_AUTHORITY`] is still the compiled-in test placeholder.
///
/// ```text
/// cargo build-sbf --features bpf-entrypoint,mainnet
/// ```
#[cfg(feature = "mainnet")]
const _: () = assert!(
    !pubkey_eq(&BACKFILL_AUTHORITY, &TEST_PLACEHOLDER_AUTHORITY),
    "BACKFILL_AUTHORITY is still the compiled-in test placeholder (seed [2u8; 32]) — \
     replace it with the real NTT migration operator pubkey before building with --features mainnet"
);

#[cfg(all(test, not(any(target_os = "solana", target_arch = "bpf"))))]
mod mainnet_guard_tests {
    use super::*;

    /// Sanity: BACKFILL_AUTHORITY still equals the placeholder today.
    #[test]
    fn backfill_authority_is_currently_the_placeholder() {
        assert!(pubkey_eq(&BACKFILL_AUTHORITY, &TEST_PLACEHOLDER_AUTHORITY));
    }

    #[test]
    fn pubkey_eq_true_on_identical_arrays() {
        assert!(pubkey_eq(
            &TEST_PLACEHOLDER_AUTHORITY,
            &TEST_PLACEHOLDER_AUTHORITY
        ));
    }

    #[test]
    fn pubkey_eq_false_on_first_byte_difference() {
        let mut other = TEST_PLACEHOLDER_AUTHORITY;
        other[0] ^= 0xFF;
        assert!(!pubkey_eq(&TEST_PLACEHOLDER_AUTHORITY, &other));
    }

    #[test]
    fn pubkey_eq_false_on_last_byte_difference() {
        let mut other = TEST_PLACEHOLDER_AUTHORITY;
        other[31] ^= 0xFF;
        assert!(!pubkey_eq(&TEST_PLACEHOLDER_AUTHORITY, &other));
    }
}

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
