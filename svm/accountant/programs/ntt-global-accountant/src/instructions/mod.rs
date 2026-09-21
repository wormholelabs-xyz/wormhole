//! NTT-specific instruction handlers. The governance handlers and `close_pending` live in
//! `accountant-operational-core`.

pub mod ntt_transfer;
pub mod register_hub;
pub mod register_peer;
pub mod sender;
pub mod submit_vaas;
