//! Deterministic backfill operator keys, and the artifact authority probe both backfill
//! suites run against their built `.so`.

use accountant_operational_core::accounts::balance;
use global_accountant_definitions::Uint256;
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::program::loader_keys::LOADER_V3;
use mollusk_svm::result::ProgramResult;
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use crate::mollusk::{system_owned_account, uninitialised_pda_account};
use crate::wire;

/// Signer the `global-accountant-backfill` test artifact accepts. Its pubkey is the
/// justfile's `TEST_BACKFILL_AUTHORITY`, passed as `BACKFILL_AUTHORITY` at build time.
pub fn test_authority_keypair() -> Keypair {
    Keypair::new_from_array([1u8; 32])
}

pub fn test_authority_pubkey() -> Pubkey {
    test_authority_keypair().pubkey()
}

/// Signer the `ntt-global-accountant-backfill` test artifact accepts. Its pubkey is the
/// justfile's `TEST_NTT_BACKFILL_AUTHORITY`, passed as `NTT_BACKFILL_AUTHORITY` at build
/// time. Distinct from the WTT key so cross-program authority isolation is testable.
pub fn ntt_test_authority_keypair() -> Keypair {
    Keypair::new_from_array([2u8; 32])
}

pub fn ntt_test_authority_pubkey() -> Pubkey {
    ntt_test_authority_keypair().pubkey()
}

const PROBE_CHAIN: u16 = 1;
const PROBE_TOKEN: [u8; 32] = [0x01u8; 32];

/// What the authority probe needs to drive one backfill artifact.
pub struct ArtifactAuthorityProbe {
    /// Basename of the `.so` under `SBF_OUT_DIR`.
    pub program_name: &'static str,
    /// The program's `declare_id!` address. The ELF is loaded here; any other address fails
    /// earlier with `DeclaredProgramIdMismatch`.
    pub program_id: Pubkey,
    /// The authority this source tree compiles in, for the self-check arm.
    pub compiled_in_authority: [u8; 32],
    /// Name of the build variable that sets it, for failure messages.
    pub authority_var: &'static str,
    /// Discriminator of the arm the probe drives.
    pub discriminator: u8,
    /// Builds that arm's instruction data and names the one PDA it writes.
    pub payload: fn(&Pubkey, u8) -> (Vec<u8>, Pubkey),
}

/// Minimal `BackfillBalance` payload: one zero-balance entry, and the `Balance` PDA it writes.
pub fn balance_probe_payload(program_id: &Pubkey, discriminator: u8) -> (Vec<u8>, Pubkey) {
    let entry = wire::balance_entry(PROBE_CHAIN, PROBE_CHAIN, PROBE_TOKEN, Uint256::ZERO.0);
    let (pda, _) = balance::derive_pda(program_id, PROBE_CHAIN, PROBE_CHAIN, &PROBE_TOKEN);
    (wire::encode_balance_batch(discriminator, &[entry]), pda)
}

/// A well-formed one-entry batch "signed" by `candidate`: mollusk trusts the `is_signer` flag,
/// so success or `UnauthorizedCaller` turns only on the `.so`'s own compiled-in authority.
fn probe_instruction(
    probe: &ArtifactAuthorityProbe,
    candidate: Pubkey,
) -> (Instruction, Vec<(Pubkey, Account)>) {
    let (data, pda) = (probe.payload)(&probe.program_id, probe.discriminator);
    let (system_program, system_account) = keyed_account_for_system_program();

    let ix = Instruction::new_with_bytes(
        probe.program_id,
        &data,
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

/// Whether `elf` accepts `candidate` as its backfill authority.
pub fn so_accepts_authority(probe: &ArtifactAuthorityProbe, elf: &[u8], candidate: Pubkey) -> bool {
    let mut mollusk = Mollusk::default();
    mollusk.add_program_with_loader_and_elf(&probe.program_id, &LOADER_V3, elf);
    let (ix, accounts) = probe_instruction(probe, candidate);
    mollusk.process_instruction(&ix, &accounts).program_result == ProgramResult::Success
}

/// The freshly built artifact accepts the key this tree compiled in, and rejects an unrelated
/// one.
pub fn assert_built_so_enforces_compiled_in_authority(probe: &ArtifactAuthorityProbe) {
    let elf = crate::accounts::deployed_elf(probe.program_name);

    assert!(
        so_accepts_authority(
            probe,
            &elf,
            Pubkey::new_from_array(probe.compiled_in_authority)
        ),
        "the built .so rejected its own compiled-in {} ({:?}); the authority gate or this probe \
         is broken",
        probe.authority_var,
        probe.compiled_in_authority,
    );

    let unrelated = Pubkey::new_from_array([0xEEu8; 32]);
    assert!(
        !so_accepts_authority(probe, &elf, unrelated),
        "the built .so accepted an unrelated pubkey ({unrelated}); the authority gate enforces \
         nothing",
    );
}

/// Operator check: `GA_VERIFY_SO` names a deploy artifact, `GA_EXPECT_AUTHORITY_BASE58` the
/// intended key. Driven by `just verify-authority`.
pub fn assert_so_enforces_operator_authority(probe: &ArtifactAuthorityProbe) {
    let so_path = std::env::var("GA_VERIFY_SO")
        .expect("set GA_VERIFY_SO=/path/to/your.so; see this test's doc comment");
    let expected_base58 = std::env::var("GA_EXPECT_AUTHORITY_BASE58")
        .expect("set GA_EXPECT_AUTHORITY_BASE58=<pubkey>; see this test's doc comment");
    let expected: Pubkey = expected_base58.parse().unwrap_or_else(|e| {
        panic!("GA_EXPECT_AUTHORITY_BASE58={expected_base58} is not a base58 pubkey: {e:?}")
    });
    let elf = std::fs::read(&so_path).unwrap_or_else(|e| panic!("read {so_path}: {e}"));

    assert!(
        so_accepts_authority(probe, &elf, expected),
        "{so_path} rejected {expected_base58}; it was not built with that key, do not deploy it"
    );
    eprintln!("[artifact-check] {so_path} enforces {expected_base58}");
}
