//! Embedded fixture binaries and the paths of built artifacts.

use std::fs;
use std::path::{Path, PathBuf};

use accountant_test_fixtures::Program;
use sha2::{Digest, Sha256};

pub const BACKFILL_PROGRAM_NAME: &str = "global_accountant_backfill";

/// Path to a built `.so` under the workspace `target/deploy`.
pub fn so_path(name: &str) -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("workspace root from CARGO_MANIFEST_DIR")
        .to_path_buf();
    workspace_root
        .join("target/deploy")
        .join(format!("{name}.so"))
}

/// Fixture bytes after a SHA-256 check; panics with the regen recipe on drift.
/// `env_override` names a file to load instead, unchecked.
pub fn program_elf(program: &Program, label: &str, env_override: &str) -> Vec<u8> {
    if let Ok(path) = std::env::var(env_override) {
        return fs::read(&path)
            .unwrap_or_else(|e| panic!("read ${env_override}={path} for `{label}`: {e}"));
    }
    let actual = Sha256::digest(program.bytes);
    if actual[..] != program.sha256 {
        panic!(
            "fixture `{label}` SHA-256 mismatch\n  expected: {}\n  actual:   {}\n  \
             recipe: rebuild the sibling program, copy the .so over `crates/test-fixtures/data/{label}.so`, \
             then update `sha256` in `crates/test-fixtures/src/lib.rs` to the actual value",
            hex(&program.sha256),
            hex(&actual),
        );
    }
    program.bytes.to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
