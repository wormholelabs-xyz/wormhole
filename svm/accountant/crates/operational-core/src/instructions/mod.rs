//! Instruction handlers shared verbatim by both accountant programs. Governance handlers
//! take the caller's [`crate::definitions::GovernanceModule`].

pub mod close_pending;
pub mod modify_balance;
pub mod register_chain;
pub mod upgrade_contract;
