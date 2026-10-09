//! Wormchain ↔ SVM accountant parity checker.
//!
//! Compares the account state the backfill writes (Balance, NoReplay marks,
//! ModifyBalance and RegisterChain records, chain registrations, and on NTT the
//! transceiver hub and peer maps) between a wormchain snapshot and the
//! deployed accountant program. Comparison logic (`state`, `compare`,
//! `ledger`) is pure; `expected_state` (from the snapshot) and
//! `onchain_state` (from Solana) produce the two `ChainState`s it compares.

pub mod compare;
pub mod compared_account;
pub mod expected_state;
pub mod ledger;
pub mod onchain_state;
pub mod rpc;
pub mod state;
pub mod time;
