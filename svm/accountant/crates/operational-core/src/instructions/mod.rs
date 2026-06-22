//! Operational instruction handlers.
//!
//! The canonical digest record is emitted via `commit_log::emit` on the
//! quorum-completing branch of `submit_observations` and on every successful
//! `submit_vaas`. Off-chain indexers consume the program-log line carrying
//! the [`crate::definitions::ACCOUNTANT_DIGEST_LOG_TAG`] prefix.

pub mod close_pending;
pub mod commit_log;
pub mod noreplay;
pub mod pda_init;
pub mod shim;
pub mod submit_observations;
pub mod submit_vaas;
