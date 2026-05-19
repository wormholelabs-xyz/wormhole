//! Shared types and constants for the Wormhole Global Accountant Solana program.
//!
//! This crate intentionally has no Solana dependency so the same layouts can be
//! re-used from on-chain code, host-side tests, and (eventually) client tooling.

#![no_std]

use bytemuck::{Pod, Zeroable};

/// 32-byte address, layout-compatible with `solana_address::Address` and
/// `pinocchio`'s re-exported `Address`. Kept untyped here to avoid pulling in
/// Solana SDK crates from the definitions layer.
pub type Pubkey = [u8; 32];

/// Instruction discriminators. Single-byte prefix on the instruction data.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Instruction {
    OpenDigest = 0,
    CloseDigest = 1,
    SubmitObservations = 2,
}

impl Instruction {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::OpenDigest),
            1 => Some(Self::CloseDigest),
            2 => Some(Self::SubmitObservations),
            _ => None,
        }
    }
}

/// Custom error codes returned via `ProgramError::Custom(u32)`. Stable across
/// program versions; do not renumber.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GlobalAccountantError {
    InvalidInstruction = 0,
    InvalidInstructionData = 1,
    InvalidPda = 2,
    DigestMismatch = 3,
    PayerMismatch = 4,
    NotImplemented = 5,
}

impl From<GlobalAccountantError> for u32 {
    fn from(e: GlobalAccountantError) -> Self {
        e as u32
    }
}

/// PDA seed prefix for [`DigestAccountLayout`].
pub const DIGEST_SEED_PREFIX: &[u8] = b"digest";

/// Zero-copy layout for a `DigestAccount` PDA. See
/// `accountant-migration-digest-design.md` §3 for design rationale.
///
/// Field ordering keeps natural alignment without padding (`u64`s on 8-byte
/// boundaries, `u32` after the `u64`s, `u16` last).
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct DigestAccountLayout {
    pub emitter: Pubkey,
    pub digest: [u8; 32],
    pub payer: Pubkey,
    pub sequence: u64,
    pub quorum_at_slot: u64,
    pub guardian_set_index: u32,
    pub chain: u16,
    /// Reserved for future use (e.g. version byte). Zero-initialised on open.
    /// Crate-private so external callers cannot inject garbage via struct
    /// literals — go through `Zeroable` for new instances.
    pub(crate) _padding: [u8; 2],
}

impl DigestAccountLayout {
    /// Byte length of the layout (also the rent-paying allocation size).
    pub const LEN: usize = core::mem::size_of::<Self>();
}

// Compile-time pins against accidental layout drift. The runtime test
// `digest_layout_offsets_pinned` in the program test crate complements these
// const-asserts with a human-readable form a reviewer can scan.
const _: () = {
    use core::mem::offset_of;
    assert!(offset_of!(DigestAccountLayout, emitter) == 0);
    assert!(offset_of!(DigestAccountLayout, digest) == 32);
    assert!(offset_of!(DigestAccountLayout, payer) == 64);
    assert!(offset_of!(DigestAccountLayout, sequence) == 96);
    assert!(offset_of!(DigestAccountLayout, quorum_at_slot) == 104);
    assert!(offset_of!(DigestAccountLayout, guardian_set_index) == 112);
    assert!(offset_of!(DigestAccountLayout, chain) == 116);
    assert!(DigestAccountLayout::LEN == 120);
};

/// Zero-copy layout for the per-(chain, token_chain, token_address) balance
/// account ported from the CosmWasm `accountant::state::account::Account`.
///
/// Not used in this slice — included so the on-chain layout is fixed before
/// any logic is written against it. 80 bytes total with natural alignment.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct BalanceAccountLayout {
    /// Native chain of the token (CW: `key.token_chain`).
    pub token_chain: u16,
    /// Chain on which this balance is held (CW: `key.chain_id`).
    pub chain: u16,
    pub(crate) _padding: [u8; 12],
    /// Token address on its native chain (CW: `key.token_address`, 32 bytes).
    pub token_address: [u8; 32],
    /// Current balance. CosmWasm uses `Uint256`; for this layout we follow the
    /// migration plan's 16-byte (`u128`) width. Stored little-endian per
    /// `bytemuck` conventions.
    pub balance: u128,
    pub(crate) _reserved: [u8; 16],
}

impl BalanceAccountLayout {
    pub const LEN: usize = core::mem::size_of::<Self>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_layout_is_pod_friendly() {
        // Sanity: round-trip through bytes.
        let original = DigestAccountLayout {
            emitter: [1u8; 32],
            digest: [2u8; 32],
            payer: [3u8; 32],
            sequence: 0xdead_beef_cafe_babe,
            quorum_at_slot: 42,
            guardian_set_index: 7,
            chain: 1,
            _padding: [0; 2],
        };
        let bytes = bytemuck::bytes_of(&original);
        let copy: &DigestAccountLayout = bytemuck::from_bytes(bytes);
        assert_eq!(&original, copy);
        assert_eq!(DigestAccountLayout::LEN, bytes.len());
    }

    #[test]
    fn balance_layout_is_80_bytes() {
        // Pin the width so downstream PDA seeders and migrations agree.
        assert_eq!(BalanceAccountLayout::LEN, 80);
    }
}
