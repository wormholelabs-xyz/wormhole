//! Shared on-chain state layouts.
//!
//! - `pending`: per-(chain, emitter, sequence, digest) signature-accumulation
//!   bucket.
//! - `account`: per-(chain, token_chain, token_address) balance ledger.

pub mod account;
pub mod pending;
