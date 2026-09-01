//! Builds a `Mollusk` with the real `solana_noreplay.so` and
//! `wormhole_verify_vaa_shim.so` at their canonical IDs, so tests exercise the
//! production CPI paths. Both `.so` files are embedded and SHA-256-pinned by
//! the shared `accountant-test-fixtures` crate — the same artefacts the WTT
//! `global-accountant` suite loads.
//!
//! Local iteration: `GA_NOREPLAY_SO=<path>` / `GA_VERIFY_VAA_SHIM_SO=<path>`
//! redirect the resolver and skip the SHA-256 check.

use accountant_test_fixtures::{Program, NOREPLAY_SO, VERIFY_VAA_SHIM_SO};
use global_accountant_definitions::{NOREPLAY_PROGRAM_ID, VERIFY_VAA_SHIM_PROGRAM_ID};
use mollusk_svm::{
    program::{create_program_account_loader_v3, loader_keys::LOADER_V3},
    Mollusk,
};
use sha2::{Digest, Sha256};
use solana_account::Account;
use solana_pubkey::Pubkey;

/// Build a `Mollusk` with the NTT program at `program_id` and both sibling
/// fixtures preloaded at their canonical IDs. `program_name` is the `.so`
/// stem (typically `"ntt_global_accountant"`).
pub fn mollusk_with_fixtures(program_id: &Pubkey, program_name: &str) -> Mollusk {
    let mut mollusk = Mollusk::new(program_id, program_name);

    let noreplay_elf = program_elf(&NOREPLAY_SO, "solana_noreplay", "GA_NOREPLAY_SO");
    let shim_elf = program_elf(
        &VERIFY_VAA_SHIM_SO,
        "wormhole_verify_vaa_shim",
        "GA_VERIFY_VAA_SHIM_SO",
    );

    mollusk.add_program_with_loader_and_elf(
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        &LOADER_V3,
        &noreplay_elf,
    );
    mollusk.add_program_with_loader_and_elf(
        &Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID),
        &LOADER_V3,
        &shim_elf,
    );

    mollusk
}

/// Loader-V3 executable `(program_id, Account)` for the noreplay program,
/// required because `process_instruction` consumes the account list verbatim;
/// a system-owned stand-in would fail as `UnsupportedProgramId` at CPI time.
pub fn keyed_account_for_noreplay_program() -> (Pubkey, Account) {
    let id = Pubkey::new_from_array(NOREPLAY_PROGRAM_ID);
    (id, create_program_account_loader_v3(&id))
}

/// As `keyed_account_for_noreplay_program`, for the Verify VAA Shim.
pub fn keyed_account_for_verify_vaa_shim_program() -> (Pubkey, Account) {
    let id = Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID);
    (id, create_program_account_loader_v3(&id))
}

/// Fetch a fixture's bytes, verifying its SHA-256 against the pinned value
/// unless `env_override` redirects to a local file.
fn program_elf(program: &Program, label: &str, env_override: &str) -> Vec<u8> {
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
