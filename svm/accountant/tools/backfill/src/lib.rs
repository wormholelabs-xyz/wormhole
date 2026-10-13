//! Off-chain orchestrator for the wormchain → SVM Global Accountant migration.
//!
//! Reads the deterministic wormchain snapshot (`svm_accountant_tools_common::snapshot`)
//! and drives the on-chain backfill program at scale.

pub mod batcher;
pub mod onchain_check;
pub mod progress;
pub mod run;
pub mod snapshot_balance_check;
pub mod stats;
pub mod submitter;
pub mod target;
pub mod tx_builder;
pub mod wire;
