//! Instruction handlers.
//!
//! Two production paths open a DigestAccount: `submit_observations` (on
//! quorum reach) and `submit_vaas` (after Shim verification).
//! `test_only_open_digest` stays in this module only behind `test-only-open-digest`
//! so the mollusk DigestAccount-lifecycle tests can drive that PDA directly.
//! All three share `open_digest::open_digest_inner` and
//! `pda_init::init_or_upgrade_pda` — keeping them as crate-private helpers
//! makes the open path a single function rather than near-duplicate code
//! paths.

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
// `test_only_open_digest` is the public quorum-init entrypoint exposed only in
// test builds; in prod the open path is only reachable from
// `submit_observations` after the NoReplay check. The module body itself
// refuses to compile without `test-only-open-digest` (file-top
// `compile_error!` mirrors the `mock-vaa` pattern in `close_digest.rs`), and
// we gate the `mod` declaration so prod builds do not even need to parse the
// file.
#[cfg(feature = "test-only-open-digest")]
pub mod test_only_open_digest;
pub mod transfer;
