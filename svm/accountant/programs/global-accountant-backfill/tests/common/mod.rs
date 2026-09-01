//! Shared helpers for backfill integration tests. The real `solana_noreplay.so` comes from
//! `accountant-test-fixtures`, checked against its pinned SHA-256.
//!
//! `GA_NOREPLAY_SO=<path>` loads a different binary and skips the check.

#![allow(dead_code)] // Different test files use different subsets.

pub mod mollusk;
pub mod surfpool;
pub mod wire;

use {
    accountant_test_fixtures::{Program, NOREPLAY_SO},
    global_accountant_definitions::NOREPLAY_PROGRAM_ID,
    mollusk_svm::{
        program::{create_program_account_loader_v3, loader_keys::LOADER_V3},
        Mollusk,
    },
    sha2::{Digest, Sha256},
    solana_account::Account,
    solana_pubkey::Pubkey,
    std::fs,
};

pub const BACKFILL_PROGRAM_NAME: &str = "global_accountant_backfill";

/// Build a `Mollusk` with the backfill `.so` plus the real
/// `solana_noreplay.so` at its canonical ID.
pub fn mollusk_with_noreplay(program_id: &Pubkey) -> Mollusk {
    let mut mollusk = Mollusk::new(program_id, BACKFILL_PROGRAM_NAME);

    let noreplay_elf = program_elf(&NOREPLAY_SO, "solana_noreplay", "GA_NOREPLAY_SO");
    mollusk.add_program_with_loader_and_elf(
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        &LOADER_V3,
        &noreplay_elf,
    );

    mollusk
}

/// `(pubkey, Account)` for the noreplay program. Mollusk consumes the
/// account list verbatim; a system-owned stand-in fails at CPI time as
/// `UnsupportedProgramId`.
pub fn keyed_account_for_noreplay_program() -> (Pubkey, Account) {
    let id = Pubkey::new_from_array(NOREPLAY_PROGRAM_ID);
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
