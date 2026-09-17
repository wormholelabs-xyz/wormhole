//! WTT-specific instruction handlers. `close_pending`, `register_chain`, `modify_balance`
//! and `upgrade_contract` live in `accountant-operational-core`.

pub mod submit_observations;
pub mod submit_vaas;
pub mod transfer;
