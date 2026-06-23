//! Shared fixtures for the in-process real-CPI suite: deterministic guardian
//! key material (`guardian_fixtures`) and the hash-pinned `.so` loader
//! (`mollusk_fixtures`).

#![allow(dead_code)] // Different integration tests use different subsets.

pub mod guardian_fixtures;
pub mod mollusk_fixtures;

use std::path::PathBuf;

/// Resolve `solana_noreplay.so`: the hash-pinned `tests/fixtures/` copy by
/// default, or `GA_NOREPLAY_SO` for local iteration (override skips the check).
pub fn noreplay_so_path() -> PathBuf {
    if let Ok(p) = std::env::var("GA_NOREPLAY_SO") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/solana_noreplay.so")
}

/// Resolve `wormhole_verify_vaa_shim.so`: the hash-pinned `tests/fixtures/`
/// copy by default, or `GA_VERIFY_VAA_SHIM_SO` for local iteration (override
/// skips the check).
pub fn verify_vaa_shim_so_path() -> PathBuf {
    if let Ok(p) = std::env::var("GA_VERIFY_VAA_SHIM_SO") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wormhole_verify_vaa_shim.so")
}
