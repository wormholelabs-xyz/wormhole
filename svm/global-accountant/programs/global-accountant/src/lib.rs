//! Wormhole Global Accountant — Solana port (Pinocchio).

#![cfg_attr(target_os = "solana", no_std)]
// `target_os = "solana"` is provided by the SBF toolchain; the host toolchain
// flags it as an unexpected cfg value.
#![allow(unexpected_cfgs)]

// Paired-feature fence: `mock-vaa`, `test-only-open-digest`, and
// `mock-noreplay` are all "this is a test build" signals. They swap the real
// Verify VAA Shim CPI in `close_digest`, re-expose `open_digest` outside of
// `submit_observations`, and substitute the real `solana-noreplay` CPI for an
// in-memory sentinel respectively. None of them is independently meaningful:
// shipping any subset gives a build that mocks one piece of the quorum
// pipeline while exposing the production-shape pieces of the others — a stuck
// state for any real deployment. Force all three to travel together so the
// only reachable shapes are "all three on" (mollusk / surfpool spike) and
// "all three off" (production: real CPIs everywhere, no public
// `open_digest`).
#[cfg(any(
    all(
        feature = "mock-vaa",
        any(not(feature = "test-only-open-digest"), not(feature = "mock-noreplay"),)
    ),
    all(
        feature = "test-only-open-digest",
        any(not(feature = "mock-vaa"), not(feature = "mock-noreplay"))
    ),
    all(
        feature = "mock-noreplay",
        any(not(feature = "mock-vaa"), not(feature = "test-only-open-digest"),)
    ),
))]
compile_error!(
    "`mock-vaa`, `test-only-open-digest`, and `mock-noreplay` are paired \
     test-build features; enable all three or none"
);

pub mod entrypoint;
pub(crate) mod hash;
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
