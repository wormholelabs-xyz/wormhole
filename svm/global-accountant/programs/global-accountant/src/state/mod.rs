//! On-chain state layouts.
//!
//! `digest` is the per-(chain, emitter, sequence) PDA created on quorum-reach.
//! `pending` is the per-(chain, emitter, sequence, digest) bucket that
//! accumulates guardian signatures up to quorum. `account` is the per-(chain,
//! token_chain, token_address) balance ledger — the CosmWasm `Account` record's
//! Solana home — touched by the quorum-completing branch of
//! `submit_observations`.

pub mod account;
pub mod digest;
pub mod pending;

// Re-exported from `definitions` for ergonomic access; the canonical
// layout/type lives in the SDK-free `definitions` crate so client tooling can
// re-use it without pulling in pinocchio.
pub use crate::definitions::BalanceAccountLayout;
