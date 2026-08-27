//! Builds a `Mollusk` with the global-accountant `.so` and the real `solana_noreplay.so` and
//! `wormhole_verify_vaa_shim.so` from `accountant-test-fixtures`, each checked against its
//! pinned SHA-256.
//!
//! `GA_NOREPLAY_SO=<path>` / `GA_VERIFY_VAA_SHIM_SO=<path>` load a different binary and skip
//! the check.

use {
    accountant_test_fixtures::{Program, NOREPLAY_SO, VERIFY_VAA_SHIM_SO},
    global_accountant_definitions::{NOREPLAY_PROGRAM_ID, VERIFY_VAA_SHIM_PROGRAM_ID},
    mollusk_svm::{
        program::{create_program_account_loader_v3, loader_keys::LOADER_V3},
        Mollusk,
    },
    sha2::{Digest, Sha256},
    solana_account::Account,
    solana_pubkey::Pubkey,
    std::fs,
};

/// `Mollusk` with the program at `program_id` and both fixtures preloaded. `program_name`
/// is the `.so` stem.
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

/// Loader-V3 executable `(program_id, Account)` for the NoReplay program. A system-owned
/// stand-in fails with `UnsupportedProgramId` at CPI time.
pub fn keyed_account_for_noreplay_program() -> (Pubkey, Account) {
    let id = Pubkey::new_from_array(NOREPLAY_PROGRAM_ID);
    let account = create_program_account_loader_v3(&id);
    (id, account)
}

/// As `keyed_account_for_noreplay_program`, for the Verify VAA Shim.
pub fn keyed_account_for_verify_vaa_shim_program() -> (Pubkey, Account) {
    let id = Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID);
    let account = create_program_account_loader_v3(&id);
    (id, account)
}

/// Fixture bytes after a SHA-256 check; panics with the regen recipe on drift.
/// `env_override` names a file to load instead, unchecked.
fn program_elf(program: &Program, label: &str, env_override: &str) -> Vec<u8> {
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
