//! Per-program instruction discriminators, one submodule per program.

pub mod backfill_ix_data;
pub mod global_accountant;
pub mod global_accountant_backfill;
pub mod ix_data;
pub mod ntt_global_accountant;
pub mod ntt_global_accountant_backfill;

pub use backfill_ix_data::*;
pub use global_accountant::*;
pub use ix_data::*;
