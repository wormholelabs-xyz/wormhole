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
    /// Permissionless signed-VAA backfill. Consumes a fully-signed VAA via
    /// the Verify VAA Shim CPI and applies its balance effects directly,
    /// bypassing the quorum tracker.
    SubmitVaas = 4,
}

impl Instruction {
    // `const`-callable so future compile-time dispatch tables can use it.
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::OpenDigest),
            1 => Some(Self::CloseDigest),
            2 => Some(Self::SubmitObservations),
            3 => Some(Self::ClosePending),
            4 => Some(Self::SubmitVaas),
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
    /// verification or PDA work.
    AlreadyAccounted = 7,
    /// The NoReplay `MarkUsed` CPI returned an error after our pre-check passed
    /// — a defence-in-depth backstop for any racing tx that flipped the slot
    /// in the same block.
    NoReplayCpiFailed = 8,
    /// The submitted signature failed `secp256k1_recover` or the recovered
    /// pubkey did not match the supplied `guardian_index`'s public key in the
    /// Core Bridge GuardianSet PDA.
    InvalidSignature = 9,
    /// The supplied `guardian_index` is out of bounds for the supplied
    /// guardian set.
    InvalidGuardianIndex = 10,
    /// The corresponding bit in the pending-PDA's signature bitmap is already
    /// set — the submitter has already counted this guardian.
    AlreadySigned = 11,
    /// The observation references a guardian set strictly older than the one
    /// the existing pending PDA is accumulating against (i.e., a stale
    /// observation arrived after rotation).
    StaleGuardianSet = 12,
    /// The observation's digest does not match the digest the pending PDA was
    /// opened with, while the `guardian_set_index` is identical. Distinct from
    /// the rotation case (which wipes-and-recreates) — same set, different
    /// digest is a forgery attempt.
    DigestForgery = 13,
    /// `close_pending` was called but neither of the two acceptable triggers
    /// holds: the recorded guardian set is still active AND NoReplay does not
    /// mark the entry as accounted-for.
    CannotCleanup = 14,
    /// The 256-bit `BalanceAccountLayout::balance` would overflow when applying
    /// a `lock_or_burn` (native-chain credit) or `unlock_or_mint` (wrapped-chain
    /// credit). Surfaces as a hard tx revert from the quorum-completing branch
    /// of `submit_observations`; mirrors CosmWasm's
    /// `Account::lock_or_burn` / `Account::unlock_or_mint` returning
    /// `StdError::Overflow` (`cosmwasm/packages/accountant/src/state/account.rs`).
    BalanceOverflow = 15,
    /// The 256-bit `BalanceAccountLayout::balance` would underflow when applying
    /// a `lock_or_burn` (wrapped-chain debit) or `unlock_or_mint` (native-chain
    /// debit). The same hard-revert behaviour as `BalanceOverflow`; CosmWasm
    /// also folds this into `StdError::Overflow` because cosmwasm's `Uint256`
    /// returns a single `OverflowError` for both directions.
    BalanceUnderflow = 16,
    /// `submit_observations`'s body bytes did not hash to the supplied digest
    /// (`keccak256(keccak256(body)) != digest`). Refuses the submission before
    /// any state mutation — the body is what carries the Token Bridge transfer
    /// payload that the quorum-commit branch reads, so an unverified body
    /// could route credits/debits at the wrong amount, chain, or token.
    BodyDigestMismatch = 17,
    /// `submit_observations`'s account list passed an Account PDA whose
    /// `(chain, token_chain, token_address)` triple does not match the
    /// canonical seeds for the source-chain or destination-chain side of the
    /// transfer. Re-derives via `find_program_address` and rejects any
    /// mismatch; mirrors the canonical-bump pattern used for the pending PDA.
    InvalidAccountPda = 18,
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
/// on a `DigestForgery` rejection.
pub const PENDING_SEED_PREFIX: &[u8] = b"pending";

/// PDA seed prefix for [`BalanceAccountLayout`]. The full seed tuple is
/// `(b"account", chain.to_be_bytes(), token_chain.to_be_bytes(), token_address)`.
/// Each unique `(chain, token_chain, token_address)` triple has exactly one
/// canonical PDA under the global-accountant program ID — the on-disk record
/// the CosmWasm contract calls `Account`. Big-endian byte order on `chain` /
/// `token_chain` matches the VAA wire format and the `DIGEST_SEED_PREFIX` /
/// `PENDING_SEED_PREFIX` derivations so all three keying schemes agree.
pub const ACCOUNT_SEED_PREFIX: &[u8] = b"account";

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
/// upgrade authority is a separate workstream.
pub const NOREPLAY_PROGRAM_ID: Pubkey = [
    0x0c, 0xb8, 0x38, 0x00, 0x73, 0xdf, 0x36, 0x25, 0xa1, 0x32, 0x11, 0x1f, 0xee, 0x67, 0x8d, 0xd0,
    0x6b, 0x7e, 0x3d, 0xf2, 0x90, 0xa2, 0xb1, 0xd5, 0x4a, 0x48, 0x5b, 0xdb, 0x72, 0x61, 0x82, 0x91,
];

/// Discriminator for `solana-noreplay`'s `MarkUsed` instruction. Single-byte
/// prefix on the wire:
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

/// Wormhole Core Bridge program ID on Solana mainnet
/// (`worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth`).
///
/// Used by `close_pending` to verify the supplied `GuardianSet` account is
/// genuinely owned by the Core Bridge before reading any bytes from it.
/// Without that check, a caller could pass an arbitrary account with bytes
/// claiming the set is expired and force a permanent DoS of any pending PDA
/// (see the regression test in `tests/submit_observations.rs`).
///
/// Vendored as raw bytes so this crate stays Solana-SDK-free. Mirrors the
/// canonical definition at
/// `svm/wormhole-core-shims/crates/definitions/src/solana.rs::mainnet::CORE_BRIDGE_PROGRAM_ID_ARRAY`.
pub const CORE_BRIDGE_PROGRAM_ID: Pubkey = [
    0x0e, 0x0a, 0x58, 0x9a, 0x41, 0xa5, 0x5f, 0xbd, 0x66, 0xc5, 0x2a, 0x47, 0x5f, 0x2d, 0x92, 0xa6,
    0xd3, 0xdc, 0x9b, 0x47, 0x47, 0x11, 0x4c, 0xb9, 0xaf, 0x82, 0x5a, 0x98, 0xb5, 0x45, 0xd3, 0xce,
];

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

/// Zero-copy layout for a `DigestAccount` PDA.
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
///
/// The on-disk layout is **88 bytes**: the `created_at_slot: u64` field forces
/// 8-byte alignment on the whole struct, and Rust pads the size out to the
/// alignment. `created_at_slot` is placed at the front of the integer block so
/// the padding sits at the tail (named explicitly via `_padding`) and
/// `bytemuck` can derive `Pod` cleanly.
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
/// The 32-bit bitmap covers 32 guardian indices; today's mainnet set is 19.
/// If the protocol ever requires >32 guardians the field must widen and the
/// PDA layout version must be bumped.
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

    /// Port of CosmWasm `Account::lock_or_burn`
    /// (`cosmwasm/packages/accountant/src/state/account.rs:17-26`).
    ///
    /// Semantics, by chain identity:
    /// - `chain == token_chain` (this Account tracks the token on its native
    ///   chain): the message LOCKs tokens into the bridge, so the source-side
    ///   ledger credits — `balance += amount`. Overflow surfaces as
    ///   `BalanceOverflow`.
    /// - `chain != token_chain` (this Account tracks a wrapped representation
    ///   of a foreign token): the message BURNs wrapped tokens, so the
    ///   wrapped-chain ledger debits — `balance -= amount`. Underflow surfaces
    ///   as `BalanceUnderflow` (insufficient source balance — a transfer larger
    ///   than what was ever bridged in).
    ///
    /// The CosmWasm reference returns `StdError::Overflow` for both directions
    /// because `cosmwasm_std::Uint256` collapses overflow / underflow into a
    /// single error. We keep them distinct so on-chain logs disambiguate the
    /// two failure modes without re-decoding the payload.
    pub fn lock_or_burn(&mut self, amount: Uint256) -> Result<(), GlobalAccountantError> {
        if self.chain == self.token_chain {
            self.balance = self
                .balance
                .checked_add(amount)
                .ok_or(GlobalAccountantError::BalanceOverflow)?;
        } else {
            self.balance = self
                .balance
                .checked_sub(amount)
                .ok_or(GlobalAccountantError::BalanceUnderflow)?;
        }
        Ok(())
    }

    /// Port of CosmWasm `Account::unlock_or_mint`
    /// (`cosmwasm/packages/accountant/src/state/account.rs:28-36`).
    ///
    /// Symmetric to [`lock_or_burn`]:
    /// - `chain == token_chain` (native side, destination of an inbound
    ///   transfer): UNLOCKs the locked balance — `balance -= amount`. Underflow
    ///   surfaces as `BalanceUnderflow` (would unlock more native than the
    ///   bridge ever locked — a global-conservation violation).
    /// - `chain != token_chain` (wrapped side, destination of an outbound
    ///   transfer): MINTs new wrapped supply — `balance += amount`. Overflow
    ///   surfaces as `BalanceOverflow`.
    pub fn unlock_or_mint(&mut self, amount: Uint256) -> Result<(), GlobalAccountantError> {
        if self.chain == self.token_chain {
            self.balance = self
                .balance
                .checked_sub(amount)
                .ok_or(GlobalAccountantError::BalanceUnderflow)?;
        } else {
            self.balance = self
                .balance
                .checked_add(amount)
                .ok_or(GlobalAccountantError::BalanceOverflow)?;
        }
        Ok(())
    }
}

/// Decoded Token Bridge VAA body payload. Carries only the fields the
/// accountant needs at quorum commit; the recipient and fee fields are present
/// in the wire format but irrelevant here. Mirrors what CosmWasm extracts via
/// `wormhole_sdk::token::Message` in
/// `cosmwasm/contracts/global-accountant/src/contract.rs:217-240` (the SVM
/// port does its own byte-slice parse to stay free of `serde_wormhole`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenBridgeAction {
    /// Action 0x01 (`Transfer`) and 0x03 (`TransferWithPayload`) collapse to
    /// the same accountant logic — only `amount`, `token_chain`,
    /// `token_address`, and `recipient_chain` matter for balance updates. The
    /// CosmWasm reference handles both by destructuring the same fields.
    Transfer {
        amount: Uint256,
        token_chain: u16,
        token_address: [u8; 32],
        recipient_chain: u16,
    },
    /// Action 0x02 (`Attest`) — Token Bridge attestation metadata. Does not
    /// move value; the commit branch must still finish (NoReplay flip,
    /// DigestAccount open, pending close) but skips both balance updates.
    Attest,
    /// Any payload byte that is not `0x01`, `0x02`, or `0x03`. CosmWasm
    /// `bail!`s with "Unknown tokenbridge payload"; we treat the same way as
    /// `Attest` from the accountant's perspective — finish the commit, do not
    /// mutate balances. Callers can match on `Other` if they need to log.
    Other,
}

/// Parse a VAA body's payload — i.e. the bytes at `body[51..]` — into a
/// [`TokenBridgeAction`].
///
/// The VAA body layout (whitepaper `0001_generic_message_passing.md`):
///
/// | offset | size | field              |
/// |--------|------|--------------------|
/// | 0      | 4    | timestamp (u32 BE) |
/// | 4      | 4    | nonce (u32 BE)     |
/// | 8      | 2    | emitter_chain      |
/// | 10     | 32   | emitter_address    |
/// | 42     | 8    | sequence (u64 BE)  |
/// | 50     | 1    | consistency_level  |
/// | 51..   | rest | payload            |
///
/// Token Bridge transfer payload (whitepaper `0003_token_bridge.md`), starting
/// at offset 51 of the body:
///
/// | offset | size | field            |
/// |--------|------|------------------|
/// | 0      | 1    | action           |
/// | 1      | 32   | amount (Uint256) |
/// | 33     | 32   | token_address    |
/// | 65     | 2    | token_chain      |
/// | 67     | 32   | recipient        |
/// | 99     | 2    | recipient_chain  |
/// | 101    | 32   | fee (action 1)   |
/// | 133..  | rest | extra (action 3) |
///
/// On action 0x02 (attest) or any unknown byte, returns the corresponding
/// `Attest` / `Other` variant — the caller skips balance work and finishes the
/// commit.
///
/// `body` must be at least 52 bytes (one byte beyond the 51-byte header so we
/// can read the action byte). For transfer actions the slice must be ≥ 184
/// bytes (51 + 133). Both bounds are checked; the function returns
/// `InvalidInstructionData` on any short slice.
pub fn parse_token_bridge_payload(
    body: &[u8],
) -> Result<TokenBridgeAction, GlobalAccountantError> {
    const HEADER_LEN: usize = 51;
    const ACTION_TRANSFER: u8 = 0x01;
    const ACTION_ATTEST: u8 = 0x02;
    const ACTION_TRANSFER_WITH_PAYLOAD: u8 = 0x03;
    const TRANSFER_PAYLOAD_MIN: usize = 1 + 32 + 32 + 2 + 32 + 2 + 32; // 133

    if body.len() <= HEADER_LEN {
        return Err(GlobalAccountantError::InvalidInstructionData);
    }
    let payload = &body[HEADER_LEN..];
    let action = payload[0];
    match action {
        ACTION_TRANSFER | ACTION_TRANSFER_WITH_PAYLOAD => {
            if payload.len() < TRANSFER_PAYLOAD_MIN {
                return Err(GlobalAccountantError::InvalidInstructionData);
            }
            // Slice each field by offset; copy into stack arrays so the caller
            // owns the values without re-borrowing the body.
            let mut amount = [0u8; 32];
            amount.copy_from_slice(&payload[1..33]);
            let mut token_address = [0u8; 32];
            token_address.copy_from_slice(&payload[33..65]);
            let token_chain = u16::from_be_bytes([payload[65], payload[66]]);
            // payload[67..99] is recipient — ignored for accountant.
            let recipient_chain = u16::from_be_bytes([payload[99], payload[100]]);
            Ok(TokenBridgeAction::Transfer {
                amount: Uint256::from_be_bytes(amount),
                token_chain,
                token_address,
                recipient_chain,
            })
        }
        ACTION_ATTEST => Ok(TokenBridgeAction::Attest),
        _ => Ok(TokenBridgeAction::Other),
    }
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
        // human-readable runtime mirror. The 88-byte total includes 6 bytes of
        // explicit tail padding required for `Pod`-derive cleanliness — see
        // the type-doc for the rationale.
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

    // ---- BalanceAccountLayout::lock_or_burn / unlock_or_mint tests ----
    //
    // These are direct ports of the CosmWasm cases in
    // `cosmwasm/packages/accountant/src/state/account.rs:152-323`. The
    // semantic is identical (chain == token_chain ⇒ credit on lock_or_burn,
    // debit on unlock_or_mint; chain != token_chain ⇒ reversed); the only
    // observable difference is that we surface overflow and underflow as
    // distinct error codes so on-chain logs disambiguate without re-decoding
    // the payload.

    fn balance_with(chain: u16, token_chain: u16, balance: Uint256) -> BalanceAccountLayout {
        // Token address is irrelevant for the lock/unlock arithmetic; pin a
        // recognisable byte pattern so a stray off-by-one in a later test
        // surfaces clearly in the diff.
        let mut token_address = [0u8; 32];
        token_address[0] = 0x62;
        token_address[31] = 0x61;
        BalanceAccountLayout {
            chain,
            token_chain,
            token_address,
            balance,
            _reserved: [0u8; 8],
        }
    }

    #[test]
    fn lock_or_burn_native_chain_credits() {
        // chain == token_chain ⇒ lock_or_burn is the native-side credit.
        // Port of CosmWasm `native_lock` (500 + 200 = 700).
        let mut acc = balance_with(0xbae2, 0xbae2, Uint256::from_u128(500));
        acc.lock_or_burn(Uint256::from_u128(200)).unwrap();
        assert_eq!(acc.balance, Uint256::from_u128(700));
    }

    #[test]
    fn lock_or_burn_wrapped_chain_debits() {
        // chain != token_chain ⇒ lock_or_burn is the wrapped-side debit.
        // Port of CosmWasm `wrapped_burn` (500 - 200 = 300).
        let mut acc = balance_with(0xcae8, 0xbae2, Uint256::from_u128(500));
        acc.lock_or_burn(Uint256::from_u128(200)).unwrap();
        assert_eq!(acc.balance, Uint256::from_u128(300));
    }

    #[test]
    fn lock_or_burn_wrapped_chain_underflow_rejects() {
        // Port of CosmWasm `wrapped_burn_underflow`. Underflow ⇒ BalanceUnderflow.
        let mut acc = balance_with(0xcae8, 0xbae2, Uint256::ZERO);
        let err = acc.lock_or_burn(Uint256::from_u128(200)).unwrap_err();
        assert_eq!(err, GlobalAccountantError::BalanceUnderflow);
        assert_eq!(acc.balance, Uint256::ZERO, "balance unchanged on error");
    }

    #[test]
    fn lock_or_burn_native_chain_overflow_rejects() {
        // Port of CosmWasm `native_lock_overflow`. Overflow ⇒ BalanceOverflow.
        let mut acc = balance_with(0xbae2, 0xbae2, Uint256::MAX);
        let err = acc.lock_or_burn(Uint256::from_u128(200)).unwrap_err();
        assert_eq!(err, GlobalAccountantError::BalanceOverflow);
        assert_eq!(acc.balance, Uint256::MAX, "balance unchanged on error");
    }

    #[test]
    fn unlock_or_mint_native_chain_debits() {
        // chain == token_chain ⇒ unlock_or_mint is the native-side debit.
        // Port of CosmWasm `native_unlock` (500 - 200 = 300).
        let mut acc = balance_with(0xbae2, 0xbae2, Uint256::from_u128(500));
        acc.unlock_or_mint(Uint256::from_u128(200)).unwrap();
        assert_eq!(acc.balance, Uint256::from_u128(300));
    }

    #[test]
    fn unlock_or_mint_native_chain_underflow_rejects() {
        // Port of CosmWasm `native_unlock_underflow`. Underflow ⇒ BalanceUnderflow.
        let mut acc = balance_with(0xbae2, 0xbae2, Uint256::ZERO);
        let err = acc.unlock_or_mint(Uint256::from_u128(200)).unwrap_err();
        assert_eq!(err, GlobalAccountantError::BalanceUnderflow);
        assert_eq!(acc.balance, Uint256::ZERO);
    }

    #[test]
    fn unlock_or_mint_wrapped_chain_credits() {
        // chain != token_chain ⇒ unlock_or_mint is the wrapped-side credit.
        // Port of CosmWasm `wrapped_mint` (500 + 200 = 700).
        let mut acc = balance_with(0xcae8, 0xbae2, Uint256::from_u128(500));
        acc.unlock_or_mint(Uint256::from_u128(200)).unwrap();
        assert_eq!(acc.balance, Uint256::from_u128(700));
    }

    #[test]
    fn unlock_or_mint_wrapped_chain_overflow_rejects() {
        // Port of CosmWasm `wrapped_mint_overflow`. Overflow ⇒ BalanceOverflow.
        let mut acc = balance_with(0xcae8, 0xbae2, Uint256::MAX);
        let err = acc.unlock_or_mint(Uint256::from_u128(200)).unwrap_err();
        assert_eq!(err, GlobalAccountantError::BalanceOverflow);
        assert_eq!(acc.balance, Uint256::MAX);
    }

    // ---- parse_token_bridge_payload tests ----
    //
    // The body wire format is laid out in the function's doc comment. These
    // tests pin every branch of the parser at the byte level so a stray
    // offset shift or endian flip surfaces immediately.

    /// Build a fully-formed 184-byte VAA body (51-byte header + 133-byte
    /// Token Bridge transfer payload) in a stack array. Avoids dragging
    /// `alloc::vec` into this `no_std` crate just for tests.
    fn transfer_body(
        action: u8,
        amount: u128,
        token_address: [u8; 32],
        token_chain: u16,
        recipient_chain: u16,
    ) -> [u8; 184] {
        let mut body = [0u8; 184];
        // Header is zeroed; emitter_chain at offset 8..10 left at 0 since
        // the parser only reads it from the higher-level submit_observations
        // caller. The transfer payload starts at offset 51.
        body[51] = action;
        // amount: 32-byte BE Uint256, low 16 bytes hold the u128.
        body[52 + 16..52 + 32].copy_from_slice(&amount.to_be_bytes());
        body[84..116].copy_from_slice(&token_address);
        body[116..118].copy_from_slice(&token_chain.to_be_bytes());
        // recipient: 32 bytes, recognisable (118..150). Zero is fine; we set
        // a couple of bytes to catch off-by-one in case parser ever reads.
        body[118] = 0xAB;
        body[149] = 0xCD;
        body[150..152].copy_from_slice(&recipient_chain.to_be_bytes());
        // fee at 152..184 stays zero.
        body
    }

    #[test]
    fn parse_token_bridge_payload_transfer_decodes_amount_token_recipient() {
        let mut token_address = [0u8; 32];
        token_address[0] = 0x11;
        token_address[31] = 0x99;
        let body = transfer_body(
            0x01,
            1_000_000_u128,
            token_address,
            2,  // token_chain = Ethereum (placeholder)
            10, // recipient_chain = Solana (placeholder)
        );
        let action = parse_token_bridge_payload(&body).expect("transfer parses");
        match action {
            TokenBridgeAction::Transfer {
                amount,
                token_chain,
                token_address: ta,
                recipient_chain,
            } => {
                assert_eq!(amount, Uint256::from_u128(1_000_000));
                assert_eq!(token_chain, 2);
                assert_eq!(ta, token_address);
                assert_eq!(recipient_chain, 10);
            }
            other => panic!("expected Transfer, got {other:?}"),
        }
    }

    #[test]
    fn parse_token_bridge_payload_transfer_with_payload_same_as_transfer() {
        // Action 0x03 must collapse to the same Transfer variant — accountant
        // logic is identical, only the on-the-wire `extra` bytes differ.
        let token_address = [0x42u8; 32];
        let body_01 = transfer_body(0x01, 99, token_address, 5, 7);
        let body_03 = transfer_body(0x03, 99, token_address, 5, 7);
        let a = parse_token_bridge_payload(&body_01).unwrap();
        let b = parse_token_bridge_payload(&body_03).unwrap();
        assert_eq!(a, b, "action 0x01 and 0x03 must decode identically");
    }

    #[test]
    fn parse_token_bridge_payload_attest() {
        // Action 0x02 — the body need only carry the one-byte action past the
        // 51-byte header; CosmWasm does the same (attestations carry
        // symbol/name/decimals which the accountant ignores).
        let mut body = [0u8; 52];
        body[51] = 0x02;
        let action = parse_token_bridge_payload(&body).expect("attest parses");
        assert_eq!(action, TokenBridgeAction::Attest);
    }

    #[test]
    fn parse_token_bridge_payload_unknown_action() {
        // Any byte that is not 0x01, 0x02, or 0x03 collapses to Other —
        // CosmWasm `bail!`s; the accountant on Solana finishes the commit
        // without balance work to keep the NoReplay flip atomic.
        let mut body = [0u8; 52];
        body[51] = 0x77;
        let action = parse_token_bridge_payload(&body).expect("unknown action parses");
        assert_eq!(action, TokenBridgeAction::Other);
    }

    #[test]
    fn parse_token_bridge_payload_short_body_rejects() {
        // Body exactly the 51-byte header length (no action byte) must reject
        // — the parser cannot read the action byte.
        let body = [0u8; 51];
        let err = parse_token_bridge_payload(&body).unwrap_err();
        assert_eq!(err, GlobalAccountantError::InvalidInstructionData);
    }

    #[test]
    fn parse_token_bridge_payload_short_transfer_payload_rejects() {
        // Body has the 51-byte header + action 0x01 + 10 trailing bytes —
        // far short of the 133-byte transfer payload minimum.
        let mut body = [0u8; 62];
        body[51] = 0x01;
        let err = parse_token_bridge_payload(&body).unwrap_err();
        assert_eq!(err, GlobalAccountantError::InvalidInstructionData);
    }
}
