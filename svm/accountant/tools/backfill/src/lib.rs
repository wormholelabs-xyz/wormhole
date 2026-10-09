//! Off-chain orchestrator for the wormchain → SVM Global Accountant migration.
//!
//! Reads the deterministic snapshot produced by `tools/wormchain-snapshot/`
//! and drives the on-chain backfill program at scale.

pub mod accountant;
pub mod accounts;
pub mod batcher;
pub mod expected_accounts;
pub mod noreplay;
pub mod onchain_check;
pub mod progress;
pub mod reconcile;
pub mod snapshot;
pub mod snapshot_balance_check;
pub mod stats;
pub mod submitter;
pub mod tx_builder;
