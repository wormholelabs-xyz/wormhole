//! Shared types and constants for the Wormhole Global Accountant Solana program.
//!
//! No Solana dependency so the layouts can be re-used from on-chain code,
//! host-side tests, and client tooling.
//!
//! The surface is split into domain modules — [`instruction`], [`error`],
//! [`primitives`], [`constants`], [`state`], and [`vaa`] — but every public
//! item is re-exported here, so consumers continue to reach them at the crate
//! root (`global_accountant_definitions::<Name>`).

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
