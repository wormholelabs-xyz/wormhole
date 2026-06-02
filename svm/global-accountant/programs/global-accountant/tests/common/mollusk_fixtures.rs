//! Mollusk-with-fixture-programs builder.
//!
//! Bundles the global-accountant `.so` together with the real
//! `solana_noreplay.so` and `wormhole_verify_vaa_shim.so` at their canonical
//! program IDs so mollusk tests can exercise the production CPI paths
//! without a stub or feature-gated mock branch.
//!
//! The two sibling `.so` files are checked into `tests/fixtures/` and
//! verified against pinned SHA-256 digests before mollusk loads them. Drift
//! (an unintended rebuild ending up in the fixtures dir; a developer's
//! local override leaking into a commit) surfaces as a panic with the
//! recompute-and-update recipe inline in the failure message.
//!
//! ## Regenerating a fixture
//!
//! 1. Build the upstream program (`cd <sibling-repo> && cargo build-sbf`).
//! 2. Copy the resulting `.so` over `tests/fixtures/<name>.so`.
//! 3. Recompute the digest: `shasum -a 256 tests/fixtures/<name>.so`.
//! 4. Update the corresponding `*_SO_SHA256` constant below.
//! 5. Run `cargo test` to confirm the new hash + behaviour both line up.
//!
//! ## Local iteration without rebuilding the fixture
//!
//! `GA_NOREPLAY_SO=/path/to/your.so cargo test ...` (or
//! `GA_VERIFY_VAA_SHIM_SO=...`) redirects the resolver to an arbitrary
//! path and **skips** the SHA-256 check — the env override is the
//! escape hatch for active development against an unstable upstream
//! binary.

use {
    global_accountant_definitions::{NOREPLAY_PROGRAM_ID, VERIFY_VAA_SHIM_PROGRAM_ID},
    mollusk_svm::{
        program::{create_program_account_loader_v3, loader_keys::LOADER_V3},
        Mollusk,
    },
    sha2::{Digest, Sha256},
    solana_account::Account,
    solana_pubkey::Pubkey,
    std::{fs, path::Path},
};

/// SHA-256 of the pinned `solana_noreplay.so`.
/// Reproduce: `shasum -a 256 programs/global-accountant/tests/fixtures/solana_noreplay.so`.
const NOREPLAY_SO_SHA256: [u8; 32] = [
    0xf0, 0xce, 0x80, 0x03, 0x97, 0xb7, 0xa4, 0x78, 0x87, 0xdd, 0x16, 0xa2, 0xbb, 0xe0, 0x67, 0x9d,
    0xe7, 0x3f, 0x95, 0x93, 0x88, 0x1c, 0x62, 0x0e, 0xcc, 0x51, 0x48, 0xd7, 0xe7, 0xf8, 0x6f, 0xd6,
];

/// SHA-256 of the pinned `wormhole_verify_vaa_shim.so`.
/// Reproduce: `shasum -a 256 programs/global-accountant/tests/fixtures/wormhole_verify_vaa_shim.so`.
const VERIFY_VAA_SHIM_SO_SHA256: [u8; 32] = [
    0xba, 0xc0, 0xee, 0x4b, 0xb4, 0xb1, 0x2b, 0xd4, 0xaf, 0x9c, 0xa9, 0xe4, 0x5a, 0xc6, 0x7e, 0x4b,
    0x77, 0x96, 0xc9, 0xf6, 0x04, 0x68, 0xa4, 0x4d, 0xa0, 0x3d, 0x16, 0x9c, 0x42, 0xaf, 0xaa, 0x40,
];

/// Build a `Mollusk` with the global-accountant program loaded at
/// `program_id` and both sibling fixture programs preloaded at their
/// canonical IDs.
///
/// `program_name` is the global-accountant `.so` filename (without the
/// extension) — typically `"global_accountant"`. Mollusk's default search
/// paths (`tests/fixtures`, `BPF_OUT_DIR`, `SBF_OUT_DIR`, cwd) apply.
pub fn mollusk_with_fixtures(program_id: &Pubkey, program_name: &str) -> Mollusk {
    let mut mollusk = Mollusk::new(program_id, program_name);

    let noreplay_elf = read_so(
        &super::noreplay_so_path(),
        "solana_noreplay",
        "GA_NOREPLAY_SO",
        &NOREPLAY_SO_SHA256,
    );
    let shim_elf = read_so(
        &super::verify_vaa_shim_so_path(),
        "wormhole_verify_vaa_shim",
        "GA_VERIFY_VAA_SHIM_SO",
        &VERIFY_VAA_SHIM_SO_SHA256,
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

/// Build an executable `(program_id, Account)` entry suitable for the
/// account list passed to `Mollusk::process_instruction`. Uses Loader V3
/// (upgradeable) to match `mollusk_with_fixtures`'s loader choice. Needed
/// because the simple `process_instruction` path consumes the caller's
/// account list verbatim — system-owned stand-ins at program IDs surface
/// as `UnsupportedProgramId` at CPI time.
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

/// Read a fixture `.so` file and verify it matches the pinned SHA-256.
/// Panics with an actionable message on either branch:
///
/// - missing file → point at `tests/fixtures/` and the regen procedure.
/// - hash drift → emit both the expected and actual digests and the
///   regen procedure, so a developer who accidentally clobbered the
///   fixture (or who upgraded an upstream program intentionally) sees
///   exactly what to update.
///
/// When the path was resolved from an env override (`env_override`), the
/// hash check is skipped — the override is the documented escape hatch
/// for active development against an unstable upstream binary.
fn read_so(path: &Path, label: &str, env_override: &str, expected_sha256: &[u8; 32]) -> Vec<u8> {
    let bytes = fs::read(path).unwrap_or_else(|e| {
        panic!(
            "missing fixture program `{label}` at {}: {e}\n  hint: the canonical \
             fixture is checked in at tests/fixtures/{label}.so; if you intentionally \
             removed it, set ${env_override}=/path/to/your.so to redirect",
            path.display()
        )
    });
    if std::env::var(env_override).is_ok() {
        // Caller is intentionally swapping the binary; respect their choice
        // and skip the hash assertion. The override path is documented in
        // the module-level doc-comment.
        return bytes;
    }
    let actual = Sha256::digest(&bytes);
    if &actual[..] != expected_sha256 {
        panic!(
            "fixture `{label}` SHA-256 drift\n  expected: {}\n  actual:   {}\n  recompute: \
             shasum -a 256 {}\n  then update the corresponding *_SO_SHA256 constant in \
             tests/common/mollusk_fixtures.rs",
            hex_lower(expected_sha256),
            hex_lower(&actual[..]),
            path.display(),
        );
    }
    bytes
}

/// Lowercase hex-encode a byte slice. Avoids a dev-dep on `hex`; only used
/// in the SHA-256 drift panic message.
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}
