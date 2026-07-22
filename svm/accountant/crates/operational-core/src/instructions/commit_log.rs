//! Canonical commit-log emit, shared by `submit_observations` (on quorum) and
//! `submit_vaas` (after Shim verification).
//!
//! Off-chain indexers consume the program-log line carrying the
//! [`ACCOUNTANT_DIGEST_LOG_TAG`] prefix. The payload layout is the single
//! source of truth in [`crate::definitions`]; this module merely marshals
//! caller-supplied fields into the 86-byte buffer and hands it to
//! `sol_log_data`.
//!
//! `anchor_lang::solana_program::log::sol_log_data` has a real on-chain arm
//! (the syscall) and a host no-op arm built in, so — unlike pinocchio's raw
//! syscall wrapper — no cfg-gating is needed on our side.

use crate::definitions::{ACCOUNTANT_DIGEST_LOG_LEN, ACCOUNTANT_DIGEST_LOG_TAG};

/// Emit one canonical commit log entry.
pub fn emit(
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

    anchor_lang::solana_program::log::sol_log_data(&[&buf]);
}
