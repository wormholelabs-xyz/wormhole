//! NTT-specific instruction handlers.
//!
//! `register_relayer_chain` validates a `WormholeRelayer` governance VAA and
//! writes the canonical `RelayerChainRegistration` PDA; `modify_balance` is the
//! NTT accountant governance path (identical to the WTT handler modulo the
//! governance module string). All product-neutral handlers live in
//! `accountant-operational-core`.

pub mod modify_balance;
pub mod ntt_transfer;
pub mod register_relayer_chain;
pub mod submit_observations;
pub mod submit_vaas;
