//! Off-chain commit-log tag and entry size emitted by the accountant programs.

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
