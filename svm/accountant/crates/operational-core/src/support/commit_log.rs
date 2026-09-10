//! Commit-log emit. Layout: [`AccountantDigestLog`].

use crate::definitions::AccountantDigestLog;

/// Emit one commit log entry through `sol_log_data`.
pub fn emit(
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    digest: &[u8; 32],
    guardian_set_index: u32,
) {
    let entry = AccountantDigestLog::new(chain, *emitter, sequence, *digest, guardian_set_index);
    anchor_lang::solana_program::log::sol_log_data(&[entry.as_bytes()]);
}
