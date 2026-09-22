//! Checks which `NTT_BACKFILL_AUTHORITY` a *built* `.so` enforces, by running its authority
//! gate inside a mollusk-hosted SBF VM. The gate the artifact carries is what a deploy
//! enforces, and only executing it establishes that; a byte scan of the binary is unreliable.
//!
//! - `built_so_accepts_compiled_in_backfill_authority_and_rejects_others` runs under
//!   `just test` against the freshly built artifact.
//! - `verify_so_enforces_specific_operator_authority` is operator-driven: point
//!   `GA_VERIFY_SO` and `GA_EXPECT_AUTHORITY_BASE58` at a deploy artifact and the intended
//!   key, through `just verify-authority <so-path> <base58-pubkey> ntt-global-accountant-backfill`.

use accountant_operational_core::accounts::balance;
use global_accountant_definitions::ntt_global_accountant_backfill::Instruction;
use global_accountant_definitions::Uint256;
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::program::loader_keys::LOADER_V3;
use mollusk_svm::result::ProgramResult;
use mollusk_svm::Mollusk;
use ntt_global_accountant_backfill::NTT_BACKFILL_AUTHORITY;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction as SolanaInstruction};
use solana_pubkey::Pubkey;

mod common;
use common::*;

const PROBE_CHAIN: u16 = 1;
const PROBE_TOKEN: [u8; 32] = [0x01u8; 32];

/// A well-formed one-entry `BackfillBalance`, "signed" by `candidate`: mollusk trusts the
/// `is_signer` flag, so success or `UnauthorizedCaller` turns only on the `.so`'s own
/// compiled-in authority.
fn authority_probe(candidate: Pubkey) -> (SolanaInstruction, Vec<(Pubkey, Account)>) {
    let entry = wire::balance_entry(PROBE_CHAIN, PROBE_CHAIN, PROBE_TOKEN, Uint256::ZERO.0);
    let (pda, _) = balance::derive_pda(&program_id(), PROBE_CHAIN, PROBE_CHAIN, &PROBE_TOKEN);
    let (system_program, system_account) = keyed_account_for_system_program();

    let ix = SolanaInstruction::new_with_bytes(
        program_id(),
        &wire::encode_balance_batch(Instruction::BackfillBalance as u8, &[entry]),
        vec![
            AccountMeta::new(candidate, true),
            AccountMeta::new_readonly(system_program, false),
            AccountMeta::new(pda, false),
        ],
    );
    let accounts = vec![
        (candidate, system_owned_account(10_000_000_000)),
        (system_program, system_account),
        (pda, uninitialised_pda_account()),
    ];
    (ix, accounts)
}

/// Whether `elf` accepts `candidate` as its backfill authority. The program is loaded at its
/// `declare_id!` address; any other id fails earlier with `DeclaredProgramIdMismatch`.
fn so_accepts_authority(elf: &[u8], candidate: Pubkey) -> bool {
    let mut mollusk = Mollusk::default();
    mollusk.add_program_with_loader_and_elf(&program_id(), &LOADER_V3, elf);
    let (ix, accounts) = authority_probe(candidate);
    mollusk.process_instruction(&ix, &accounts).program_result == ProgramResult::Success
}

#[test]
fn built_so_accepts_compiled_in_backfill_authority_and_rejects_others() {
    let elf = deployed_elf(PROGRAM_NAME);

    assert!(
        so_accepts_authority(&elf, Pubkey::new_from_array(NTT_BACKFILL_AUTHORITY)),
        "the built .so rejected its own compiled-in NTT_BACKFILL_AUTHORITY ({:?}); the \
         authority gate or this probe is broken",
        NTT_BACKFILL_AUTHORITY,
    );

    let unrelated = Pubkey::new_from_array([0xEEu8; 32]);
    assert!(
        !so_accepts_authority(&elf, unrelated),
        "the built .so accepted an unrelated pubkey ({unrelated}); the authority gate enforces \
         nothing",
    );
}

/// Operator check against a deploy artifact and a production key, both outside source
/// control:
///
/// ```text
/// just verify-authority /path/to/ntt_global_accountant_backfill.so <base58-pubkey> ntt-global-accountant-backfill
/// ```
#[test]
#[ignore = "operator-driven; needs GA_VERIFY_SO + GA_EXPECT_AUTHORITY_BASE58, see the doc comment"]
fn verify_so_enforces_specific_operator_authority() {
    let so_path = std::env::var("GA_VERIFY_SO")
        .expect("set GA_VERIFY_SO=/path/to/your.so; see this test's doc comment");
    let expected_base58 = std::env::var("GA_EXPECT_AUTHORITY_BASE58")
        .expect("set GA_EXPECT_AUTHORITY_BASE58=<pubkey>; see this test's doc comment");
    let expected: Pubkey = expected_base58.parse().unwrap_or_else(|e| {
        panic!("GA_EXPECT_AUTHORITY_BASE58={expected_base58} is not a base58 pubkey: {e:?}")
    });
    let elf = std::fs::read(&so_path).unwrap_or_else(|e| panic!("read {so_path}: {e}"));

    assert!(
        so_accepts_authority(&elf, expected),
        "{so_path} rejected {expected_base58}; it was not built with that key, do not deploy it"
    );
    eprintln!("[artifact-check] {so_path} enforces {expected_base58}");
}
