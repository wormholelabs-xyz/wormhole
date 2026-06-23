//! Protocol constants: log tags, PDA seed prefixes, governance identifiers,
//! external program IDs, and the NoReplay wire format.
//!
//! Organised into one submodule per source/concern (external programs each get
//! their own). Every item is re-exported flat here, so consumers continue to
//! reach them at the crate root (`global_accountant_definitions::<NAME>`).

pub mod core_bridge;
pub mod governance;
pub mod log;
pub mod noreplay;
pub mod seeds;
pub mod verify_vaa_shim;

pub use core_bridge::*;
pub use governance::*;
pub use log::*;
pub use noreplay::*;
pub use seeds::*;
pub use verify_vaa_shim::*;
