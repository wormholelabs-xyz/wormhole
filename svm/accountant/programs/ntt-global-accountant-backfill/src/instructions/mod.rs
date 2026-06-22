//! NTT-native backfill instruction handlers — relayer registry, transceiver
//! hub, and transceiver peer maps. Each mirrors `accountant_backfill_core`'s
//! `backfill_balance`: parse a sorted batch, authority-gate, then per entry
//! verify the sort, derive the canonical PDA, allocate, and write the tagged
//! layout. The shared NoReplay/Balance handlers live in the core crate.

pub mod backfill_relayer_registration;
pub mod backfill_transceiver_hub;
pub mod backfill_transceiver_peer;
