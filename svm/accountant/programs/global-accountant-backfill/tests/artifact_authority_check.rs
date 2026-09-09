//! Verifies a *built* `.so`'s enforced `BACKFILL_AUTHORITY` against an
//! expected pubkey supplied via env var, independent of the compiled-in
//! constant every other test in this suite relies on. A byte scan of the
//! compiled binary is unreliable, so this runs the authority gate inside a
//! real mollusk-hosted SBF VM.
//!
//! Two tests:
//!   - `built_so_accepts_compiled_in_backfill_authority_and_rejects_others` —
//!     sanity check that the freshly-built `.so` enforces its own compiled-in
//!     authority. Runs automatically under `just test`.
//!   - `verify_so_enforces_specific_operator_authority` — operator-facing
//!     check: point it at any `.so` and base58 pubkey via env vars.
//!     `#[ignore]`d, requires those env vars.

use mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use global_accountant_backfill::BACKFILL_AUTHORITY;
use global_accountant_definitions::{Uint256, ACCOUNT_SEED_PREFIX};

mod common;
use common::*;

/// The program's `declare_id!`-fixed address. Anchor checks the declared
/// program id against the runtime `program_id` on every entry
/// (`DeclaredProgramIdMismatch` otherwise), so this probe must deploy at
/// the same fixed id the `.so` was built with.
fn probe_program_id() -> Pubkey {
    Pubkey::new_from_array(global_accountant_backfill::ID.to_bytes())
}

/// Canonical seeds match `backfill_balance::process`'s
/// `[ACCOUNT_SEED_PREFIX, chain_be, token_chain_be, token_address]`.
fn derive_probe_balance_pda(program_id: &Pubkey) -> Pubkey {
    let (pda, _) = Pubkey::find_program_address(
        &[
            ACCOUNT_SEED_PREFIX,
            &1u16.to_be_bytes(),
            &1u16.to_be_bytes(),
            &[0x01u8; 32],
        ],
        program_id,
    );
    pda
}

/// Builds a minimal, well-formed `BackfillBalance` (1 entry) instruction +
/// accounts, "signed" by `candidate`. Success vs. `UnauthorizedCaller`
/// depends only on whether `candidate` matches the target `.so`'s
/// compiled-in `BACKFILL_AUTHORITY` — mollusk's `process_instruction`
/// trusts the `AccountMeta::is_signer` flag as the sole proof of signing.
fn build_authority_probe(
    program_id: &Pubkey,
    candidate: Pubkey,
) -> (Instruction, Vec<(Pubkey, Account)>) {
    let entry = balance_entry(1, 1, [0x01u8; 32], Uint256::ZERO.0);
    let data = encode_balance_batch(&[entry]);

    let pda = derive_probe_balance_pda(program_id);
    let (sys_id, sys_acc) = keyed_account_for_system_program();

    let accounts: Vec<(Pubkey, Account)> = vec![
        (
            candidate,
            Account {
                lamports: 10_000_000_000,
                data: vec![],
                owner: sys_id,
                executable: false,
                rent_epoch: 0,
            },
        ),
        (sys_id, sys_acc),
        (
            pda,
            Account {
                lamports: 0,
                data: vec![],
                owner: sys_id,
                executable: false,
                rent_epoch: 0,
            },
        ),
    ];
    let metas = vec![
        AccountMeta::new(candidate, true),
        AccountMeta::new_readonly(sys_id, false),
        AccountMeta::new(pda, false),
    ];
    (
        Instruction {
            program_id: *program_id,
            accounts: metas,
            data,
        },
        accounts,
    )
}

/// Load `elf_bytes` into a fresh, isolated Mollusk instance and report
/// whether the authority probe signed by `candidate` runs to completion
/// (`true`) or is rejected (`false`) by that specific binary.
fn so_accepts_authority(elf_bytes: &[u8], candidate: Pubkey) -> bool {
    let program_id = probe_program_id();
    let mut mollusk = Mollusk::default();
    mollusk.add_program_with_loader_and_elf(
        &program_id,
        &mollusk_svm::program::loader_keys::LOADER_V3,
        elf_bytes,
    );
    let (ix, accounts) = build_authority_probe(&program_id, candidate);
    let result = mollusk.process_instruction(&ix, &accounts);
    result.program_result == ProgramResult::Success
}

/// Confirms the freshly-built `.so` enforces `BACKFILL_AUTHORITY`: accepts
/// that exact pubkey as a signer, rejects an unrelated one. Requires
/// `just build` (or `just test`, which builds first) to produce the
/// artifact on disk.
#[test]
fn built_so_accepts_compiled_in_backfill_authority_and_rejects_others() {
    let path = so_path(BACKFILL_PROGRAM_NAME);
    let elf = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "could not read {}: {e}. Run `just build` first (or `just test`, which builds \
             automatically).",
            path.display()
        )
    });

    assert!(
        so_accepts_authority(&elf, Pubkey::new_from_array(BACKFILL_AUTHORITY)),
        "the built .so at {} did NOT accept its own compiled-in BACKFILL_AUTHORITY \
         ({:?}) as a valid signer — the authority gate itself, or this probe's wiring, is broken",
        path.display(),
        BACKFILL_AUTHORITY,
    );

    let unrelated = Pubkey::new_from_array([0xEEu8; 32]);
    assert!(
        !so_accepts_authority(&elf, unrelated),
        "the built .so at {} accepted an UNRELATED pubkey ({unrelated}) as a valid signer — \
         the authority gate is not enforcing anything",
        path.display(),
    );
}

/// Operator check: verifies a specific deploy-ready `.so` enforces a given
/// production authority pubkey, independent of this repo's compiled-in
/// (test) `BACKFILL_AUTHORITY`. The production pubkey lives outside
/// source control, so it is driven by env vars:
///
/// ```text
/// GA_VERIFY_SO=/path/to/deploy-ready/global_accountant_backfill.so \
/// GA_EXPECT_AUTHORITY_BASE58=<operator pubkey, base58> \
///     cargo test -p global-accountant-backfill --test artifact_authority_check \
///     -- --ignored --nocapture verify_so_enforces_specific_operator_authority
/// ```
///
/// Also available as `just verify-authority <so-path> <base58-pubkey>`.
#[test]
#[ignore = "operator-driven; requires GA_VERIFY_SO + GA_EXPECT_AUTHORITY_BASE58 env vars, see doc comment"]
fn verify_so_enforces_specific_operator_authority() {
    let so_path = std::env::var("GA_VERIFY_SO").unwrap_or_else(|_| {
        panic!(
            "set GA_VERIFY_SO=/path/to/your.so and GA_EXPECT_AUTHORITY_BASE58=<pubkey> to run \
             this check against a real deploy artifact; see this test's doc comment for the \
             exact invocation."
        )
    });
    let expected_base58 = std::env::var("GA_EXPECT_AUTHORITY_BASE58").unwrap_or_else(|_| {
        panic!(
            "GA_EXPECT_AUTHORITY_BASE58 not set; see this test's doc comment for the exact \
             invocation."
        )
    });
    let expected: Pubkey = expected_base58.parse().unwrap_or_else(|e| {
        panic!("GA_EXPECT_AUTHORITY_BASE58={expected_base58} is not a valid base58 pubkey: {e:?}")
    });

    let elf = std::fs::read(&so_path).unwrap_or_else(|e| panic!("could not read {so_path}: {e}"));

    assert!(
        so_accepts_authority(&elf, expected),
        "expected authority {expected_base58} was REJECTED by {so_path} — this `.so` was NOT \
         built with the intended operator key; do not deploy it."
    );
    eprintln!(
        "[artifact-check] {expected_base58} IS accepted as the backfill authority by {so_path}"
    );
}
