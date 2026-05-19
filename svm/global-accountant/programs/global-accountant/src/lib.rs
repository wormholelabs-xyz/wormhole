//! Wormhole Global Accountant — Solana port (Pinocchio).
//!
//! Scope: digest-account open/close lifecycle. Balance accounting,
//! quorum logic, NoReplay, and Verify VAA Shim CPI live in later slices.
//! See `.claude/tasks/accountant-migration.md`.

#![cfg_attr(target_os = "solana", no_std)]
// `target_os = "solana"` is provided by the SBF toolchain; the host toolchain
// flags it as an unexpected cfg value.
#![allow(unexpected_cfgs)]

// Paired-feature fence: `mock-vaa` and `test-only-open-digest` are both
// "this is a test build" signals (one swaps the real Verify VAA Shim CPI in
// `close_digest` for an instruction-data shortcut, the other re-exposes
// `open_digest` outside of `submit_observations`). They are not independently
// meaningful: shipping `mock-vaa` without `test-only-open-digest` gives a
// build that mocks the VAA verification but offers no way to populate a
// digest PDA from a test, and shipping `test-only-open-digest` without
// `mock-vaa` exposes the test-only `open_digest` entrypoint against a
// production-shape close path that demands a real VAA quorum to ever close
// the PDA again — both shapes are stuck states for a real deployment. Force
// the features to travel together so the only reachable shapes are
// "both on" (mollusk / surfpool spike) and "both off" (production: real CPI
// in `close_digest`, no public `open_digest`).
#[cfg(any(
    all(feature = "mock-vaa", not(feature = "test-only-open-digest")),
    all(feature = "test-only-open-digest", not(feature = "mock-vaa")),
))]
compile_error!(
    "`mock-vaa` and `test-only-open-digest` are paired test-build features; \
     enable both or neither"
);

pub mod entrypoint;
pub mod instructions;
pub mod state;

pub use global_accountant_definitions as definitions;

use pinocchio::error::ProgramError;

use crate::definitions::GlobalAccountantError;

/// Convert a `GlobalAccountantError` into a `ProgramError::Custom`.
///
/// Lives here rather than as `impl From<GlobalAccountantError> for ProgramError`
/// because both types are foreign to the program crate (the error is owned by
/// `global-accountant-definitions`; the program-error type is owned by
/// `pinocchio`), so the orphan rules forbid the impl. Moving the helper into
/// `definitions` would force a `pinocchio` dependency on that crate — which
/// the design intentionally avoids so the layouts can be re-used from
/// non-Solana tooling (`crates/definitions/src/lib.rs` is `no_std` and Solana-
/// SDK-free).
#[inline]
pub(crate) fn err(e: GlobalAccountantError) -> ProgramError {
    ProgramError::Custom(e as u32)
}

/// Compile-time pin on the `test-only-open-digest` Cargo feature. Read by the
/// integration test crate via `cargo test`; the feature must be **on** for the
/// test build (mollusk drives `open_digest` directly) and **off** for the
/// production build (`open_digest` is only reachable from inside
/// `submit_observations` after the NoReplay check).
pub const TEST_ONLY_OPEN_DIGEST_ENABLED: bool = cfg!(feature = "test-only-open-digest");
