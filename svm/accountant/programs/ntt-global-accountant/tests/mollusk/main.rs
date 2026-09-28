//! Mollusk suite: in-process runs of the NTT accountant against the pinned NoReplay and
//! Verify VAA Shim binaries. `just test` builds the `.so` and runs this binary.

#[path = "../common/mod.rs"]
mod common;

mod close_pending;
mod cross_path_replay;
mod modify_balance;
mod program_id;
mod register_hub;
mod register_peer;
mod register_relayer_chain;
mod submit_observations;
mod submit_vaas;
mod upgrade_contract;
