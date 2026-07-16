//! Off-chain orchestrator for the wormchain → SVM Global Accountant migration.
//!
//! Reads the deterministic catalogue produced by `tools/wormchain-snapshot/`
//! and drives the on-chain backfill program at scale.

pub mod balance_reconcile;
pub mod catalogue;
pub mod chunker;
pub mod cursor;
pub mod reconcile;
pub mod stats;
pub mod submitter;
pub mod tx_builder;
