//! Parity comparison between the snapshot and the deployed program.
//!
//! `state`, `compare` and `findings::collect_findings` are pure;
//! `expected_state` (from the snapshot) and `onchain_state` (from Solana)
//! produce the two `ChainState`s they compare.

pub mod compare;
pub mod compared_account;
pub mod expected_state;
pub mod findings;
pub mod onchain_state;
pub mod state;
