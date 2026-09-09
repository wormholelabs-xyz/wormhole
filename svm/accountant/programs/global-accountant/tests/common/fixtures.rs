//! Program binaries from `accountant-test-fixtures`, with their SHA-256 pins.

use accountant_test_fixtures::Program;
use sha2::{Digest, Sha256};

/// Name of the accountant `.so` under `SBF_OUT_DIR`.
pub const PROGRAM_NAME: &str = "global_accountant";

/// ELF bytes of a sibling program fixture. `env_override` names an env var whose
/// value is a path to a locally built `.so`; it skips the SHA-256 pin.
pub fn fixture_elf(program: &Program, label: &str, env_override: &str) -> Vec<u8> {
    if let Ok(path) = std::env::var(env_override) {
        return std::fs::read(&path)
            .unwrap_or_else(|e| panic!("read ${env_override}={path} for `{label}`: {e}"));
    }
    let actual = Sha256::digest(program.bytes);
    assert_eq!(
        actual[..],
        program.sha256,
        "fixture `{label}` SHA-256 mismatch; rebuild the sibling program, copy the .so over \
         `crates/test-fixtures/data/{label}.so`, update `sha256` in `crates/test-fixtures/src/lib.rs`"
    );
    program.bytes.to_vec()
}
