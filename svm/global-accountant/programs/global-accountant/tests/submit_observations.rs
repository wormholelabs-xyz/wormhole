//! Integration tests for `submit_observations` + `close_pending`.
//!
//! Written test-first per `.claude/tasks/accountant-migration.md`'s TDD rule.
//! Gated on the paired (`mock-vaa`, `test-only-open-digest`, `mock-noreplay`)
//! feature trio so the in-process mollusk runs can sidestep the Verify VAA
//! Shim, expose the test-only `open_digest` for cross-verification, and use a
//! single-byte NoReplay sentinel in place of the real CPI.
//!
//! Test surface covered, mapping to the design doc
//! (`accountant-migration-pending-quorum-design.md`):
//!
//! - §3.1 — single pending PDA per `(chain, emitter, sequence)`.
//! - §3.2 — bitmap-only signature storage.
//! - §3.3 — wipe-on-rotation decision table (stale / rotation / forgery).
//! - §3.4 — inline `secp256k1_recover` verification (good + bad signatures).
//! - §3.5 — quorum commit flow (NoReplay flip + DigestAccount open + pending
//!   close).
//! - §3.6 — `close_pending` triggers (expired set + NoReplay-marked).

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        DigestAccountLayout, GlobalAccountantError, Instruction as IxDiscriminator,
        PendingObservationsLayout, DIGEST_SEED_PREFIX, PENDING_SEED_PREFIX,
    },
    libsecp256k1::{sign, Message, PublicKey, SecretKey},
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk},
    solana_account::Account,
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

const PROGRAM_NAME: &str = "global_accountant";

fn program_id() -> Pubkey {
    // Fixed program id so PDA derivation in the test matches the program's
    // on-chain view.
    Pubkey::new_from_array([7u8; 32])
}

fn mollusk() -> Mollusk {
    Mollusk::new(&program_id(), PROGRAM_NAME)
}

fn system_program_id() -> Pubkey {
    keyed_account_for_system_program().0
}

// ============================================================================
// PDA / instruction-data helpers
// ============================================================================

fn derive_pending_pda(
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    digest: &[u8; 32],
) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    let sequence_be = sequence.to_be_bytes();
    Pubkey::find_program_address(
        &[PENDING_SEED_PREFIX, &chain_be, emitter, &sequence_be, digest],
        &program_id(),
    )
}

fn derive_digest_pda(chain: u16, emitter: &[u8; 32], sequence: u64) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    let sequence_be = sequence.to_be_bytes();
    Pubkey::find_program_address(
        &[DIGEST_SEED_PREFIX, &chain_be, emitter, &sequence_be],
        &program_id(),
    )
}

fn submit_ix_data(
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    digest: &[u8; 32],
    guardian_set_index: u32,
    guardian_index: u8,
    signature: &[u8; 65],
    pending_bump: u8,
    digest_bump: u8,
) -> Vec<u8> {
    // Wire shape: 1-byte discriminator + 145-byte body (see
    // submit_observations.rs SUBMIT_DATA_LEN).
    let mut data = Vec::with_capacity(1 + 145);
    data.push(IxDiscriminator::SubmitObservations as u8);
    data.extend_from_slice(&chain.to_be_bytes());
    data.extend_from_slice(emitter);
    data.extend_from_slice(&sequence.to_be_bytes());
    data.extend_from_slice(digest);
    data.extend_from_slice(&guardian_set_index.to_le_bytes());
    data.push(guardian_index);
    data.extend_from_slice(signature);
    data.push(pending_bump);
    data.push(digest_bump);
    data
}

fn close_pending_ix_data(emitter: &[u8; 32], sequence: u64) -> Vec<u8> {
    // Wire shape: 1-byte discriminator + 32-byte emitter + 8-byte big-endian
    // sequence. Mirrors `close_pending.rs::CLOSE_PENDING_DATA_LEN`. The
    // emitter / sequence drive both the canonical pending-PDA address check
    // and the (real-CPI branch's) bitmap-bit index.
    let mut data = Vec::with_capacity(1 + 32 + 8);
    data.push(IxDiscriminator::ClosePending as u8);
    data.extend_from_slice(emitter);
    data.extend_from_slice(&sequence.to_be_bytes());
    data
}

// ============================================================================
// Account fixtures
// ============================================================================

fn system_owned_account(lamports: u64) -> Account {
    Account {
        lamports,
        data: vec![],
        owner: system_program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn uninitialised_pda_account() -> Account {
    system_owned_account(0)
}

/// 1-byte NoReplay bucket: `0x00` = unmarked, `0x01` = marked.
///
/// Owned by our program ID so the mock's `mark_used` path can write to the
/// data buffer (the runtime forbids writes to accounts the program does not
/// own). The real CPI path will use the actual NoReplay program ID; the mock
/// is just a stand-in.
fn noreplay_bucket_unmarked() -> Account {
    Account {
        lamports: 1_000_000,
        data: vec![0u8; 1],
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn noreplay_bucket_marked() -> Account {
    Account {
        lamports: 1_000_000,
        data: vec![0x01u8; 1],
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

/// Build a Core-Bridge-style `GuardianSet` account with the supplied
/// Ethereum-style 20-byte guardian addresses, `creation_time`, and
/// `expiration_time`.
fn guardian_set_account(
    index: u32,
    keys: &[[u8; 20]],
    creation_time: u32,
    expiration_time: u32,
) -> Account {
    let mut data = Vec::with_capacity(8 + keys.len() * 20 + 8);
    data.extend_from_slice(&index.to_le_bytes());
    data.extend_from_slice(&(keys.len() as u32).to_le_bytes());
    for key in keys {
        data.extend_from_slice(key);
    }
    data.extend_from_slice(&creation_time.to_le_bytes());
    data.extend_from_slice(&expiration_time.to_le_bytes());
    Account {
        lamports: 1_000_000,
        data,
        owner: Pubkey::new_from_array([0xCC; 32]), // Core Bridge placeholder
        executable: false,
        rent_epoch: 0,
    }
}

// ============================================================================
// Guardian-set generation: deterministic per-test seeded keys.
// ============================================================================

#[derive(Clone)]
struct Guardian {
    secret: SecretKey,
    eth_address: [u8; 20],
}

/// Generate `count` deterministic secp256k1 keypairs and their corresponding
/// 20-byte Ethereum-style addresses.
///
/// The seed is folded into a 32-byte SecretKey via repeated hashing so the
/// fixture is reproducible across test runs and machines without dragging in
/// `rand` for crypto-grade RNG.
fn make_guardians(count: usize, seed: u8) -> Vec<Guardian> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut sk_bytes = [0u8; 32];
        sk_bytes[0] = seed;
        sk_bytes[1] = i as u8;
        // Bias the high bits so the resulting scalar is well within the
        // secp256k1 group order. `libsecp256k1::SecretKey::parse` rejects 0
        // and values >= curve order; using small-magnitude bytes is safe.
        sk_bytes[31] = (i as u8).wrapping_add(1);

        let secret =
            SecretKey::parse(&sk_bytes).expect("deterministic seed inside secp256k1 group order");
        let public = PublicKey::from_secret_key(&secret);
        // `serialize()` emits 65 bytes: 0x04 prefix + 64 raw (X||Y). Strip
        // the prefix before keccak.
        let pk_uncompressed = public.serialize();
        let raw = &pk_uncompressed[1..];
        let hash = keccak256_host(raw);
        let mut eth_address = [0u8; 20];
        eth_address.copy_from_slice(&hash[12..]);
        out.push(Guardian { secret, eth_address });
    }
    out
}

/// Host-side keccak256 — only used to derive the guardian-set fixture's
/// Ethereum-style addresses. The on-chain program calls
/// `pinocchio::syscalls::sol_keccak256` directly.
fn keccak256_host(data: &[u8]) -> [u8; 32] {
    solana_keccak_hasher::hashv(&[data]).to_bytes()
}

fn sign_digest(guardian: &Guardian, digest: &[u8; 32]) -> [u8; 65] {
    let msg = Message::parse(digest);
    let (sig, rec) = sign(&msg, &guardian.secret);
    let sig_bytes = sig.serialize(); // 64-byte r||s
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig_bytes);
    out[64] = rec.serialize();
    out
}

// ============================================================================
// Scenario builder — one place to assemble the account list and submit a
// single observation. Returns the post-tx mollusk `InstructionResult` so
// individual tests can inspect resulting accounts.
// ============================================================================

#[derive(Clone)]
struct Scenario {
    chain: u16,
    emitter: [u8; 32],
    sequence: u64,
    digest: [u8; 32],
    guardian_set_index: u32,
    guardians: Vec<Guardian>,
    submitter: Pubkey,
    pending_pda: Pubkey,
    pending_bump: u8,
    digest_pda: Pubkey,
    digest_bump: u8,
    guardian_set_pubkey: Pubkey,
    noreplay_bucket_pubkey: Pubkey,
    noreplay_program_pubkey: Pubkey,
    /// Stand-in for the global-accountant-owned `noreplay-authority` PDA. In
    /// mollusk runs we never reach the real CPI (gated behind `mock-noreplay`),
    /// so the address only needs to be a stable distinct pubkey the runtime
    /// can include in the account list. Phase 2.3's e2e test
    /// (`surfpool_e2e_submit_observations_real_noreplay.rs`) exercises the
    /// real derivation and the CPI together.
    noreplay_authority_pubkey: Pubkey,
}

impl Scenario {
    fn new(guardian_count: usize, gsi: u32, seed: u8) -> Self {
        let chain: u16 = 2;
        let mut emitter = [0u8; 32];
        emitter[31] = 0x77;
        let sequence: u64 = 0x0000_0000_0000_0042;

        // Distinct digest from `lifecycle_inputs()` to avoid any cross-test
        // confusion when both fixtures coexist.
        let mut digest = [0u8; 32];
        for (i, byte) in digest.iter_mut().enumerate() {
            *byte = (i as u8).wrapping_add(0x10);
        }

        let guardians = make_guardians(guardian_count, seed);
        let submitter = Pubkey::new_from_array([0x11u8; 32]);
        let (pending_pda, pending_bump) = derive_pending_pda(chain, &emitter, sequence, &digest);
        let (digest_pda, digest_bump) = derive_digest_pda(chain, &emitter, sequence);

        Self {
            chain,
            emitter,
            sequence,
            digest,
            guardian_set_index: gsi,
            guardians,
            submitter,
            pending_pda,
            pending_bump,
            digest_pda,
            digest_bump,
            guardian_set_pubkey: Pubkey::new_from_array([0xC1u8; 32]),
            noreplay_bucket_pubkey: Pubkey::new_from_array([0xC2u8; 32]),
            noreplay_program_pubkey: Pubkey::new_from_array([0xC3u8; 32]),
            noreplay_authority_pubkey: Pubkey::new_from_array([0xC4u8; 32]),
        }
    }

    fn guardian_keys(&self) -> Vec<[u8; 20]> {
        self.guardians.iter().map(|g| g.eth_address).collect()
    }

    /// Submit one observation from `guardian_index` and return the resulting
    /// account list (key, Account) tuples. Subsequent calls pass the previous
    /// `accounts` so PDA state persists across observations.
    fn submit_once(
        &self,
        mollusk: &Mollusk,
        starting_accounts: Vec<(Pubkey, Account)>,
        guardian_index: u8,
    ) -> mollusk_svm::result::InstructionResult {
        let guardian = &self.guardians[guardian_index as usize];
        let signature = sign_digest(guardian, &self.digest);
        let ix = Instruction::new_with_bytes(
            program_id(),
            &submit_ix_data(
                self.chain,
                &self.emitter,
                self.sequence,
                &self.digest,
                self.guardian_set_index,
                guardian_index,
                &signature,
                self.pending_bump,
                self.digest_bump,
            ),
            vec![
                AccountMeta::new(self.submitter, true),
                AccountMeta::new(self.pending_pda, false),
                AccountMeta::new_readonly(self.guardian_set_pubkey, false),
                AccountMeta::new(self.noreplay_bucket_pubkey, false),
                AccountMeta::new(self.digest_pda, false),
                AccountMeta::new_readonly(system_program_id(), false),
                AccountMeta::new_readonly(self.noreplay_program_pubkey, false),
                AccountMeta::new_readonly(self.noreplay_authority_pubkey, false),
            ],
        );
        mollusk.process_instruction(&ix, &starting_accounts)
    }

    /// Build the initial 8-account list with all PDAs uninitialised.
    fn initial_accounts(&self) -> Vec<(Pubkey, Account)> {
        vec![
            (self.submitter, system_owned_account(50_000_000_000)),
            (self.pending_pda, uninitialised_pda_account()),
            (
                self.guardian_set_pubkey,
                guardian_set_account(self.guardian_set_index, &self.guardian_keys(), 0, 0),
            ),
            (self.noreplay_bucket_pubkey, noreplay_bucket_unmarked()),
            (self.digest_pda, uninitialised_pda_account()),
            keyed_account_for_system_program(),
            (
                self.noreplay_program_pubkey,
                system_owned_account(0),
            ),
            (
                self.noreplay_authority_pubkey,
                system_owned_account(0),
            ),
        ]
    }

    /// Run `n` observations sequentially from guardian indices `0..n`. Returns
    /// the resulting accounts after the last submission.
    fn submit_n(
        &self,
        mollusk: &Mollusk,
        n: u8,
    ) -> Vec<(Pubkey, Account)> {
        let mut accounts = self.initial_accounts();
        for i in 0..n {
            let result = self.submit_once(mollusk, accounts.clone(), i);
            assert!(
                matches!(result.program_result, ProgramResult::Success),
                "submit #{i} expected success, got {:?}",
                result.program_result
            );
            accounts = result.resulting_accounts.clone();
        }
        accounts
    }
}

fn find_account<'a>(
    accounts: &'a [(Pubkey, Account)],
    key: &Pubkey,
) -> &'a Account {
    &accounts
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("account {key} not in result list"))
        .1
}

// ============================================================================
// Tests
// ============================================================================

#[test]
fn submit_first_observation_creates_pending_pda() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x42);

    let accounts = scenario.submit_n(&mollusk, 1);

    let pending = find_account(&accounts, &scenario.pending_pda);
    assert_eq!(pending.owner, program_id(), "pending PDA owned by program");
    assert_eq!(
        pending.data.len(),
        PendingObservationsLayout::LEN,
        "pending PDA allocated to full layout length"
    );
    let layout: &PendingObservationsLayout = bytemuck::from_bytes(&pending.data);
    assert_eq!(layout.digest, scenario.digest, "digest persisted");
    assert_eq!(
        layout.guardian_set_index, scenario.guardian_set_index,
        "guardian_set_index persisted"
    );
    assert_eq!(layout.signatures, 0b1, "bit 0 set after first observation");
    assert_eq!(layout.chain, scenario.chain, "chain persisted");
    assert_eq!(layout.payer, scenario.submitter.to_bytes(), "submitter is the recorded payer");

    // No quorum yet -> digest PDA is untouched.
    let digest = find_account(&accounts, &scenario.digest_pda);
    assert!(
        digest.owner == system_program_id() && digest.data.is_empty(),
        "digest PDA must not be opened before quorum reach"
    );

    // No quorum yet -> NoReplay bucket is unmarked.
    let bucket = find_account(&accounts, &scenario.noreplay_bucket_pubkey);
    assert_eq!(bucket.data[0], 0u8, "NoReplay bucket still unmarked");
}

#[test]
fn submit_12_observations_accumulates_without_commit() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x43);

    let accounts = scenario.submit_n(&mollusk, 12);

    let pending = find_account(&accounts, &scenario.pending_pda);
    let layout: &PendingObservationsLayout = bytemuck::from_bytes(&pending.data);
    assert_eq!(
        layout.signatures.count_ones(),
        12,
        "bitmap has exactly 12 bits set after 12 observations"
    );
    assert_eq!(
        layout.signatures, 0b1111_1111_1111u32,
        "bits 0..12 set in low-to-high order"
    );

    // Quorum not yet reached: digest PDA still uninit, NoReplay still unmarked.
    let digest = find_account(&accounts, &scenario.digest_pda);
    assert!(digest.data.is_empty(), "digest PDA untouched at 12/19");
    let bucket = find_account(&accounts, &scenario.noreplay_bucket_pubkey);
    assert_eq!(bucket.data[0], 0u8, "NoReplay bucket still unmarked at 12/19");
}

#[test]
fn submit_13th_observation_reaches_quorum_and_commits() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x44);

    let accounts = scenario.submit_n(&mollusk, 13);

    // Pending PDA must be closed (lamports drained, owner reverted to system).
    let pending = find_account(&accounts, &scenario.pending_pda);
    assert_eq!(pending.lamports, 0, "pending PDA lamports drained on commit");
    assert_eq!(
        pending.owner,
        system_program_id(),
        "pending PDA reassigned to system program on close"
    );
    assert!(
        pending.data.is_empty(),
        "pending PDA data dropped on close, got {} bytes",
        pending.data.len()
    );

    // DigestAccount must exist with the expected fields.
    let digest = find_account(&accounts, &scenario.digest_pda);
    assert_eq!(
        digest.owner,
        program_id(),
        "digest PDA owned by program after quorum"
    );
    assert_eq!(
        digest.data.len(),
        DigestAccountLayout::LEN,
        "digest PDA allocated to full layout length"
    );
    let stored: &DigestAccountLayout = bytemuck::from_bytes(&digest.data);
    assert_eq!(stored.digest, scenario.digest);
    assert_eq!(stored.chain, scenario.chain);
    assert_eq!(stored.emitter, scenario.emitter);
    assert_eq!(stored.sequence, scenario.sequence);
    assert_eq!(stored.guardian_set_index, scenario.guardian_set_index);
    assert_eq!(
        stored.payer,
        scenario.submitter.to_bytes(),
        "digest payer = submitter of the quorum-completing tx"
    );

    // NoReplay must be flipped to marked.
    let bucket = find_account(&accounts, &scenario.noreplay_bucket_pubkey);
    assert_eq!(
        bucket.data[0], 0x01,
        "NoReplay bucket flipped to marked on quorum reach"
    );
}

#[test]
fn submit_with_invalid_signature_fails() {
    // Take a valid signature, flip a byte, expect InvalidSignature.
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x45);

    let mut signature = sign_digest(&scenario.guardians[0], &scenario.digest);
    signature[0] ^= 0xff;

    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(
            scenario.chain,
            &scenario.emitter,
            scenario.sequence,
            &scenario.digest,
            scenario.guardian_set_index,
            0,
            &signature,
            scenario.pending_bump,
            scenario.digest_bump,
        ),
        vec![
            AccountMeta::new(scenario.submitter, true),
            AccountMeta::new(scenario.pending_pda, false),
            AccountMeta::new_readonly(scenario.guardian_set_pubkey, false),
            AccountMeta::new(scenario.noreplay_bucket_pubkey, false),
            AccountMeta::new(scenario.digest_pda, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(scenario.noreplay_program_pubkey, false),
            AccountMeta::new_readonly(scenario.noreplay_authority_pubkey, false),
        ],
    );
    let result = mollusk.process_instruction(&ix, &scenario.initial_accounts());
    match result.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidSignature as u32,
                "expected InvalidSignature, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidSignature), got {other:?}"),
    }
}

#[test]
fn submit_with_duplicate_guardian_index_fails() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x46);

    // First observation: succeeds.
    let mut accounts = scenario.initial_accounts();
    let r1 = scenario.submit_once(&mollusk, accounts.clone(), 0);
    assert!(matches!(r1.program_result, ProgramResult::Success));
    accounts = r1.resulting_accounts.clone();

    // Second observation from the *same* guardian index: must fail with
    // AlreadySigned.
    let r2 = scenario.submit_once(&mollusk, accounts, 0);
    match r2.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::AlreadySigned as u32,
                "expected AlreadySigned, got {code:?}"
            );
        }
        other => panic!("expected Failure(AlreadySigned), got {other:?}"),
    }
}

#[test]
fn submit_with_stale_old_set_observation_fails() {
    // Build a pending PDA under GSI=5 with one observation, then attempt to
    // submit an observation from GSI=4 — should fail with StaleGuardianSet.
    let mollusk = mollusk();
    let new_scenario = Scenario::new(19, 5, 0x47);
    let accounts_after_first = new_scenario.submit_n(&mollusk, 1);

    let old_guardians = make_guardians(19, 0x48); // distinct keys for GSI=4
    let stale_signature = sign_digest(&old_guardians[1], &new_scenario.digest);

    // Build the GSI=4 guardian-set account at the SAME pubkey so the program
    // reads the matching index from the supplied account.
    let old_gs_account = guardian_set_account(
        4,
        &old_guardians.iter().map(|g| g.eth_address).collect::<Vec<_>>(),
        0,
        0,
    );
    let mut accounts = accounts_after_first.clone();
    // Replace the guardian-set account: same pubkey, new index 4 payload.
    if let Some(entry) = accounts
        .iter_mut()
        .find(|(k, _)| *k == new_scenario.guardian_set_pubkey)
    {
        entry.1 = old_gs_account;
    }

    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(
            new_scenario.chain,
            &new_scenario.emitter,
            new_scenario.sequence,
            &new_scenario.digest,
            4, // stale index
            1,
            &stale_signature,
            new_scenario.pending_bump,
            new_scenario.digest_bump,
        ),
        vec![
            AccountMeta::new(new_scenario.submitter, true),
            AccountMeta::new(new_scenario.pending_pda, false),
            AccountMeta::new_readonly(new_scenario.guardian_set_pubkey, false),
            AccountMeta::new(new_scenario.noreplay_bucket_pubkey, false),
            AccountMeta::new(new_scenario.digest_pda, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(new_scenario.noreplay_program_pubkey, false),
            AccountMeta::new_readonly(new_scenario.noreplay_authority_pubkey, false),
        ],
    );
    let r = mollusk.process_instruction(&ix, &accounts);
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::StaleGuardianSet as u32,
                "expected StaleGuardianSet, got {code:?}"
            );
        }
        other => panic!("expected Failure(StaleGuardianSet), got {other:?}"),
    }
}

#[test]
fn submit_with_new_set_observation_wipes_old_pending() {
    // Set up a pending PDA under GSI=4 with 1 observation, then submit under
    // GSI=5 — the PDA must be wiped and recreated with bit-0 set under the new
    // index. Old payer's rent is forfeit to the new submitter per the
    // implementation note in `wipe_pending_pda`.
    let mollusk = mollusk();
    let old_scenario = Scenario::new(19, 4, 0x4A);
    let accounts_after_first = old_scenario.submit_n(&mollusk, 1);

    // Switch to new guardians under GSI=5 and submit. Same emitter / sequence
    // / chain as old_scenario so the pending PDA address collides intentionally.
    let new_guardians = make_guardians(19, 0x4B);
    let new_gs_account = guardian_set_account(
        5,
        &new_guardians.iter().map(|g| g.eth_address).collect::<Vec<_>>(),
        0,
        0,
    );
    let mut accounts = accounts_after_first.clone();
    if let Some(entry) = accounts
        .iter_mut()
        .find(|(k, _)| *k == old_scenario.guardian_set_pubkey)
    {
        entry.1 = new_gs_account;
    }

    let signature = sign_digest(&new_guardians[0], &old_scenario.digest);
    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(
            old_scenario.chain,
            &old_scenario.emitter,
            old_scenario.sequence,
            &old_scenario.digest,
            5,
            0,
            &signature,
            old_scenario.pending_bump,
            old_scenario.digest_bump,
        ),
        vec![
            AccountMeta::new(old_scenario.submitter, true),
            AccountMeta::new(old_scenario.pending_pda, false),
            AccountMeta::new_readonly(old_scenario.guardian_set_pubkey, false),
            AccountMeta::new(old_scenario.noreplay_bucket_pubkey, false),
            AccountMeta::new(old_scenario.digest_pda, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(old_scenario.noreplay_program_pubkey, false),
            AccountMeta::new_readonly(old_scenario.noreplay_authority_pubkey, false),
        ],
    );
    let r = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(r.program_result, ProgramResult::Success),
        "rotation submit must succeed, got {:?}",
        r.program_result
    );

    let pending = find_account(&r.resulting_accounts, &old_scenario.pending_pda);
    assert_eq!(pending.owner, program_id(), "pending PDA still owned by program after rotation");
    let layout: &PendingObservationsLayout = bytemuck::from_bytes(&pending.data);
    assert_eq!(
        layout.guardian_set_index, 5,
        "guardian_set_index advanced to the new set"
    );
    assert_eq!(
        layout.signatures, 0b1,
        "bitmap reset to a single bit under the new set"
    );
}

#[test]
fn submit_with_different_digest_under_same_set_creates_sibling_bucket() {
    // §3.1 (per-digest PDA seeds) and §3.3 (digest-mismatch under the same
    // guardian set creates a sibling bucket, not a rejection). Source-chain
    // reorgs that change the VAA body's `timestamp` produce a different digest
    // for the same `(chain, emitter, sequence)`; the new design routes those
    // observations into a distinct pending PDA whose seeds include the digest,
    // letting both digests race to quorum independently.
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x4C);

    // First observation under digest D1 — succeeds in the D1 bucket.
    let accounts_after_first = scenario.submit_n(&mollusk, 1);

    // Second observation under digest D2 with same GSI. Must SUCCEED into a
    // sibling PDA at a different canonical address (derived from the new
    // digest).
    let mut alternate_digest = scenario.digest;
    alternate_digest[0] ^= 0xff;
    let signature = sign_digest(&scenario.guardians[1], &alternate_digest);

    let (d2_pending_pda, d2_pending_bump) = derive_pending_pda(
        scenario.chain,
        &scenario.emitter,
        scenario.sequence,
        &alternate_digest,
    );
    assert_ne!(
        d2_pending_pda, scenario.pending_pda,
        "per-digest PDA seeds must yield distinct addresses for distinct digests"
    );

    // Add the D2 pending PDA as an uninitialised slot in the account list.
    let mut accounts = accounts_after_first.clone();
    accounts.push((d2_pending_pda, uninitialised_pda_account()));

    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(
            scenario.chain,
            &scenario.emitter,
            scenario.sequence,
            &alternate_digest,
            scenario.guardian_set_index,
            1,
            &signature,
            d2_pending_bump,
            scenario.digest_bump,
        ),
        vec![
            AccountMeta::new(scenario.submitter, true),
            AccountMeta::new(d2_pending_pda, false),
            AccountMeta::new_readonly(scenario.guardian_set_pubkey, false),
            AccountMeta::new(scenario.noreplay_bucket_pubkey, false),
            AccountMeta::new(scenario.digest_pda, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(scenario.noreplay_program_pubkey, false),
            AccountMeta::new_readonly(scenario.noreplay_authority_pubkey, false),
        ],
    );
    let r = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(r.program_result, ProgramResult::Success),
        "different-digest submit under same GSI must succeed into a sibling \
         bucket, got {:?}",
        r.program_result
    );

    // D1 bucket untouched at 1 sig.
    let d1 = find_account(&r.resulting_accounts, &scenario.pending_pda);
    let d1_layout: &PendingObservationsLayout = bytemuck::from_bytes(&d1.data);
    assert_eq!(d1_layout.digest, scenario.digest);
    assert_eq!(d1_layout.signatures, 0b1, "D1 bucket still at one signature");

    // D2 bucket has one sig at guardian-index 1.
    let d2 = find_account(&r.resulting_accounts, &d2_pending_pda);
    assert_eq!(d2.owner, program_id(), "D2 sibling PDA owned by program");
    let d2_layout: &PendingObservationsLayout = bytemuck::from_bytes(&d2.data);
    assert_eq!(d2_layout.digest, alternate_digest, "D2 digest persisted");
    assert_eq!(
        d2_layout.signatures, 0b10,
        "D2 bucket has bit 1 set (only guardian-index 1 has signed)"
    );
    assert_eq!(
        d2_layout.guardian_set_index, scenario.guardian_set_index,
        "D2 bucket recorded the active guardian set"
    );
}

/// Fork-recovery test. Documents the digest-in-seeds choice that substitutes
/// for CosmWasm's `tx_hash` bucket discriminator.
///
/// CosmWasm's pending state keys buckets by `(guardian_set_index, digest,
/// tx_hash)`. The Solana port omits `tx_hash` entirely and seeds the pending
/// PDA with `(b"pending", chain_be, emitter, sequence_be, digest)`. The digest
/// alone is a sufficient substitute because:
///
/// - Source-chain reorgs that change the body timestamp produce a different
///   digest. This test demonstrates that the two digests accumulate in
///   separate sibling PDAs and race to quorum, with the loser cleaned up
///   permissionlessly via `close_pending` trigger (b) once NoReplay marks
///   the shared `(chain, emitter, sequence)`.
/// - Reorgs that preserve the timestamp produce the same digest. Guardians
///   sign the digest, not the tx_hash, so their signatures merge cleanly
///   into one bucket — accelerating rather than blocking quorum. Accounting
///   commits the correct digest in either case.
/// - NoReplay is keyed by `(chain, emitter, sequence)` only. Including
///   tx_hash in that namespace would break replay protection (per-tx_hash
///   bits would never collide and a replay could land at a different bit).
///   The accountant operates on message identity, not on the source-chain
///   transaction that emitted it.
///
/// The CosmWasm `tx_hash` split was an audit-trail aid (recording which
/// source-chain tx contributed each signature). The Solana port treats that
/// as out of scope for the on-chain accountant — downstream indexers can
/// reconstruct it from emission events if required.
#[test]
fn fork_recovery_different_digest_same_seq_under_same_set_both_accumulate() {
    // Source-chain reorg recovery scenario.
    //
    // 1. Pre-reorg: 7 guardians observe digest D1 for `(chain=2, emitter, N)`
    //    under guardian set 6. Pending PDA at the D1-seeded address holds 7
    //    bits.
    // 2. Reorg flips the source chain's body timestamp; 13 guardians now
    //    observe digest D2 for the same `(chain, emitter, sequence)` under the
    //    same set 6.
    // 3. The first D2 observation creates a fresh sibling PDA at the
    //    D2-seeded address.
    // 4. Subsequent D2 observations accumulate.
    // 5. The 13th D2 observation reaches quorum, flips NoReplay for
    //    `(chain, emitter, sequence)` (shared across siblings), opens the
    //    DigestAccount with D2, and closes the D2 pending PDA.
    // 6. The D1 pending PDA (still at 7 sigs) remains stranded.

    let mollusk = mollusk();
    let scenario = Scenario::new(19, 6, 0x5A);

    // (1) Accumulate 7 D1 signatures in the D1 bucket.
    let accounts_after_d1 = scenario.submit_n(&mollusk, 7);
    let d1_after = find_account(&accounts_after_d1, &scenario.pending_pda);
    let d1_layout: &PendingObservationsLayout = bytemuck::from_bytes(&d1_after.data);
    assert_eq!(
        d1_layout.signatures.count_ones(),
        7,
        "D1 bucket accumulated 7 sigs before reorg"
    );

    // (2 + 3) Switch to D2; first D2 observation must create a fresh PDA
    // (different address) under the same GSI.
    let mut alternate_digest = scenario.digest;
    alternate_digest[0] ^= 0xa5;
    alternate_digest[31] ^= 0x5a;

    let (d2_pending_pda, d2_pending_bump) = derive_pending_pda(
        scenario.chain,
        &scenario.emitter,
        scenario.sequence,
        &alternate_digest,
    );
    assert_ne!(d2_pending_pda, scenario.pending_pda);

    let mut accounts = accounts_after_d1.clone();
    accounts.push((d2_pending_pda, uninitialised_pda_account()));

    // (3 + 4) Drive 13 distinct D2 guardians: indices 0..13 from the same
    // set 6 guardian fixture, just signing D2 instead of D1.
    for i in 0..13u8 {
        let signature = sign_digest(&scenario.guardians[i as usize], &alternate_digest);
        let ix = Instruction::new_with_bytes(
            program_id(),
            &submit_ix_data(
                scenario.chain,
                &scenario.emitter,
                scenario.sequence,
                &alternate_digest,
                scenario.guardian_set_index,
                i,
                &signature,
                d2_pending_bump,
                scenario.digest_bump,
            ),
            vec![
                AccountMeta::new(scenario.submitter, true),
                AccountMeta::new(d2_pending_pda, false),
                AccountMeta::new_readonly(scenario.guardian_set_pubkey, false),
                AccountMeta::new(scenario.noreplay_bucket_pubkey, false),
                AccountMeta::new(scenario.digest_pda, false),
                AccountMeta::new_readonly(system_program_id(), false),
                AccountMeta::new_readonly(scenario.noreplay_program_pubkey, false),
                AccountMeta::new_readonly(scenario.noreplay_authority_pubkey, false),
            ],
        );
        let r = mollusk.process_instruction(&ix, &accounts);
        assert!(
            matches!(r.program_result, ProgramResult::Success),
            "D2 submit #{i} expected success, got {:?}",
            r.program_result
        );
        accounts = r.resulting_accounts.clone();
    }

    // (5) Post-quorum assertions:
    //   - NoReplay flipped for the shared `(chain, emitter, sequence)`.
    //   - DigestAccount opened with D2 (not D1).
    //   - D2 pending PDA closed (lamports drained, system-owned).
    //   - D1 pending PDA still exists with its 7 sigs.
    let bucket = find_account(&accounts, &scenario.noreplay_bucket_pubkey);
    assert_eq!(
        bucket.data[0], 0x01,
        "NoReplay flipped on D2 quorum reach (shared bucket across siblings)"
    );

    let digest_acc = find_account(&accounts, &scenario.digest_pda);
    assert_eq!(
        digest_acc.owner,
        program_id(),
        "DigestAccount opened after D2 quorum reach"
    );
    let digest_layout: &DigestAccountLayout = bytemuck::from_bytes(&digest_acc.data);
    assert_eq!(
        digest_layout.digest, alternate_digest,
        "DigestAccount records the winning digest D2 (not D1)"
    );

    let d2_post = find_account(&accounts, &d2_pending_pda);
    assert_eq!(d2_post.lamports, 0, "D2 pending PDA drained on commit");
    assert!(d2_post.data.is_empty(), "D2 pending PDA closed");
    assert_eq!(d2_post.owner, system_program_id());

    let d1_post = find_account(&accounts, &scenario.pending_pda);
    assert_eq!(
        d1_post.owner,
        program_id(),
        "D1 pending PDA stranded but still owned by program"
    );
    let d1_post_layout: &PendingObservationsLayout = bytemuck::from_bytes(&d1_post.data);
    assert_eq!(
        d1_post_layout.signatures.count_ones(),
        7,
        "D1 pending PDA still holds 7 sigs after D2 sibling reached quorum"
    );
    assert_eq!(
        d1_post_layout.digest, scenario.digest,
        "D1 pending PDA still records the original digest"
    );
}

#[test]
fn close_pending_stranded_digest_bucket_after_sibling_committed() {
    // After the fork-recovery scenario, the D1 pending PDA is unreachable: a
    // sibling D2 bucket reached quorum and flipped NoReplay for the shared
    // `(chain, emitter, sequence)`. Trigger (b) of `close_pending` —
    // "NoReplay-marked" — must reclaim the D1 rent. Mirrors the source-chain
    // reorg cleanup story described in
    // `accountant-migration-pending-quorum-design.md` §3.6.

    let mollusk = mollusk();
    let scenario = Scenario::new(19, 6, 0x5B);
    let accounts_after_d1 = scenario.submit_n(&mollusk, 7);

    // Simulate the D2 sibling having reached quorum by flipping NoReplay
    // directly. The fork-recovery test above exercises the full path; this
    // test isolates the cleanup half.
    let mut accounts = accounts_after_d1.clone();
    if let Some(entry) = accounts
        .iter_mut()
        .find(|(k, _)| *k == scenario.noreplay_bucket_pubkey)
    {
        entry.1 = noreplay_bucket_marked();
    }

    // Trigger (b): NoReplay-marked. The D1 pending PDA is closed; rent goes
    // to the recorded payer (= submitter).
    let ix = Instruction::new_with_bytes(
        program_id(),
        &close_pending_ix_data(&scenario.emitter, scenario.sequence),
        vec![
            AccountMeta::new_readonly(scenario.submitter, true),
            AccountMeta::new(scenario.pending_pda, false),
            AccountMeta::new(scenario.submitter, false),
            AccountMeta::new_readonly(scenario.guardian_set_pubkey, false),
            AccountMeta::new_readonly(scenario.noreplay_bucket_pubkey, false),
        ],
    );
    let r = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(r.program_result, ProgramResult::Success),
        "close_pending(NoReplay-marked) on stranded D1 bucket must succeed, \
         got {:?}",
        r.program_result
    );

    let d1 = find_account(&r.resulting_accounts, &scenario.pending_pda);
    assert_eq!(d1.lamports, 0, "stranded D1 lamports drained to recorded payer");
    assert!(d1.data.is_empty(), "stranded D1 data dropped");
    assert_eq!(d1.owner, system_program_id());
}

#[test]
fn submit_rejected_when_noreplay_already_marked() {
    // Replay-protection pre-check: NoReplay marked => submit aborts before
    // signature verification or PDA work.
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x4D);

    let mut accounts = scenario.initial_accounts();
    // Flip the NoReplay bucket to marked.
    if let Some(entry) = accounts
        .iter_mut()
        .find(|(k, _)| *k == scenario.noreplay_bucket_pubkey)
    {
        entry.1 = noreplay_bucket_marked();
    }

    let r = scenario.submit_once(&mollusk, accounts, 0);
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::AlreadyAccounted as u32,
                "expected AlreadyAccounted, got {code:?}"
            );
        }
        other => panic!("expected Failure(AlreadyAccounted), got {other:?}"),
    }
}

// ============================================================================
// close_pending tests
// ============================================================================

#[test]
fn close_pending_with_expired_set_succeeds() {
    // Trigger (a) covers two sub-cases:
    //   (a1) The supplied GS PDA's index *matches* the pending's recorded
    //        index but its `expiration_time` is past — set has rolled over
    //        recently.
    //   (a2) The supplied GS PDA's index *differs* from the recorded one —
    //        the current set is not the pending's set, which is a strict
    //        superset of "expired".
    //
    // We cover (a2) here because mollusk's `Clock` sysvar has
    // `unix_timestamp = 0` by default, which makes (a1) impossible to drive
    // from a host test without a custom clock fixture.
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x4E);
    let accounts_after_first = scenario.submit_n(&mollusk, 1);

    // Swap in a GS account with a different index — this is the (a2) case.
    let expired_gs = guardian_set_account(
        5, // current set != pending's recorded set
        &scenario.guardian_keys(),
        0,
        0,
    );
    let mut accounts = accounts_after_first.clone();
    if let Some(entry) = accounts
        .iter_mut()
        .find(|(k, _)| *k == scenario.guardian_set_pubkey)
    {
        entry.1 = expired_gs;
    }

    let closer = scenario.submitter; // recorded payer = submitter
    let ix = Instruction::new_with_bytes(
        program_id(),
        &close_pending_ix_data(&scenario.emitter, scenario.sequence),
        vec![
            AccountMeta::new_readonly(closer, true),
            AccountMeta::new(scenario.pending_pda, false),
            AccountMeta::new(scenario.submitter, false), // rent recipient = payer
            AccountMeta::new_readonly(scenario.guardian_set_pubkey, false),
            AccountMeta::new_readonly(scenario.noreplay_bucket_pubkey, false),
        ],
    );
    let r = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(r.program_result, ProgramResult::Success),
        "close_pending(expired set) must succeed, got {:?}",
        r.program_result
    );

    let pending = find_account(&r.resulting_accounts, &scenario.pending_pda);
    assert_eq!(pending.lamports, 0, "pending lamports drained");
    assert!(pending.data.is_empty(), "pending data dropped");
    assert_eq!(pending.owner, system_program_id());
}

#[test]
fn close_pending_with_noreplay_marked_succeeds() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x4F);
    let accounts_after_first = scenario.submit_n(&mollusk, 1);

    // Flip the NoReplay bucket to marked (simulating a quorum reached via
    // some other path).
    let mut accounts = accounts_after_first.clone();
    if let Some(entry) = accounts
        .iter_mut()
        .find(|(k, _)| *k == scenario.noreplay_bucket_pubkey)
    {
        entry.1 = noreplay_bucket_marked();
    }

    let ix = Instruction::new_with_bytes(
        program_id(),
        &close_pending_ix_data(&scenario.emitter, scenario.sequence),
        vec![
            AccountMeta::new_readonly(scenario.submitter, true),
            AccountMeta::new(scenario.pending_pda, false),
            AccountMeta::new(scenario.submitter, false),
            AccountMeta::new_readonly(scenario.guardian_set_pubkey, false),
            AccountMeta::new_readonly(scenario.noreplay_bucket_pubkey, false),
        ],
    );
    let r = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(r.program_result, ProgramResult::Success),
        "close_pending(noreplay-marked) must succeed, got {:?}",
        r.program_result
    );

    let pending = find_account(&r.resulting_accounts, &scenario.pending_pda);
    assert_eq!(pending.lamports, 0);
    assert!(pending.data.is_empty());
}

#[test]
fn close_pending_with_active_set_and_no_noreplay_fails() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x50);
    let accounts_after_first = scenario.submit_n(&mollusk, 1);

    // Active set (expiration_time = 0), NoReplay unmarked — close must fail
    // with CannotCleanup.
    let ix = Instruction::new_with_bytes(
        program_id(),
        &close_pending_ix_data(&scenario.emitter, scenario.sequence),
        vec![
            AccountMeta::new_readonly(scenario.submitter, true),
            AccountMeta::new(scenario.pending_pda, false),
            AccountMeta::new(scenario.submitter, false),
            AccountMeta::new_readonly(scenario.guardian_set_pubkey, false),
            AccountMeta::new_readonly(scenario.noreplay_bucket_pubkey, false),
        ],
    );
    let r = mollusk.process_instruction(&ix, &accounts_after_first);
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::CannotCleanup as u32,
                "expected CannotCleanup, got {code:?}"
            );
        }
        other => panic!("expected Failure(CannotCleanup), got {other:?}"),
    }
}
