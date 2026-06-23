//! WTT-specific instruction handlers.
//!
//! All instruction implementations live here: the quorum tracker
//! (`submit_observations`), signed-VAA backfill (`submit_vaas`), governance
//! paths (`register_chain`, `modify_balance`), and the balance applicator
//! (`transfer`). Only the permissionless cleanup handler (`close_pending`)
//! lives in `accountant-operational-core` since it is shared with NTT.

pub mod modify_balance;
pub mod register_chain;
pub mod submit_observations;
pub mod submit_vaas;
pub mod transfer;
