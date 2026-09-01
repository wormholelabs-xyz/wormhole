//! Commit-log emit, byte-identical to the operational program's
//! `accountant_operational_core::support::commit_log::emit`. Layout: [`AccountantDigestLog`].
//!
//! Duplicated rather than shared: pulling in `accountant-operational-core` for this one
//! function would drag in its whole dependency graph and its `GlobalAccountantError` numbering,
//! against backfill-core's deliberately separate, smaller footprint (see `crate::BackfillError`'s
//! doc). `pda_init.rs` duplicates for the same reason.

use crate::definitions::AccountantDigestLog;

/// Emit one commit log entry through `sol_log_data`.
pub(crate) fn emit(
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    digest: &[u8; 32],
    guardian_set_index: u32,
) {
    let entry = AccountantDigestLog::new(chain, *emitter, sequence, *digest, guardian_set_index);
    anchor_lang::solana_program::log::sol_log_data(&[entry.as_bytes()]);
}
