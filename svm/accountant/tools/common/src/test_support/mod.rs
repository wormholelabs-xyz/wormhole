//! Test helpers for the e2e suites and the shared snapshot fixtures.

use std::path::PathBuf;

pub mod surfpool;

/// Snapshot fixture shipped with this crate.
pub fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}
