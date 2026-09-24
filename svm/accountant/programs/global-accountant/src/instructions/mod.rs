//! WTT-specific instruction handlers. `close_pending` lives in `accountant-operational-core`.

pub mod modify_balance;
pub mod register_chain;
pub mod submit_observations;
pub mod submit_vaas;
pub mod transfer;
pub mod upgrade_contract;
