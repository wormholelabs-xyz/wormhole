//! On-chain state layouts.
//!
//! `digest` is the per-(chain, emitter, sequence) PDA created on quorum-reach.
//! `pending` is the per-(chain, emitter, sequence, digest) bucket that
//! accumulates guardian signatures up to quorum. `account` is the per-(chain,
//! token_chain, token_address) balance ledger — the CosmWasm `Account` record's
//! Solana home — touched by the quorum-completing branch of
//! `submit_observations`.

pub mod account;
pub mod chain_registration;
pub mod digest;
pub mod modification;
pub mod pending;
