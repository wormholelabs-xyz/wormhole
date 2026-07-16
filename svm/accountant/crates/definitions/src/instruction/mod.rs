//! Per-program instruction discriminators.
//!
//! One submodule per on-chain program — separate program IDs mean separate
//! dispatch tables. The WTT program's discriminators are re-exported flat for
//! crate-root access (`global_accountant_definitions::Instruction`). The NTT
//! program's set lands as its own submodule alongside its first consumer,
//! namespaced to avoid the duplicate `Instruction` name.

pub mod global_accountant;

pub use global_accountant::*;
