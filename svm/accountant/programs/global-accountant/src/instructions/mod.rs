//! WTT-specific instruction handlers.
//!
//! `register_chain` and `modify_balance` are the Token Bridge / Accountant
//! governance paths; `transfer` is the Token Bridge balance applicator the
//! entrypoint injects into the core `submit_observations` / `submit_vaas`
//! handlers. All product-neutral handlers live in
//! `accountant-operational-core`.

pub mod modify_balance;
pub mod register_chain;
pub mod transfer;
