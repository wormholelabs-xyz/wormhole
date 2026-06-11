//! Canonical commit-log emit — byte-identical to the operational program's
//! `commit_log::emit`. Duplicated here intentionally: lifting it into the
//! shared `definitions` crate would require adding a `pinocchio` dependency
//! there, breaking that crate's Solana-SDK-free invariant. A future cleanup
//! could introduce a `crates/program-shared/` crate; for now duplication
//! keeps the dependency graph clean.

use crate::definitions::{ACCOUNTANT_DIGEST_LOG_LEN, ACCOUNTANT_DIGEST_LOG_TAG};

/// Build the 86-byte canonical commit-log payload. Split out from `emit` so
/// host-side unit tests can pin the byte layout without spinning up the SBF VM.
pub(crate) fn build_payload(
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    digest: &[u8; 32],
    guardian_set_index: u32,
) -> [u8; ACCOUNTANT_DIGEST_LOG_LEN] {
    let mut buf = [0u8; ACCOUNTANT_DIGEST_LOG_LEN];
    buf[..8].copy_from_slice(&ACCOUNTANT_DIGEST_LOG_TAG);
    buf[8..10].copy_from_slice(&chain.to_be_bytes());
    buf[10..42].copy_from_slice(emitter);
    buf[42..50].copy_from_slice(&sequence.to_be_bytes());
    buf[50..82].copy_from_slice(digest);
    buf[82..86].copy_from_slice(&guardian_set_index.to_le_bytes());
    buf
}

/// Emit one canonical commit-log entry via `sol_log_data`. Host build is a
/// no-op so mollusk and clippy continue to link.
pub(crate) fn emit(
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    digest: &[u8; 32],
    guardian_set_index: u32,
) {
    let buf = build_payload(chain, emitter, sequence, digest, guardian_set_index);
    log_data(&buf);
}

#[cfg(any(target_os = "solana", target_arch = "bpf"))]
fn log_data(buf: &[u8]) {
    let slices: [&[u8]; 1] = [buf];
    // SAFETY: `sol_log_data` reads exactly `slices.len()` fat-pointer slice
    // references starting at `slices.as_ptr()`. The buffer outlives the call.
    unsafe {
        pinocchio::syscalls::sol_log_data(slices.as_ptr() as *const u8, slices.len() as u64);
    }
}

#[cfg(not(any(target_os = "solana", target_arch = "bpf")))]
fn log_data(_buf: &[u8]) {}

#[cfg(all(test, not(any(target_os = "solana", target_arch = "bpf"))))]
mod tests {
    use super::*;

    /// Pin the 86-byte canonical layout. Surfpool e2e will later confirm the
    /// buffer is actually emitted via `sol_log_data` and recoverable from
    /// `meta.logMessages`; this fast unit test just guards against drift in
    /// the byte offsets.
    #[test]
    fn build_payload_matches_canonical_layout() {
        let chain = 0xABCDu16;
        let emitter = [0x11u8; 32];
        let sequence = 0x0102_0304_0506_0708u64;
        let digest = [0x77u8; 32];
        let gsi = 0u32;

        let buf = build_payload(chain, &emitter, sequence, &digest, gsi);

        assert_eq!(&buf[..8], &ACCOUNTANT_DIGEST_LOG_TAG);
        assert_eq!(&buf[8..10], &chain.to_be_bytes());
        assert_eq!(&buf[10..42], &emitter);
        assert_eq!(&buf[42..50], &sequence.to_be_bytes());
        assert_eq!(&buf[50..82], &digest);
        assert_eq!(&buf[82..86], &gsi.to_le_bytes());
        assert_eq!(buf.len(), ACCOUNTANT_DIGEST_LOG_LEN);
    }
}
