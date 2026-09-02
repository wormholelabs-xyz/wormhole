//! Shared types and constants for the Wormhole Global Accountant Solana program.
//!
//! Solana-SDK-free, so on-chain code, host tests, and client tooling share the layouts.
//! Every public item of the submodules is re-exported at the crate root.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod constants;
pub mod error;
pub mod governance;
pub mod instructions;
pub mod ntt;
pub mod primitives;
pub mod state;
pub mod vaa;

pub use constants::*;
pub use error::*;
pub use governance::*;
pub use instructions::*;
pub use ntt::*;
pub use primitives::*;
pub use state::*;
pub use vaa::*;
