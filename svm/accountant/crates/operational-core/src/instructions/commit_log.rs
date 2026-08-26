//! Commit-log emit. Layout: see [`ACCOUNTANT_DIGEST_LOG_TAG`].

use crate::definitions::{ACCOUNTANT_DIGEST_LOG_LEN, ACCOUNTANT_DIGEST_LOG_TAG};

/// Emit one commit log entry through `sol_log_data`.
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
