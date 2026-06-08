//! Canonical commit-log emit — byte-identical to the operational program's
//! `commit_log::emit`. Duplicated here intentionally: lifting it into the
//! shared `definitions` crate would require adding a `pinocchio` dependency
//! there, breaking that crate's Solana-SDK-free invariant. A future cleanup
//! could introduce a `crates/program-shared/` crate; for now duplication
//! keeps the dependency graph clean.

use crate::definitions::{ACCOUNTANT_DIGEST_LOG_LEN, ACCOUNTANT_DIGEST_LOG_TAG};

/// Emit one canonical commit-log entry via `sol_log_data`. Host build is a
/// no-op so mollusk and clippy continue to link.
pub(crate) fn emit(
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    digest: &[u8; 32],
    guardian_set_index: u32,
) {
    let mut buf = [0u8; ACCOUNTANT_DIGEST_LOG_LEN];
    buf[..8].copy_from_slice(&ACCOUNTANT_DIGEST_LOG_TAG);
    buf[8..10].copy_from_slice(&chain.to_be_bytes());
    buf[10..42].copy_from_slice(emitter);
    buf[42..50].copy_from_slice(&sequence.to_be_bytes());
    buf[50..82].copy_from_slice(digest);
    buf[82..86].copy_from_slice(&guardian_set_index.to_le_bytes());

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
