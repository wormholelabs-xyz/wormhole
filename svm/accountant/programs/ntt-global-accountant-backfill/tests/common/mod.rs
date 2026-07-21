//! Shared mollusk fixtures for the NTT backfill suite. The three NTT-native
//! map handlers do no CPI, so a plain `Mollusk` with just the NTT `.so`
//! (resolved from `SBF_OUT_DIR`) is all that's needed for them. The reused
//! `BackfillNoReplay` handler CPIs into `solana-noreplay`, so its tests go
//! through [`mollusk_with_noreplay`] instead, which co-deploys the same
//! hash-pinned `solana_noreplay.so` vendored under this program's own
//! `tests/fixtures/`.

#![allow(dead_code)]

pub mod surfpool;

use {
    global_accountant_definitions::NOREPLAY_PROGRAM_ID,
    mollusk_svm::{
        program::{
            create_program_account_loader_v3, keyed_account_for_system_program,
            loader_keys::LOADER_V3,
        },
        Mollusk,
    },
    sha2::{Digest, Sha256},
    solana_account::Account,
    solana_keypair::Keypair,
    solana_pubkey::Pubkey,
    solana_signer::Signer,
    std::{fs, path::PathBuf},
};

/// Canonical test program id. Arbitrary but deterministic; distinct from the
/// WTT suite's id so the two never share derived PDAs.
pub fn program_id() -> Pubkey {
    Pubkey::new_from_array([9u8; 32])
}

/// Mollusk loaded with the NTT backfill `.so` from `SBF_OUT_DIR`. Sufficient
/// for the NTT-native map handlers (`BackfillRelayerRegistration`,
/// `BackfillTransceiverHub`, `BackfillTransceiverPeer`), which perform no CPI.
pub fn mollusk() -> Mollusk {
    Mollusk::new(&program_id(), "ntt_global_accountant_backfill")
}

/// SHA-256 of the vendored `solana_noreplay.so`, identical to the constant
/// pinned in `global-accountant-backfill/tests/common/mod.rs` — same real
/// upstream program. Recompute (`shasum -a 256 tests/fixtures/solana_noreplay.so`)
/// and update here if the fixture is intentionally regenerated.
const NOREPLAY_SO_SHA256: [u8; 32] = [
    0x33, 0xbe, 0x38, 0x6b, 0xac, 0xf5, 0x6b, 0x98, 0x98, 0xfb, 0xa7, 0x5b, 0x10, 0x48, 0xcb, 0xe1,
    0x17, 0x90, 0xf2, 0x42, 0xe9, 0x75, 0x07, 0xb4, 0x52, 0xeb, 0x7f, 0x08, 0xd0, 0x86, 0x9a, 0xc7,
];

/// Resolve the vendored `solana_noreplay.so` from this program's own
/// `tests/fixtures/`. `GA_NOREPLAY_SO` overrides for local iteration (skips
/// the hash check).
pub fn noreplay_so_path() -> PathBuf {
    if let Ok(p) = std::env::var("GA_NOREPLAY_SO") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/solana_noreplay.so")
}

/// Build a `Mollusk` with the NTT backfill `.so` plus the real
/// `solana_noreplay.so` at its canonical ID. Needed for `BackfillNoReplay`
/// tests exercised through the NTT program's own entrypoint/program id.
pub fn mollusk_with_noreplay() -> Mollusk {
    let mut mollusk = Mollusk::new(&program_id(), "ntt_global_accountant_backfill");

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
            "missing fixture program `{label}` at {}: {e}\n  hint: the vendored \
             fixture is at tests/fixtures/{label}.so; \
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

pub fn system_program_id() -> Pubkey {
    keyed_account_for_system_program().0
}

/// Deterministic test keypair matching the NTT `BACKFILL_AUTHORITY` const
/// (seed `[2u8; 32]`), deliberately distinct from WTT's own test authority
/// (seed `[1u8; 32]`) so cross-program isolation tests are meaningful.
pub fn test_authority_keypair() -> Keypair {
    Keypair::new_from_array([2u8; 32])
}

/// WTT backfill program's test authority pubkey, imported directly from its
/// `BACKFILL_AUTHORITY` const (not re-derived) so drift in the real const
/// is caught.
pub fn wtt_authority_pubkey() -> Pubkey {
    Pubkey::new_from_array(global_accountant_backfill::BACKFILL_AUTHORITY)
}

pub fn test_authority_pubkey() -> Pubkey {
    test_authority_keypair().pubkey()
}

/// Empty, system-owned account with the given lamport balance.
pub fn system_owned_account(lamports: u64) -> Account {
    Account {
        lamports,
        data: vec![],
        owner: system_program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

pub fn signer_account(lamports: u64) -> Account {
    system_owned_account(lamports)
}

/// Uninitialised PDA stub: zero lamports, zero-length data, system-owned.
/// Drives the `CreateAccount` branch of `pda_init::init_or_upgrade_pda`.
pub fn uninitialised_pda_account() -> Account {
    system_owned_account(0)
}
