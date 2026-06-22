//! Shared mollusk fixtures for the NTT operational program's integration tests.
//!
//! The sibling `.so` programs CPI'd into during a test (`solana_noreplay`,
//! `wormhole_verify_vaa_shim`) are the same hash-pinned artefacts the WTT suite
//! uses; rather than duplicate the binaries, the resolvers below point at the
//! WTT crate's `tests/fixtures/` directory. Local iteration can redirect each
//! via `GA_NOREPLAY_SO` / `GA_VERIFY_VAA_SHIM_SO`.

#![allow(dead_code)] // Different integration tests use different subsets.

pub mod guardian_fixtures;
pub mod mollusk_fixtures;

use std::path::PathBuf;

/// WTT crate's `tests/fixtures/` directory, which holds the canonical
/// hash-pinned sibling `.so` programs shared across both suites.
fn wtt_fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("programs/ parent of ntt-global-accountant")
        .join("global-accountant/tests/fixtures")
}

/// Resolve `solana_noreplay.so`: the WTT crate's hash-pinned copy by default,
/// or `GA_NOREPLAY_SO` for local iteration (override skips the SHA-256 check).
pub fn noreplay_so_path() -> PathBuf {
    if let Ok(p) = std::env::var("GA_NOREPLAY_SO") {
        return PathBuf::from(p);
    }
    wtt_fixtures_dir().join("solana_noreplay.so")
}

/// Resolve `wormhole_verify_vaa_shim.so`: the WTT crate's hash-pinned copy by
/// default, or `GA_VERIFY_VAA_SHIM_SO` for local iteration (override skips the
/// SHA-256 check).
pub fn verify_vaa_shim_so_path() -> PathBuf {
    if let Ok(p) = std::env::var("GA_VERIFY_VAA_SHIM_SO") {
        return PathBuf::from(p);
    }
    wtt_fixtures_dir().join("wormhole_verify_vaa_shim.so")
}
