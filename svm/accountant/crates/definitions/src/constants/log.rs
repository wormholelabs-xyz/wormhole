//! Commit-log tag and entry size.

/// 8-byte tag on every commit log entry. Emitted on quorum in `submit_observations`
/// and on every successful `submit_vaas`. Off-chain indexers filter on this prefix.
///
/// Entry layout (86 bytes):
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
/// `guardian_set_index` is the quorum set on the observations path; `submit_vaas` writes `0`.
pub const ACCOUNTANT_DIGEST_LOG_TAG: [u8; 8] = *b"ACCDGST\0";

/// Commit log entry length (tag + payload).
pub const ACCOUNTANT_DIGEST_LOG_LEN: usize = 8 + 2 + 32 + 8 + 32 + 4;
