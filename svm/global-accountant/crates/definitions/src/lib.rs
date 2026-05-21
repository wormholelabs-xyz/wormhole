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
    // `const`-callable so future compile-time dispatch tables can use it.
    pub const fn from_u8(value: u8) -> Option<Self> {
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
    /// The instruction exists in the dispatch table but the build it was
    /// compiled into intentionally disabled it (Cargo-feature-gated). Used to
    /// keep `open_digest` out of production builds until `submit_observations`
    /// is the only legitimate caller.
    NotEnabled = 6,
}

impl From<GlobalAccountantError> for u32 {
    fn from(e: GlobalAccountantError) -> Self {
        e as u32
    }
}

/// PDA seed prefix for [`DigestAccountLayout`].
pub const DIGEST_SEED_PREFIX: &[u8] = b"digest";

/// Verify VAA Shim program ID (`EFaNWErqAtVWufdNb7yofSHHfWFos843DFpu4JBw24at`).
///
/// The Shim deploys to the same address on mainnet, devnet, and Wormhole's Tilt
/// localnet; see `svm/wormhole-core-shims/crates/definitions/src/solana.rs`.
///
/// Vendored as a raw byte array so this crate stays free of Solana SDK
/// dependencies (the canonical definition pulls in `solana-program` 1.18..=2.x,
/// which would conflict with the program crate's Pinocchio + `solana-*` 3.x
/// dev-deps). The bytes are the base58 decoding of the program ID.
pub const VERIFY_VAA_SHIM_PROGRAM_ID: Pubkey = [
    196, 227, 203, 55, 17, 156, 166, 124, 168, 35, 28, 170, 3, 131, 164, 140, 195, 254, 137, 233,
    101, 80, 83, 225, 249, 25, 254, 66, 226, 131, 254, 161,
];

/// Anchor discriminator for the Verify VAA Shim's `verify_hash` instruction.
///
/// Equal to the first 8 bytes of `sha256("global:verify_hash")`. Mirrors the
/// constant computed at compile time in
/// `svm/wormhole-core-shims/crates/shim/src/verify_vaa/mod.rs::VerifyVaaShimInstruction::VERIFY_HASH_SELECTOR`.
pub const VERIFY_HASH_SELECTOR: [u8; 8] = [22, 152, 160, 69, 241, 148, 14, 124];

/// Wire-format size of the `verify_hash` instruction data: 8-byte selector +
/// 1-byte guardian-set bump + 32-byte digest.
pub const VERIFY_HASH_DATA_LEN: usize = 8 + 1 + 32;

/// 256-bit unsigned integer stored on-disk as 32 **big-endian** bytes.
///
/// Width matches both the CosmWasm `accountant::state::account::Balance(Uint256)`
/// baseline (see `cosmwasm/packages/accountant/src/state/account.rs:101`) and
/// the Wormhole VAA wire format — Token Bridge transfer payloads encode the
/// `amount` field as a 32-byte big-endian unsigned integer (whitepaper
/// `0003_token_bridge.md`). Storing big-endian on-chain means a VAA's bytes can
/// be copied directly into the balance account without re-ordering.
///
/// `#[repr(transparent)]` over `[u8; 32]` keeps the type `Pod`-compatible so it
/// can sit inside a zero-copy account layout. Arithmetic round-trips through
/// `ruint::aliases::U256` (limb-based, little-endian internally) at the
/// boundary; the on-disk representation never changes.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct Uint256(pub [u8; 32]);

impl Uint256 {
    /// All-zero value.
    pub const ZERO: Self = Self([0u8; 32]);

    /// All-ones value (`2^256 - 1`).
    pub const MAX: Self = Self([0xffu8; 32]);

    /// Build a `Uint256` from a `u128`, big-endian. The low 16 bytes carry the
    /// value; the high 16 bytes are zero. Mirrors `cosmwasm_std::Uint256::from`
    /// for u128 inputs.
    pub const fn from_u128(v: u128) -> Self {
        let v_be = v.to_be_bytes();
        let mut bytes = [0u8; 32];
        let mut i = 0;
        while i < 16 {
            bytes[16 + i] = v_be[i];
            i += 1;
        }
        Self(bytes)
    }

    /// Down-cast to a `u128` if the value fits; otherwise `None`. The check is
    /// "are the high 16 bytes all zero?".
    pub fn to_u128(self) -> Option<u128> {
        let (hi, lo) = self.0.split_at(16);
        for byte in hi {
            if *byte != 0 {
                return None;
            }
        }
        let mut lo_array = [0u8; 16];
        lo_array.copy_from_slice(lo);
        Some(u128::from_be_bytes(lo_array))
    }

    /// Saturating-free add. Returns `None` on overflow, matching the CosmWasm
    /// `Uint256::checked_add` semantic that propagates as an `Err` to the
    /// caller (we convert to `ProgramError::Custom` at the program boundary).
    #[inline]
    pub fn checked_add(self, other: Self) -> Option<Self> {
        let a = ruint::aliases::U256::from_be_bytes::<32>(self.0);
        let b = ruint::aliases::U256::from_be_bytes::<32>(other.0);
        a.checked_add(b).map(|r| Self(r.to_be_bytes::<32>()))
    }

    /// Subtract, returning `None` on underflow.
    #[inline]
    pub fn checked_sub(self, other: Self) -> Option<Self> {
        let a = ruint::aliases::U256::from_be_bytes::<32>(self.0);
        let b = ruint::aliases::U256::from_be_bytes::<32>(other.0);
        a.checked_sub(b).map(|r| Self(r.to_be_bytes::<32>()))
    }

    /// Borrow the big-endian byte representation. Suitable for direct copy
    /// into / out of a VAA transfer payload's `amount` field.
    pub const fn as_be_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Construct from 32 big-endian bytes (e.g. the `amount` slice of a
    /// Token Bridge transfer payload).
    pub const fn from_be_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl PartialOrd for Uint256 {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Uint256 {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        // Lexicographic over big-endian bytes is numerical order for unsigned
        // big-endian representations.
        self.0.cmp(&other.0)
    }
}

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
/// account ported from the CosmWasm `accountant::state::account::Account`
/// (`cosmwasm/packages/accountant/src/state/account.rs`).
///
/// Total on-disk size is **76 bytes**, matching the CosmWasm baseline byte
/// budget for an `Account` record. Field ordering keeps natural alignment for
/// the `u16`s up front (every field has alignment 1 or 2; `Uint256` is
/// `repr(transparent)` over `[u8; 32]` so it inherits alignment 1).
///
/// | offset | size | field         |
/// |--------|------|---------------|
/// | 0      | 2    | chain         |
/// | 2      | 2    | token_chain   |
/// | 4      | 32   | token_address |
/// | 36     | 32   | balance       |
/// | 68     | 8    | _reserved     |
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct BalanceAccountLayout {
    /// Chain on which this balance is held (CW: `key.chain_id`).
    pub chain: u16,
    /// Native chain of the token (CW: `key.token_chain`).
    pub token_chain: u16,
    /// Token address on its native chain (CW: `key.token_address`, 32 bytes).
    pub token_address: [u8; 32],
    /// Current balance. 32-byte big-endian unsigned integer to match the
    /// CosmWasm `Balance(Uint256)` baseline and the Wormhole VAA wire format
    /// (Token Bridge transfer payloads encode `amount` as big-endian u256).
    pub balance: Uint256,
    /// Reserved for forward compatibility (e.g. a version byte plus padding).
    /// Crate-private so callers go through `Zeroable` for new instances.
    pub(crate) _reserved: [u8; 8],
}

impl BalanceAccountLayout {
    pub const LEN: usize = core::mem::size_of::<Self>();
}

// Compile-time pins for the balance layout. Mirrors the DigestAccount pattern
// so a stray reorder fails the build instead of silently corrupting state.
const _: () = {
    use core::mem::offset_of;
    assert!(offset_of!(BalanceAccountLayout, chain) == 0);
    assert!(offset_of!(BalanceAccountLayout, token_chain) == 2);
    assert!(offset_of!(BalanceAccountLayout, token_address) == 4);
    assert!(offset_of!(BalanceAccountLayout, balance) == 36);
    assert!(offset_of!(BalanceAccountLayout, _reserved) == 68);
    assert!(BalanceAccountLayout::LEN == 76);
};

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

    // ---- Uint256 unit tests (port of CosmWasm account.rs:152-323 cases) ----

    #[test]
    fn uint256_add_basic() {
        let a = Uint256::from_u128(500);
        let b = Uint256::from_u128(200);
        assert_eq!(a.checked_add(b), Some(Uint256::from_u128(700)));
    }

    #[test]
    fn uint256_add_overflow() {
        // CosmWasm `native_lock_overflow` / `wrapped_mint_overflow` analogue.
        assert_eq!(Uint256::MAX.checked_add(Uint256::from_u128(200)), None);
    }

    #[test]
    fn uint256_sub_basic() {
        let a = Uint256::from_u128(500);
        let b = Uint256::from_u128(200);
        assert_eq!(a.checked_sub(b), Some(Uint256::from_u128(300)));
    }

    #[test]
    fn uint256_sub_underflow() {
        // CosmWasm `native_unlock_underflow` / `wrapped_burn_underflow` analogue.
        assert_eq!(
            Uint256::ZERO.checked_sub(Uint256::from_u128(200)),
            None,
            "subtracting from zero must return None, not wrap"
        );
    }

    #[test]
    fn uint256_round_trip_bytes() {
        // Pack/unpack through the `[u8; 32]` representation preserves the value
        // bit-for-bit.
        let original = Uint256::from_u128(0xdead_beef_cafe_babe_u128);
        let bytes: [u8; 32] = *original.as_be_bytes();
        let restored = Uint256::from_be_bytes(bytes);
        assert_eq!(original, restored);
    }

    #[test]
    fn uint256_big_endian_wire_order() {
        // `0x1234` packs with the most-significant byte at offset 30 — i.e.
        // network byte order. Matches Token Bridge transfer payload `amount`
        // encoding (whitepaper 0003).
        let v = Uint256::from_u128(0x1234);
        let bytes = v.as_be_bytes();
        let mut expected = [0u8; 32];
        expected[30] = 0x12;
        expected[31] = 0x34;
        assert_eq!(bytes, &expected);
    }

    #[test]
    fn uint256_ordering() {
        assert!(Uint256::from_u128(1) < Uint256::from_u128(2));
        assert!(Uint256::MAX > Uint256::from_u128(u128::MAX));
        assert!(Uint256::ZERO < Uint256::from_u128(1));
        // Lexicographic-over-big-endian == numerical order: a value with a
        // higher MSB sorts higher even if the LSBs are smaller.
        let mut a_bytes = [0u8; 32];
        a_bytes[0] = 0x01;
        let a = Uint256::from_be_bytes(a_bytes);
        let b = Uint256::from_u128(u128::MAX);
        assert!(a > b);
    }

    #[test]
    fn uint256_to_u128_in_range() {
        let v = Uint256::from_u128(0xdead_beef);
        assert_eq!(v.to_u128(), Some(0xdead_beef));
    }

    #[test]
    fn uint256_to_u128_out_of_range() {
        // Any non-zero byte in the high 16 bytes pushes the value above
        // u128::MAX.
        let mut bytes = [0u8; 32];
        bytes[15] = 0x01;
        let v = Uint256::from_be_bytes(bytes);
        assert_eq!(v.to_u128(), None);
    }

    #[test]
    fn uint256_add_then_sub_round_trips() {
        // CosmWasm `native_lock` (500 + 200 = 700) then unwind back to 500.
        let start = Uint256::from_u128(500);
        let added = start.checked_add(Uint256::from_u128(200)).unwrap();
        assert_eq!(added, Uint256::from_u128(700));
        let restored = added.checked_sub(Uint256::from_u128(200)).unwrap();
        assert_eq!(restored, start);
    }

    // ---- BalanceAccountLayout tests ----

    #[test]
    fn balance_layout_size_matches_cosmwasm() {
        // CosmWasm `Balance(Uint256)` + `Key { chain_id: u16, token_chain: u16,
        // token_address: [u8; 32] }` ≈ 76 bytes on disk. Pin exactly so the
        // backfill program and migration tooling agree on the rent budget.
        assert_eq!(BalanceAccountLayout::LEN, 76);
    }

    #[test]
    fn balance_layout_uint256_offsets_pinned() {
        // Runtime mirror of the const-assert block above. A reviewer reads this
        // test rather than tracing through `offset_of!` macros.
        use core::mem::offset_of;
        assert_eq!(offset_of!(BalanceAccountLayout, chain), 0);
        assert_eq!(offset_of!(BalanceAccountLayout, token_chain), 2);
        assert_eq!(offset_of!(BalanceAccountLayout, token_address), 4);
        assert_eq!(offset_of!(BalanceAccountLayout, balance), 36);
        assert_eq!(offset_of!(BalanceAccountLayout, _reserved), 68);
    }

    #[test]
    fn balance_layout_is_pod_friendly() {
        // Round-trip through bytes preserves every field.
        let mut token_address = [0u8; 32];
        for (i, b) in token_address.iter_mut().enumerate() {
            *b = i as u8;
        }
        let original = BalanceAccountLayout {
            chain: 1,
            token_chain: 2,
            token_address,
            balance: Uint256::from_u128(0xcafe_babe),
            _reserved: [0u8; 8],
        };
        let bytes = bytemuck::bytes_of(&original);
        assert_eq!(bytes.len(), BalanceAccountLayout::LEN);
        let copy: &BalanceAccountLayout = bytemuck::from_bytes(bytes);
        assert_eq!(&original, copy);
    }

    #[test]
    fn balance_layout_balance_encodes_big_endian_on_disk() {
        // The balance field's bytes inside the packed account must be exactly
        // the big-endian encoding of the value. This is the property that lets
        // a VAA transfer payload's `amount` slice be copied directly without
        // any byte-order conversion.
        let original = BalanceAccountLayout {
            chain: 0,
            token_chain: 0,
            token_address: [0u8; 32],
            balance: Uint256::from_u128(0x1234_5678),
            _reserved: [0u8; 8],
        };
        let bytes = bytemuck::bytes_of(&original);
        let balance_slice = &bytes[36..68];
        let mut expected = [0u8; 32];
        expected[28] = 0x12;
        expected[29] = 0x34;
        expected[30] = 0x56;
        expected[31] = 0x78;
        assert_eq!(balance_slice, &expected);
    }
}
