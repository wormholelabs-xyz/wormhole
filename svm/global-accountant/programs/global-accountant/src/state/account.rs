//! Re-export of the per-(chain, token_chain, token_address) balance layout.
//!
//! No instructions touch this yet — included so the on-chain layout is fixed
//! before the balance-accounting slice lands.

pub use crate::definitions::BalanceAccountLayout;
