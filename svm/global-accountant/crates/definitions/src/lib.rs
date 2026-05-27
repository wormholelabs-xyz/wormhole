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
    ClosePending = 3,
}

impl Instruction {
    // `const`-callable so future compile-time dispatch tables can use it.
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::OpenDigest),
            1 => Some(Self::CloseDigest),
            2 => Some(Self::SubmitObservations),
            3 => Some(Self::ClosePending),
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
    /// The (chain, emitter, sequence) is already marked as accounted-for in
    /// NoReplay; observations are rejected as replays before any signature
    /// verification or PDA work. See
    /// `accountant-migration-noreplay-integration.md` §3.
    AlreadyAccounted = 7,
    /// The NoReplay `MarkUsed` CPI returned an error after our pre-check passed
    /// — a defence-in-depth backstop for any racing tx that flipped the slot
    /// in the same block.
    NoReplayCpiFailed = 8,
    /// The submitted signature failed `secp256k1_recover` or the recovered
    /// pubkey did not match the supplied `guardian_index`'s public key in the
    /// Core Bridge GuardianSet PDA. See
    /// `accountant-migration-pending-quorum-design.md` §3.4.
    InvalidSignature = 9,
    /// The supplied `guardian_index` is out of bounds for the supplied
    /// guardian set.
    InvalidGuardianIndex = 10,
    /// The corresponding bit in the pending-PDA's signature bitmap is already
    /// set — the submitter has already counted this guardian.
    AlreadySigned = 11,
    /// The observation references a guardian set strictly older than the one
    /// the existing pending PDA is accumulating against (i.e., a stale
    /// observation arrived after rotation). See §3.3 rule "Older than the
    /// active set".
    StaleGuardianSet = 12,
    /// The observation's digest does not match the digest the pending PDA was
    /// opened with, while the `guardian_set_index` is identical. Distinct from
    /// the rotation case (which wipes-and-recreates) — same set, different
    /// digest is a forgery attempt.
    DigestForgery = 13,
    /// `close_pending` was called but neither of the two acceptable triggers
    /// holds: the recorded guardian set is still active AND NoReplay does not
    /// mark the entry as accounted-for. See §3.6.
    CannotCleanup = 14,
}

impl From<GlobalAccountantError> for u32 {
    fn from(e: GlobalAccountantError) -> Self {
        e as u32
    }
}

/// PDA seed prefix for [`DigestAccountLayout`].
pub const DIGEST_SEED_PREFIX: &[u8] = b"digest";

/// PDA seed prefix for [`PendingObservationsLayout`]. The full seed tuple is
/// `(b"pending", chain.to_be_bytes(), emitter, sequence.to_be_bytes(), digest)`
/// — the digest suffix is what lets fork/reorg observations (same chain /
/// emitter / sequence but a different body-hash) accumulate in parallel
/// sibling buckets rather than colliding on a single bucket and getting stuck
/// on a `DigestForgery` rejection. See
/// `accountant-migration-pending-quorum-design.md` §3.1.
pub const PENDING_SEED_PREFIX: &[u8] = b"pending";

/// PDA seed prefix for the global-accountant-owned authority that signs all
/// `solana-noreplay` CPIs. The full seed tuple is just `[b"noreplay-authority"]`
/// — one global authority is sufficient because the noreplay namespace
/// (`chain_be ‖ emitter`) already segregates per-emitter sequence spaces, and
/// the authority itself only needs to be unique-per-program so a different
/// global-accountant deployment cannot stomp on this one's bitmap namespace.
///
/// Only global-accountant can sign for this PDA via `invoke_signed`. The
/// resulting bitmap buckets are therefore exclusively write-controlled by this
/// program; the noreplay processor enforces that constraint by deriving the
/// bitmap PDA from the supplied authority pubkey and rejecting any caller
/// whose `is_signer` bit is not set on the authority slot.
pub const NOREPLAY_AUTHORITY_SEED_PREFIX: &[u8] = b"noreplay-authority";

/// Canonical mainnet/devnet program ID for `solana-noreplay`
/// (`repMHgR5BEpGLeZvM5iGoNNDPw4eu2BS6sXJzaC8K4t`). Pinned as a raw byte array
/// so this crate stays Solana-SDK-free (the canonical `Pubkey::from_str_const`
/// path would drag in `solana-program`). Verified against the `NOREPLAY_PROGRAM_ID`
/// env var baked into the upstream `solana-noreplay` binary at compile time
/// (see `~/WormholeLabs/CoreTeam/solana-noreplay/program/src/client.rs::PROGRAM_ID`).
///
/// Deferred: deploying this program at a stable mainnet ID and locking its
/// upgrade authority is a separate workstream — see
/// `accountant-migration.md` §15.
pub const NOREPLAY_PROGRAM_ID: Pubkey = [
    0x0c, 0xb8, 0x38, 0x00, 0x73, 0xdf, 0x36, 0x25, 0xa1, 0x32, 0x11, 0x1f, 0xee, 0x67, 0x8d, 0xd0,
    0x6b, 0x7e, 0x3d, 0xf2, 0x90, 0xa2, 0xb1, 0xd5, 0x4a, 0x48, 0x5b, 0xdb, 0x72, 0x61, 0x82, 0x91,
];

/// Discriminator for `solana-noreplay`'s `MarkUsed` instruction. Single-byte
/// prefix on the wire (the noreplay wire format is documented in
/// `accountant-migration-noreplay-integration.md` §2):
///
/// `[disc: u8][namespace_len: u16 LE][namespace: ≤64 B][sequence: u64 LE]`
pub const NOREPLAY_MARK_USED_DISCRIMINATOR: u8 = 1;

/// Bits per bitmap bucket. Mirrors `solana_noreplay::state::BITS_PER_BUCKET`.
/// Used to derive both the bucket index (`sequence / BITS_PER_BUCKET`) and the
/// bit offset within the bucket (`sequence % BITS_PER_BUCKET`).
pub const NOREPLAY_BITS_PER_BUCKET: u64 = 1024;

/// Byte size of the bitmap payload inside a noreplay PDA. The full account is
/// 129 bytes (1-byte stored bump + 128-byte bitmap). Mirrors
/// `solana_noreplay::state::BITMAP_BYTES`.
pub const NOREPLAY_BITMAP_BYTES: usize = 128;

/// Byte offset where the bitmap payload starts inside a noreplay account.
/// Byte 0 is the stored canonical bump; bytes 1..=128 are the bitmap.
pub const NOREPLAY_BITMAP_OFFSET: usize = 1;

/// Maximum namespace length accepted by the noreplay program. Mirrors
/// `solana_noreplay::MAX_NAMESPACE_LEN`.
pub const NOREPLAY_MAX_NAMESPACE_LEN: usize = 64;

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

/// Zero-copy layout for a per-`(chain, emitter, sequence)` pending-quorum PDA.
/// See [`accountant-migration-pending-quorum-design.md`] §4 for the design
/// rationale and lifecycle diagram.
///
/// Design doc lists 84 bytes payload. The actual on-disk layout is **88
/// bytes** because the `created_at_slot: u64` field forces 8-byte alignment
/// on the whole struct, and Rust pads the size out to the alignment. We move
/// `created_at_slot` to the front of the integer block so the padding sits at
/// the tail (where it is named explicitly via `_tail_padding`) and `bytemuck`
/// can derive `Pod` cleanly. The visible field order stays: digest, payer,
/// guardian_set_index, signatures, created_at_slot, chain — matching the
/// design doc — and the byte layout is pinned by the const-asserts below.
///
/// | offset | size | field              |
/// |--------|------|--------------------|
/// | 0      | 32   | digest             |
/// | 32     | 32   | payer              |
/// | 64     | 8    | created_at_slot    |
/// | 72     | 4    | guardian_set_index |
/// | 76     | 4    | signatures (u32 bitmap; bit N == guardian-index N signed) |
/// | 80     | 2    | chain              |
/// | 82     | 6    | _padding (explicit; required by `Pod` derive) |
///
/// 88-byte total. The 32-bit bitmap covers 32 guardian indices; today's
/// mainnet set is 19. If the protocol ever requires >32 guardians the field
/// must widen (and the PDA layout version bumped) — pinned in §10 risks.
///
/// [`accountant-migration-pending-quorum-design.md`]: ../../../../.claude/tasks/accountant-migration-pending-quorum-design.md
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct PendingObservationsLayout {
    pub digest: [u8; 32],
    pub payer: Pubkey,
    pub created_at_slot: u64,
    pub guardian_set_index: u32,
    pub signatures: u32,
    pub chain: u16,
    /// Explicit tail padding — required because `created_at_slot: u64`
    /// forces 8-byte struct alignment and the trailing `u16 + reserved`
    /// would otherwise be silent compiler-emitted padding (which trips
    /// `bytemuck::Pod`'s "no implicit padding" check). Zero-initialised on
    /// open. Crate-private so external callers cannot inject garbage via
    /// struct literals — go through `Zeroable` for new instances. Mirrors
    /// the `DigestAccountLayout._padding` privacy pattern.
    pub(crate) _padding: [u8; 6],
}

impl PendingObservationsLayout {
    /// Byte length of the layout (also the rent-paying allocation size).
    pub const LEN: usize = core::mem::size_of::<Self>();

    /// Quorum threshold: 13 of 19 guardians. Matches the Core Bridge's
    /// `(keys.len() * 2) / 3 + 1` formula for `keys.len() == 19` and is the
    /// CosmWasm Global Accountant's hard-coded threshold today. Re-deriving
    /// from the live GuardianSet would couple this constant to the set's
    /// runtime size; keep it pinned and refuse to commit if the set ever
    /// shrinks below 13 guardians (which would itself be a protocol-level
    /// emergency).
    pub const QUORUM_THRESHOLD: u32 = 13;
}

const _: () = {
    use core::mem::offset_of;
    assert!(offset_of!(PendingObservationsLayout, digest) == 0);
    assert!(offset_of!(PendingObservationsLayout, payer) == 32);
    assert!(offset_of!(PendingObservationsLayout, created_at_slot) == 64);
    assert!(offset_of!(PendingObservationsLayout, guardian_set_index) == 72);
    assert!(offset_of!(PendingObservationsLayout, signatures) == 76);
    assert!(offset_of!(PendingObservationsLayout, chain) == 80);
    assert!(PendingObservationsLayout::LEN == 88);
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

    // ---- PendingObservationsLayout tests ----

    #[test]
    fn pending_layout_size_pinned() {
        // The const-assert above is the primary defence; this is the
        // human-readable runtime mirror a reviewer can scan against the design
        // doc §4 byte map. Design doc lists 84 bytes payload; the realised
        // layout is 88 bytes after the explicit tail padding required for
        // `Pod`-derive cleanliness — see the type-doc for the rationale.
        assert_eq!(PendingObservationsLayout::LEN, 88);
    }

    #[test]
    fn pending_layout_offsets_pinned() {
        use core::mem::offset_of;
        assert_eq!(offset_of!(PendingObservationsLayout, digest), 0);
        assert_eq!(offset_of!(PendingObservationsLayout, payer), 32);
        assert_eq!(offset_of!(PendingObservationsLayout, created_at_slot), 64);
        assert_eq!(offset_of!(PendingObservationsLayout, guardian_set_index), 72);
        assert_eq!(offset_of!(PendingObservationsLayout, signatures), 76);
        assert_eq!(offset_of!(PendingObservationsLayout, chain), 80);
    }

    #[test]
    fn pending_layout_is_pod_friendly() {
        let mut digest = [0u8; 32];
        for (i, b) in digest.iter_mut().enumerate() {
            *b = i as u8;
        }
        let original = PendingObservationsLayout {
            digest,
            payer: [0xAA; 32],
            created_at_slot: 0xdead_beef_cafe_babe,
            guardian_set_index: 0x0BAD_CAFE,
            signatures: 0x0000_1FFFu32, // 13 low bits set
            chain: 1,
            _padding: [0; 6],
        };
        let bytes = bytemuck::bytes_of(&original);
        let copy: &PendingObservationsLayout = bytemuck::from_bytes(bytes);
        assert_eq!(&original, copy);
        assert_eq!(PendingObservationsLayout::LEN, bytes.len());
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
