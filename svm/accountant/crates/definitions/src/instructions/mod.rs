//! Per-program instruction discriminators, one submodule per program.

pub mod global_accountant;
pub mod ix_data;
pub mod ntt_global_accountant;

pub use global_accountant::*;
pub use ix_data::*;
