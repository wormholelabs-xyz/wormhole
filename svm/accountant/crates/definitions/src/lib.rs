//! Shared types and constants for the Wormhole Global Accountant Solana program.
//!
//! Solana-SDK-free, so on-chain code, host tests, and client tooling share the layouts.
//! Every public item of the submodules is re-exported at the crate root.

#![no_std]

pub mod constants;
pub mod error;
pub mod instruction;
pub mod primitives;
pub mod state;
pub mod vaa;

pub use constants::*;
pub use error::*;
pub use instruction::*;
pub use primitives::*;
pub use state::*;
pub use vaa::*;
