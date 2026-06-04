//! Wormhole Global Accountant — Solana port (Pinocchio).

// The `target_arch = "bpf"` arm keeps the crate `no_std` under upstream
// rustc's `bpfel-unknown-none` target, which is what lets nightly clippy lint
// the SBF-shaped code (`just clippy-sbf`) — the platform-tools toolchain
// ships no clippy of its own.
#![cfg_attr(any(target_os = "solana", target_arch = "bpf"), no_std)]
// `target_os = "solana"` is provided by the SBF toolchain; the host toolchain
// flags it as an unexpected cfg value.
#![allow(unexpected_cfgs)]

// `test-only-open-digest` exposes the `TestOnlyOpenDigest` arm in the dispatch table
// (handled by `test_only_open_digest`) for mollusk tests that drive it directly.
// The default (no features) build keeps the open path reachable only from
// inside `submit_observations` and `submit_vaas`, after the NoReplay check. This is the only remaining test-build feature; the
// historic `mock-vaa` / `mock-noreplay` pair has been replaced by sibling
// fixture programs loaded into mollusk at the canonical IDs (see
// `tests/common/mollusk_fixtures.rs`).

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
/// test build (mollusk drives `test_only_open_digest` directly) and **off** for the
/// production build (the open path is only reachable from inside
/// `submit_observations` and `submit_vaas`, after the NoReplay check).
pub const TEST_ONLY_OPEN_DIGEST_ENABLED: bool = cfg!(feature = "test-only-open-digest");
