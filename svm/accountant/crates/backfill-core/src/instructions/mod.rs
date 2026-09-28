//! One handler module per backfill arm. Each takes a positional `&[AccountInfo]`, the raw
//! instruction payload and the program's compile-time authority, so the two program shells
//! share one implementation and differ only in program id and operator key.
//!
//! Every handler follows the same order: account framing, `require_authority`, wire parse,
//! account-count check, then per-entry PDA check and write.

pub mod backfill_balance;
pub mod backfill_chain_registration;
pub mod backfill_modify_balance;
pub mod backfill_noreplay;
pub mod backfill_transceiver_hub;
pub mod backfill_transceiver_peer;
