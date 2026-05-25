//! On-chain state layouts.
//!
//! `digest` is the per-(chain, emitter, sequence) PDA created on quorum-reach.
//! The balance-account layout is re-exported from the definitions crate so the
//! on-chain shape is fixed before the balance-accounting slice lands; no
//! instructions touch it yet.

pub mod digest;
pub mod pending;

// Re-exported from `definitions` for ergonomic access from Phase 2 balance
// accounting; unused this slice.
pub use crate::definitions::BalanceAccountLayout;
