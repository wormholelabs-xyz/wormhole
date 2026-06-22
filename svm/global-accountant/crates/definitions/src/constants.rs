//! Protocol constants: log tags, PDA seed prefixes, governance identifiers,
//! external program IDs, the NoReplay wire format, and compute-unit tuning.

use crate::primitives::Pubkey;

// ---- Logging ----

/// 8-byte tag prefixing every accountant commit log entry. Off-chain indexers
/// filter program logs for this prefix to find the canonical
/// `(chain, emitter, sequence, digest, guardian_set_index)` record emitted on
/// the quorum-completing branch of `submit_observations` and on every
/// successful `submit_vaas`.
///
/// The log payload layout (86 bytes total) is:
///
/// | offset | size | field                              |
/// |--------|------|------------------------------------|
/// | 0      | 8    | `ACCOUNTANT_DIGEST_LOG_TAG`        |
/// | 8      | 2    | chain (big endian)                 |
/// | 10     | 32   | emitter                            |
/// | 42     | 8    | sequence (big endian)              |
/// | 50     | 32   | digest                             |
/// | 82     | 4    | guardian_set_index (little endian) |
///
/// `guardian_set_index` is the set that reached quorum on the observations
/// path; `submit_vaas` records the sentinel `0` (the Shim accepts any
/// currently-active set, so no single index meaningfully describes the
/// authorisation).
pub const ACCOUNTANT_DIGEST_LOG_TAG: [u8; 8] = *b"ACCDGST\0";

/// Total byte length of an emitted commit log entry (tag + payload).
pub const ACCOUNTANT_DIGEST_LOG_LEN: usize = 8 + 2 + 32 + 8 + 32 + 4;

// ---- PDA seed prefixes ----

/// PDA seed prefix for [`crate::PendingObservationsLayout`]. Full tuple:
/// `(b"pending", chain_be, emitter, sequence_be, digest)`. The digest suffix
/// lets fork/reorg observations accumulate in sibling buckets and binds each
/// bucket to its digest, so no runtime digest-equality check is needed.
pub const PENDING_OBSERVATIONS_SEED_PREFIX: &[u8] = b"pending";

/// PDA seed prefix for [`crate::BalanceAccountLayout`]. Full tuple:
/// `(b"account", chain_be, token_chain_be, token_address)`. Big-endian chain
/// fields match the VAA wire format and the other seed derivations.
pub const ACCOUNT_SEED_PREFIX: &[u8] = b"account";

/// PDA seed prefix for [`crate::ChainRegistrationLayout`]. Full tuple:
/// `(b"chain_registration", chain_be)`.
pub const CHAIN_REGISTRATION_SEED_PREFIX: &[u8] = b"chain_registration";

/// PDA seed prefix for [`crate::ModificationLayout`]. Full tuple:
/// `(b"modification", sequence_be)`. Existence of this PDA enforces replay
/// protection on the governance path.
pub const MODIFICATION_SEED_PREFIX: &[u8] = b"modification";

/// PDA seed prefix for the global-accountant authority that signs all
/// `solana-noreplay` CPIs. Full tuple: `[b"noreplay-authority"]`. One global
/// authority suffices because the noreplay namespace (`chain_be ‖ emitter`)
/// already segregates per-emitter sequence spaces.
pub const NOREPLAY_AUTHORITY_SEED_PREFIX: &[u8] = b"noreplay-authority";

/// PDA seed prefix for Core Bridge's GuardianSet accounts. Full tuple:
/// `(b"GuardianSet", guardian_set_index_be)`. Owned by CORE_BRIDGE_PROGRAM_ID.
pub const GUARDIAN_SET_SEED: &[u8] = b"GuardianSet";

// ---- Governance ----

/// Wormhole governance emitter — `chain = 1 (Solana)`, `address = [0; 31] ||
/// 0x04`. `register_chain` only accepts governance VAAs signed by this emitter.
pub const GOVERNANCE_EMITTER: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04,
];

/// Wormhole chain ID for Solana, also stamped on the governance emitter pair.
pub const SOLANA_CHAIN_ID: u16 = 1;

/// Wormhole chain ID for Wormchain. `register_chain` governance VAAs must
/// target either chain `0x0000` (Any) or this.
pub const WORMCHAIN_CHAIN_ID: u16 = 3104;

/// Token Bridge governance module — first 32 bytes of a Token Bridge
/// governance payload. "TokenBridge" right-aligned in 32 bytes.
pub const TOKEN_BRIDGE_GOVERNANCE_MODULE: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, b'T', b'o', b'k', b'e', b'n', b'B', b'r', b'i', b'd', b'g', b'e',
];

/// Token Bridge governance `RegisterChain` action byte.
pub const REGISTER_CHAIN_ACTION: u8 = 0x01;

/// Accountant governance module — first 32 bytes of a `ModifyBalance` payload.
/// "GlobalAccountant" right-aligned in 32 bytes. The action byte `0x01`
/// overlaps RegisterChain, so the module is what disambiguates the flows.
pub const ACCOUNTANT_GOVERNANCE_MODULE: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    b'G', b'l', b'o', b'b', b'a', b'l', b'A', b'c', b'c', b'o', b'u', b'n', b't', b'a', b'n', b't',
];

/// Accountant governance `ModifyBalance` action byte.
pub const MODIFY_BALANCE_ACTION: u8 = 0x01;

// ---- NoReplay (solana-noreplay) ----

/// Canonical program ID for `solana-noreplay`
/// (`repMHgR5BEpGLeZvM5iGoNNDPw4eu2BS6sXJzaC8K4t`). Raw bytes to keep this
/// crate Solana-SDK-free.
pub const NOREPLAY_PROGRAM_ID: Pubkey = [
    0x0c, 0xb8, 0x38, 0x00, 0x73, 0xdf, 0x36, 0x25, 0xa1, 0x32, 0x11, 0x1f, 0xee, 0x67, 0x8d, 0xd0,
    0x6b, 0x7e, 0x3d, 0xf2, 0x90, 0xa2, 0xb1, 0xd5, 0x4a, 0x48, 0x5b, 0xdb, 0x72, 0x61, 0x82, 0x91,
];

/// Discriminator for `solana-noreplay`'s `MarkUsed`. Wire format:
/// `[disc: u8][namespace_len: u16 LE][namespace: ≤64 B][sequence: u64 LE]`.
pub const NOREPLAY_MARK_USED_DISCRIMINATOR: u8 = 1;

/// Discriminator for `solana-noreplay`'s `MarkUsedBulk`. Wire format:
/// `[disc: u8][namespace_len: u16 LE][namespace: ≤64 B][bucket_index: u64 LE]
///  [or_mask: 128 B]`. Semantics: `bitmap |= or_mask` (OR-only; never clears
/// bits). Used by the backfill program to flip many bits per CPI, dropping
/// per-entry CU from ~3,000 to ~80 in the dense-bucket case.
///
/// Allocated as the next free byte in the noreplay program's dispatch table
/// after `CreateBitmap=0`, `MarkUsed=1`, `UnmarkUsed=2`.
pub const NOREPLAY_MARK_USED_BULK_DISCRIMINATOR: u8 = 3;

/// Bits per bitmap bucket. Bucket index is `sequence / BITS_PER_BUCKET`, bit
/// offset is `sequence % BITS_PER_BUCKET`.
pub const NOREPLAY_BITS_PER_BUCKET: u64 = 1024;

/// Bitmap payload size inside a noreplay PDA (account is 1-byte bump + bitmap).
pub const NOREPLAY_BITMAP_BYTES: usize = 128;

/// Byte offset of the bitmap payload (byte 0 is the stored canonical bump).
pub const NOREPLAY_BITMAP_OFFSET: usize = 1;

// ---- External program IDs ----

/// Wormhole Core Bridge program ID on Solana mainnet
/// (`worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth`). Raw bytes to keep this
/// crate Solana-SDK-free. Used by `close_pending` to verify the `GuardianSet`
/// account is Core-Bridge-owned before reading it — otherwise a forged
/// "expired" set could permanently DoS a pending PDA.
pub const CORE_BRIDGE_PROGRAM_ID: Pubkey = [
    0x0e, 0x0a, 0x58, 0x9a, 0x41, 0xa5, 0x5f, 0xbd, 0x66, 0xc5, 0x2a, 0x47, 0x5f, 0x2d, 0x92, 0xa6,
    0xd3, 0xdc, 0x9b, 0x47, 0x47, 0x11, 0x4c, 0xb9, 0xaf, 0x82, 0x5a, 0x98, 0xb5, 0x45, 0xd3, 0xce,
];

/// Verify VAA Shim program ID (`EFaNWErqAtVWufdNb7yofSHHfWFos843DFpu4JBw24at`),
/// same address on mainnet, devnet, and Tilt localnet. Raw bytes to keep this
/// crate Solana-SDK-free.
pub const VERIFY_VAA_SHIM_PROGRAM_ID: Pubkey = [
    196, 227, 203, 55, 17, 156, 166, 124, 168, 35, 28, 170, 3, 131, 164, 140, 195, 254, 137, 233,
    101, 80, 83, 225, 249, 25, 254, 66, 226, 131, 254, 161,
];

/// Anchor discriminator for the Verify VAA Shim's `verify_hash` — first 8 bytes
/// of `sha256("global:verify_hash")`.
pub const VERIFY_HASH_SELECTOR: [u8; 8] = [22, 152, 160, 69, 241, 148, 14, 124];

/// Wire size of `verify_hash` instruction data: 8-byte selector + 1-byte
/// guardian-set bump + 32-byte digest.
pub const VERIFY_HASH_DATA_LEN: usize = 8 + 1 + 32;

// ---- Compute-unit tuning ----

/// Compute-unit ceiling for the hottest `submit_observations` / `submit_vaas`
/// path (quorum commit branch with a Transfer + lazy-init of both Account
/// PDAs). Pinned by a regression test in `tests/submit_observations.rs`.
pub const MAX_QUORUM_BRANCH_CU: u64 = 80_000;
