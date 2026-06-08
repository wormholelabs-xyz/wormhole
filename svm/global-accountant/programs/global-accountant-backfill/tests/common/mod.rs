//! Shared helpers for backfill integration tests. The pinned
//! `solana_noreplay.so` lives under the operational program's
//! `tests/fixtures/` — resolved via a workspace-relative walk so we keep one
//! canonical copy and one canonical SHA-256 constant.

#![allow(dead_code)] // Different test files use different subsets.

use {
    global_accountant_definitions::NOREPLAY_PROGRAM_ID,
    mollusk_svm::{
        program::{create_program_account_loader_v3, loader_keys::LOADER_V3},
        Mollusk,
    },
    sha2::{Digest, Sha256},
    solana_account::Account,
    solana_pubkey::Pubkey,
    std::{fs, path::PathBuf},
};

pub const BACKFILL_PROGRAM_NAME: &str = "global_accountant_backfill";

/// SHA-256 of the pinned `solana_noreplay.so` — must match the operational
/// program's `tests/common/mollusk_fixtures.rs`. If you regenerate the fixture
/// there, update this constant too (or rip both out and lift to a shared
/// crates/ helper).
const NOREPLAY_SO_SHA256: [u8; 32] = [
    0xf0, 0xce, 0x80, 0x03, 0x97, 0xb7, 0xa4, 0x78, 0x87, 0xdd, 0x16, 0xa2, 0xbb, 0xe0, 0x67, 0x9d,
    0xe7, 0x3f, 0x95, 0x93, 0x88, 0x1c, 0x62, 0x0e, 0xcc, 0x51, 0x48, 0xd7, 0xe7, 0xf8, 0x6f, 0xd6,
];

/// Resolve `solana_noreplay.so` from the operational program's fixtures
/// directory. `GA_NOREPLAY_SO` overrides for local iteration (skips hash check).
pub fn noreplay_so_path() -> PathBuf {
    if let Ok(p) = std::env::var("GA_NOREPLAY_SO") {
        return PathBuf::from(p);
    }
    // CARGO_MANIFEST_DIR = .../svm/global-accountant/programs/global-accountant-backfill
    // Workspace root = .../svm/global-accountant
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root from CARGO_MANIFEST_DIR");
    workspace_root.join("programs/global-accountant/tests/fixtures/solana_noreplay.so")
}

/// Build a `Mollusk` with the backfill `.so` plus the real `solana_noreplay.so`
/// at its canonical ID. The shim is intentionally NOT loaded — the backfill
/// program performs no VAA signature verification.
pub fn mollusk_with_noreplay(program_id: &Pubkey) -> Mollusk {
    let mut mollusk = Mollusk::new(program_id, BACKFILL_PROGRAM_NAME);

    let noreplay_elf = read_so_pinned(
        &noreplay_so_path(),
        "solana_noreplay",
        "GA_NOREPLAY_SO",
        &NOREPLAY_SO_SHA256,
    );
    mollusk.add_program_with_loader_and_elf(
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        &LOADER_V3,
        &noreplay_elf,
    );

    mollusk
}

/// `(pubkey, Account)` for the noreplay program. Required because mollusk
/// consumes the account list verbatim; a system-owned stand-in would fail at
/// CPI time as `UnsupportedProgramId`.
pub fn keyed_account_for_noreplay_program() -> (Pubkey, Account) {
    let id = Pubkey::new_from_array(NOREPLAY_PROGRAM_ID);
    let account = create_program_account_loader_v3(&id);
    (id, account)
}

fn read_so_pinned(
    path: &std::path::Path,
    label: &str,
    env_override: &str,
    expected_sha256: &[u8; 32],
) -> Vec<u8> {
    let bytes = fs::read(path).unwrap_or_else(|e| {
        panic!(
            "missing fixture program `{label}` at {}: {e}\n  hint: the canonical \
             fixture is at programs/global-accountant/tests/fixtures/{label}.so; \
             set ${env_override}=/path/to/your.so to redirect",
            path.display()
        )
    });
    if std::env::var(env_override).is_ok() {
        return bytes;
    }
    let actual = Sha256::digest(&bytes);
    if &actual[..] != expected_sha256 {
        panic!(
            "fixture `{label}` SHA-256 drift\n  expected: {}\n  actual:   {}\n  recompute: \
             shasum -a 256 {}\n  then update NOREPLAY_SO_SHA256 in tests/common/mod.rs",
            hex_lower(expected_sha256),
            hex_lower(&actual[..]),
            path.display(),
        );
    }
    bytes
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}
