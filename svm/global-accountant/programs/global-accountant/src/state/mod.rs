//! On-chain state layouts.
//!
//! - `digest`: per-(chain, emitter, sequence) PDA created on quorum-reach.
//! - `pending`: per-(chain, emitter, sequence, digest) signature-accumulation
//!   bucket.
//! - `account`: per-(chain, token_chain, token_address) balance ledger.
//! - `chain_registration`, `modification`: governance-path state.

pub mod account;
pub mod chain_registration;
pub mod digest;
pub mod modification;
pub mod pending;
