//! Verifies a *built* `.so`'s enforced `BACKFILL_AUTHORITY` against an
//! expected pubkey supplied via env var, independent of the compiled-in
//! constant every other test in this suite relies on. A byte scan for the
//! pubkey does not work reliably against the compiled binary, so this
//! executes the authority gate inside a real mollusk-hosted SBF VM instead.
//!
//! Two tests:
//!   - `built_so_accepts_compiled_in_backfill_authority_and_rejects_others` —
//!     sanity check that the freshly-built `.so` enforces its own compiled-in
//!     authority. Runs automatically under `just test`.
//!   - `verify_so_enforces_specific_operator_authority` — the operator-facing
//!     check: point it at any `.so` and any base58 pubkey via env vars.
//!     `#[ignore]`d since it requires those env vars.

use mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use global_accountant_backfill::{Instruction as IxDiscriminator, BACKFILL_AUTHORITY};
use global_accountant_definitions::ACCOUNT_SEED_PREFIX;

mod common;
use common::surfpool::so_path;

const BACKFILL_PROGRAM_NAME: &str = "global_accountant_backfill";

/// Arbitrary fixed program id used only to derive this probe's balance PDA.
/// It doesn't need to match any real deployment id: the authority gate
/// (`require_authority`, checked in `backfill_balance::process` step (3))
/// runs before `program_id` is used for anything, so any value works.
fn probe_program_id() -> Pubkey {
    Pubkey::new_from_array([0x42u8; 32])
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
/// accounts, "signed" by `candidate`. Whether this succeeds or fails with
/// `UnauthorizedCaller` depends only on whether `candidate` matches the
/// target `.so`'s compiled-in `BACKFILL_AUTHORITY` — no private key is
/// needed since mollusk's `process_instruction` trusts the
/// `AccountMeta::is_signer` flag directly rather than verifying a signature.
fn build_authority_probe(
    program_id: &Pubkey,
    candidate: Pubkey,
) -> (Instruction, Vec<(Pubkey, Account)>) {
    let mut data = vec![IxDiscriminator::BackfillBalance as u8, 1u8 /* count */];
    data.extend_from_slice(&1u16.to_be_bytes()); // chain
    data.extend_from_slice(&1u16.to_be_bytes()); // token_chain
    data.extend_from_slice(&[0x01u8; 32]); // token_address
    data.extend_from_slice(&[0u8; 32]); // balance = 0

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
/// whether the authority probe signed by `candidate` is ACCEPTED (`true`) —
/// meaning the instruction ran to completion successfully — or rejected
/// (`false`) by that specific binary.
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

/// Confirms the freshly-built `.so` genuinely enforces `BACKFILL_AUTHORITY`:
/// it accepts that exact pubkey as a signer and rejects an unrelated one.
/// Requires `just build` (or `just test`, which builds first) to have run so
/// the artifact exists on disk.
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
/// (test) `BACKFILL_AUTHORITY`. Driven by env vars since there's no real
/// production pubkey checked into source:
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

    let elf = std::fs::read(&so_path)
        .unwrap_or_else(|e| panic!("could not read {so_path}: {e}"));

    assert!(
        so_accepts_authority(&elf, expected),
        "expected authority {expected_base58} was REJECTED by {so_path} — this `.so` was NOT \
         built with the intended operator key; do not deploy it."
    );
    eprintln!(
        "[artifact-check] {expected_base58} IS accepted as the backfill authority by {so_path}"
    );
}
