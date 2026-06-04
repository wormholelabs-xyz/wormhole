//! Instruction handlers.
//!
//! DigestAccount opens go through `open_digest::open_digest_inner`, shared by
//! `submit_observations` (on quorum), `submit_vaas` (after Shim verification),
//! and the test-only `test_only_open_digest` entrypoint.

pub mod close_digest;
pub mod close_pending;
pub mod modify_balance;
pub mod noreplay;
pub(crate) mod open_digest;
pub mod pda_init;
pub mod register_chain;
pub(crate) mod shim;
pub mod submit_observations;
pub mod submit_vaas;
// Test-only open entrypoint. The module body `compile_error!`s without the
// feature; gating the `mod` here means prod builds never parse it.
#[cfg(feature = "test-only-open-digest")]
pub mod test_only_open_digest;
pub mod transfer;
