//! Mollusk integration tests for `submit_observations`, with the real `solana_noreplay.so`
//! (see `common::mollusk_fixtures`).

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        BalanceAccountLayout, ChainRegistrationLayout, GlobalAccountantError,
        Instruction as IxDiscriminator, NoReplayBitmapAccount, PendingObservationsLayout, Uint256,
        ACCOUNT_SEED_PREFIX, CHAIN_REGISTRATION_SEED_PREFIX, CORE_BRIDGE_PROGRAM_ID,
        GUARDIAN_SET_SEED, NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITS_PER_BUCKET,
        NOREPLAY_PROGRAM_ID, PENDING_OBSERVATIONS_SEED_PREFIX, SUBMIT_OBSERVATION_PREFIX,
    },
    libsecp256k1::{sign, Message, PublicKey, SecretKey},
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk},
    solana_account::Account,
    solana_instruction::{AccountMeta, Instruction},
    solana_program_error::ProgramError,
    solana_pubkey::Pubkey,
};

mod common;
use common::mollusk_fixtures::{keyed_account_for_noreplay_program, mollusk_with_fixtures};

const PROGRAM_NAME: &str = "global_accountant";

/// CU ceiling for the quorum-commit branch (Transfer + lazy init of both balance PDAs).
/// Measured 67,009 CU on anchor-lang 1.1.2; ceiling adds ~12% headroom.
const MAX_QUORUM_BRANCH_CU: u64 = 75_000;

fn program_id() -> Pubkey {
    // Fixed program id; PDA derivation must match the program.
    Pubkey::new_from_array([7u8; 32])
}

fn mollusk() -> Mollusk {
    mollusk_with_fixtures(&program_id(), PROGRAM_NAME)
}

fn system_program_id() -> Pubkey {
    keyed_account_for_system_program().0
}

fn derive_pending_pda(
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    digest: &[u8; 32],
) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    let sequence_be = sequence.to_be_bytes();
    Pubkey::find_program_address(
        &[
            PENDING_OBSERVATIONS_SEED_PREFIX,
            &chain_be,
            emitter,
            &sequence_be,
            digest,
        ],
        &program_id(),
    )
}

fn derive_balance_account_pda(
    chain: u16,
    token_chain: u16,
    token_address: &[u8; 32],
) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    let token_chain_be = token_chain.to_be_bytes();
    Pubkey::find_program_address(
        &[
            ACCOUNT_SEED_PREFIX,
            &chain_be,
            &token_chain_be,
            token_address,
        ],
        &program_id(),
    )
}

fn derive_chain_registration_pda(chain: u16) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    Pubkey::find_program_address(&[CHAIN_REGISTRATION_SEED_PREFIX, &chain_be], &program_id())
}

/// Host mirror of `noreplay::derive_bucket_pda`.
fn derive_canonical_noreplay_bucket(
    authority: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> Pubkey {
    let mut namespace = [0u8; 34];
    namespace[..2].copy_from_slice(&chain.to_be_bytes());
    namespace[2..].copy_from_slice(emitter);
    let bucket_index = (sequence / NOREPLAY_BITS_PER_BUCKET).to_le_bytes();
    let (pda, _) = Pubkey::find_program_address(
        &[
            authority.as_ref(),
            &namespace[..32],
            &namespace[32..],
            &bucket_index,
        ],
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
    );
    pda
}

/// Program-owned `ChainRegistration` PDA fixture.
fn chain_registration_account(chain: u16, emitter_address: &[u8; 32]) -> Account {
    let mut layout: ChainRegistrationLayout = bytemuck::Zeroable::zeroed();
    layout.tag = ChainRegistrationLayout::TAG;
    layout.chain = chain;
    layout.emitter_address = *emitter_address;
    Account {
        lamports: 1_000_000,
        data: bytemuck::bytes_of(&layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

/// Dedup digest `keccak256(keccak256(body))`.
fn double_keccak256_host(body: &[u8]) -> [u8; 32] {
    let inner = solana_keccak_hasher::hashv(&[body]).to_bytes();
    solana_keccak_hasher::hashv(&[&inner]).to_bytes()
}

/// Fixed source-chain transaction id for the signing digest.
const TX_HASH: [u8; 32] = [0xA9_u8; 32];

/// Host mirror of `hash::observation_signing_digest`.
fn observation_signing_digest_host(prefix: &[u8], tx_hash: &[u8; 32], body: &[u8]) -> [u8; 32] {
    solana_keccak_hasher::hashv(&[prefix, tx_hash, body]).to_bytes()
}

/// WTT signing digest for `body` under the canonical prefix and `TX_HASH`.
fn signing_digest_for(body: &[u8]) -> [u8; 32] {
    observation_signing_digest_host(SUBMIT_OBSERVATION_PREFIX, &TX_HASH, body)
}

/// Attest VAA body (action 0x02).
fn build_attest_body(emitter_chain: u16, emitter_address: &[u8; 32], sequence: u64) -> Vec<u8> {
    // 51-byte header + action byte.
    let mut body = vec![0u8; 52];
    body[8..10].copy_from_slice(&emitter_chain.to_be_bytes());
    body[10..42].copy_from_slice(emitter_address);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body[51] = 0x02;
    body
}

/// Token Bridge transfer VAA body (action 0x01).
fn build_transfer_body(
    emitter_chain: u16,
    emitter_address: &[u8; 32],
    sequence: u64,
    amount: u128,
    token_chain: u16,
    token_address: &[u8; 32],
    recipient_chain: u16,
) -> Vec<u8> {
    let mut body = vec![0u8; 51 + 133];
    body[8..10].copy_from_slice(&emitter_chain.to_be_bytes());
    body[10..42].copy_from_slice(emitter_address);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body[51] = 0x01; // transfer payload starts at offset 51
    body[52 + 16..52 + 32].copy_from_slice(&amount.to_be_bytes()); // amount: 32-byte BE, low 16 hold the u128
    body[84..116].copy_from_slice(token_address);
    body[116..118].copy_from_slice(&token_chain.to_be_bytes());
    body[118] = 0xAB; // recipient: opaque to the accountant
    body[149] = 0xCD;
    body[150..152].copy_from_slice(&recipient_chain.to_be_bytes());
    body
}

fn submit_ix_data(
    guardian_set_index: u32,
    guardian_index: u8,
    signature: &[u8; 65],
    body: &[u8],
) -> Vec<u8> {
    // Wire: discriminator + 70-byte prefix + tx_hash(32) + body_len(u16 LE) + body.
    let mut data = Vec::with_capacity(1 + 70 + 32 + 2 + body.len());
    data.push(IxDiscriminator::SubmitObservations as u8);
    data.extend_from_slice(&guardian_set_index.to_le_bytes());
    data.push(guardian_index);
    data.extend_from_slice(signature);
    data.extend_from_slice(&TX_HASH);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
    data
}

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

/// Uninitialised NoReplay bucket.
fn noreplay_bucket_unmarked() -> Account {
    system_owned_account(0)
}

/// NoReplay bucket with the bit at `sequence % 1024` set.
fn noreplay_bucket_marked(sequence: u64) -> Account {
    let mut account: NoReplayBitmapAccount = bytemuck::Zeroable::zeroed();
    let bit_index = (sequence % NOREPLAY_BITS_PER_BUCKET) as usize;
    account.bitmap[bit_index / 8] |= 1u8 << (bit_index % 8);
    Account {
        lamports: 1_500_000_000,
        data: bytemuck::bytes_of(&account).to_vec(),
        owner: Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        executable: false,
        rent_epoch: 0,
    }
}

/// Core Bridge `GuardianSet` account.
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
        owner: Pubkey::new_from_array(CORE_BRIDGE_PROGRAM_ID),
        executable: false,
        rent_epoch: 0,
    }
}

#[derive(Clone)]
struct Guardian {
    secret: SecretKey,
    eth_address: [u8; 20],
}

/// `count` deterministic secp256k1 keypairs and their 20-byte addresses.
fn make_guardians(count: usize, seed: u8) -> Vec<Guardian> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut sk_bytes = [0u8; 32];
        sk_bytes[0] = seed;
        sk_bytes[1] = i as u8;
        sk_bytes[31] = (i as u8).wrapping_add(1);

        let secret =
            SecretKey::parse(&sk_bytes).expect("deterministic seed inside secp256k1 group order");
        let public = PublicKey::from_secret_key(&secret);
        // Strip the 0x04 prefix.
        let pk_uncompressed = public.serialize();
        let raw = &pk_uncompressed[1..];
        let hash = keccak256_host(raw);
        let mut eth_address = [0u8; 20];
        eth_address.copy_from_slice(&hash[12..]);
        out.push(Guardian {
            secret,
            eth_address,
        });
    }
    out
}

fn keccak256_host(data: &[u8]) -> [u8; 32] {
    solana_keccak_hasher::hashv(&[data]).to_bytes()
}

fn sign_digest(guardian: &Guardian, digest: &[u8; 32]) -> [u8; 65] {
    let msg = Message::parse(digest);
    let (sig, rec) = sign(&msg, &guardian.secret);
    let sig_bytes = sig.serialize(); // 64-byte r||s + recovery byte
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig_bytes);
    out[64] = rec.serialize();
    out
}

#[derive(Clone)]
struct Scenario {
    chain: u16,
    emitter: [u8; 32],
    sequence: u64,
    /// VAA body; `digest` derives from it. Default is Attest.
    body: Vec<u8>,
    /// `double_keccak256(body)`.
    digest: [u8; 32],
    /// `keccak256(prefix ‖ tx_hash ‖ body)`.
    signing_digest: [u8; 32],
    guardian_set_index: u32,
    guardians: Vec<Guardian>,
    submitter: Pubkey,
    pending_pda: Pubkey,
    guardian_set_pubkey: Pubkey,
    noreplay_bucket_pubkey: Pubkey,
    noreplay_program_pubkey: Pubkey,
    noreplay_authority_pubkey: Pubkey,
    /// Slot 7. Attest scenarios use the authority pubkey as a placeholder.
    source_account_pubkey: Pubkey,
    /// Slot 8.
    dest_account_pubkey: Pubkey,
    /// Slot 10, pre-populated `chain -> emitter`.
    chain_registration_pubkey: Pubkey,
}

impl Scenario {
    fn new(guardian_count: usize, gsi: u32, seed: u8) -> Self {
        let chain: u16 = 2;
        let mut emitter = [0u8; 32];
        emitter[31] = 0x77;
        let sequence: u64 = 0x0000_0000_0000_0042;

        let body = build_attest_body(chain, &emitter, sequence);
        let digest = double_keccak256_host(&body);
        let signing_digest = signing_digest_for(&body);

        let guardians = make_guardians(guardian_count, seed);
        let submitter = Pubkey::new_from_array([0x11u8; 32]);
        let (pending_pda, _) = derive_pending_pda(chain, &emitter, sequence, &digest);
        let (noreplay_authority_pubkey, _) =
            Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
        let (chain_registration_pubkey, _) = derive_chain_registration_pda(chain);
        let noreplay_bucket_pubkey =
            derive_canonical_noreplay_bucket(&noreplay_authority_pubkey, chain, &emitter, sequence);
        let gsi_be = gsi.to_be_bytes();
        let (guardian_set_pubkey, _) = Pubkey::find_program_address(
            &[GUARDIAN_SET_SEED, &gsi_be],
            &Pubkey::new_from_array(CORE_BRIDGE_PROGRAM_ID),
        );

        Self {
            chain,
            emitter,
            sequence,
            body,
            digest,
            signing_digest,
            guardian_set_index: gsi,
            guardians,
            submitter,
            pending_pda,
            guardian_set_pubkey,
            noreplay_bucket_pubkey,
            noreplay_program_pubkey: Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
            noreplay_authority_pubkey,
            // Attest: slots 7/8 unused.
            source_account_pubkey: noreplay_authority_pubkey,
            dest_account_pubkey: noreplay_authority_pubkey,
            chain_registration_pubkey,
        }
    }

    /// Switch to a Transfer body; re-derive digest and PDAs.
    fn with_transfer_body(
        guardian_count: usize,
        gsi: u32,
        seed: u8,
        amount: u128,
        token_chain: u16,
        token_address: [u8; 32],
        recipient_chain: u16,
    ) -> Self {
        let mut base = Self::new(guardian_count, gsi, seed);
        base.body = build_transfer_body(
            base.chain,
            &base.emitter,
            base.sequence,
            amount,
            token_chain,
            &token_address,
            recipient_chain,
        );
        base.digest = double_keccak256_host(&base.body);
        base.signing_digest = signing_digest_for(&base.body);
        let (pending_pda, _) =
            derive_pending_pda(base.chain, &base.emitter, base.sequence, &base.digest);
        base.pending_pda = pending_pda;
        // Source keys on the emitter chain; dest keys on recipient_chain.
        let (src, _) = derive_balance_account_pda(base.chain, token_chain, &token_address);
        let (dst, _) = derive_balance_account_pda(recipient_chain, token_chain, &token_address);
        base.source_account_pubkey = src;
        base.dest_account_pubkey = dst;
        base
    }

    fn guardian_keys(&self) -> Vec<[u8; 20]> {
        self.guardians.iter().map(|g| g.eth_address).collect()
    }

    /// Submit one observation. Pass the previous result's accounts to carry state.
    fn submit_once(
        &self,
        mollusk: &Mollusk,
        starting_accounts: Vec<(Pubkey, Account)>,
        guardian_index: u8,
    ) -> mollusk_svm::result::InstructionResult {
        let guardian = &self.guardians[guardian_index as usize];
        let signature = sign_digest(guardian, &self.signing_digest);
        let ix = Instruction::new_with_bytes(
            program_id(),
            &submit_ix_data(
                self.guardian_set_index,
                guardian_index,
                &signature,
                &self.body,
            ),
            self.account_metas(),
        );
        mollusk.process_instruction(&ix, &starting_accounts)
    }

    /// 11-entry account list. Slot 9 is `rent_recipient` (= submitter).
    fn account_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.submitter, true),
            AccountMeta::new(self.pending_pda, false),
            AccountMeta::new_readonly(self.guardian_set_pubkey, false),
            AccountMeta::new(self.noreplay_bucket_pubkey, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(self.noreplay_program_pubkey, false),
            AccountMeta::new_readonly(self.noreplay_authority_pubkey, false),
            AccountMeta::new(self.source_account_pubkey, false),
            AccountMeta::new(self.dest_account_pubkey, false),
            AccountMeta::new(self.submitter, false),
            AccountMeta::new_readonly(self.chain_registration_pubkey, false),
        ]
    }

    /// Initial accounts with all PDAs uninitialised.
    fn initial_accounts(&self) -> Vec<(Pubkey, Account)> {
        let mut accounts = vec![
            (self.submitter, system_owned_account(50_000_000_000)),
            (self.pending_pda, uninitialised_pda_account()),
            (
                self.guardian_set_pubkey,
                guardian_set_account(self.guardian_set_index, &self.guardian_keys(), 0, 0),
            ),
            (self.noreplay_bucket_pubkey, noreplay_bucket_unmarked()),
            keyed_account_for_system_program(),
            keyed_account_for_noreplay_program(),
            (self.noreplay_authority_pubkey, system_owned_account(0)),
        ];
        // Slots 7/8 only when distinct from the authority placeholder.
        if self.source_account_pubkey != self.noreplay_authority_pubkey {
            accounts.push((self.source_account_pubkey, uninitialised_pda_account()));
        }
        if self.dest_account_pubkey != self.noreplay_authority_pubkey
            && self.dest_account_pubkey != self.source_account_pubkey
        {
            accounts.push((self.dest_account_pubkey, uninitialised_pda_account()));
        }
        accounts.push((
            self.chain_registration_pubkey,
            chain_registration_account(self.chain, &self.emitter),
        ));
        accounts
    }

    /// Run observations from guardian indices `0..n`.
    fn submit_n(&self, mollusk: &Mollusk, n: u8) -> Vec<(Pubkey, Account)> {
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

fn find_account<'a>(accounts: &'a [(Pubkey, Account)], key: &Pubkey) -> &'a Account {
    &accounts
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("account {key} not in result list"))
        .1
}

/// First observation allocates and populates the pending PDA.
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
    assert_eq!(
        layout.payer,
        scenario.submitter.to_bytes(),
        "submitter is the recorded payer"
    );

    let bucket = find_account(&accounts, &scenario.noreplay_bucket_pubkey);
    assert_eq!(
        bucket.owner,
        system_program_id(),
        "bucket still system-owned"
    );
    assert!(bucket.data.is_empty(), "bucket still uninitialised");
}

/// 12 observations (sub-quorum) accumulate in the bitmap without committing.
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

    let bucket = find_account(&accounts, &scenario.noreplay_bucket_pubkey);
    assert_eq!(
        bucket.owner,
        system_program_id(),
        "bucket still system-owned"
    );
    assert!(
        bucket.data.is_empty(),
        "bucket still uninitialised at 12/19"
    );
}

/// 13th observation reaches quorum: pending closes, commit-log emitted, NoReplay flips.
#[test]
fn submit_13th_observation_reaches_quorum_and_commits() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x44);

    let accounts = scenario.submit_n(&mollusk, 13);

    let pending = find_account(&accounts, &scenario.pending_pda);
    assert_eq!(
        pending.lamports, 0,
        "pending PDA lamports drained on commit"
    );
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

    // Mollusk hides program logs; the surfpool e2e suite checks the commit log.

    let bucket = find_account(&accounts, &scenario.noreplay_bucket_pubkey);
    assert_eq!(
        bucket.owner,
        Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        "bucket owned by solana_noreplay after MarkUsed"
    );
    let account = NoReplayBitmapAccount::from_bytes(&bucket.data)
        .expect("bucket data sized to bitmap layout");
    assert!(account.is_marked(scenario.sequence), "bitmap bit set");
}

/// Rent refunds to the recorded payer, not the completing signer.
/// A wrong `rent_recipient` fails with `PayerMismatch`.
#[test]
fn submit_observations_quorum_with_different_submitter_refunds_recorded_payer() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x70);

    let accounts_after_12 = scenario.submit_n(&mollusk, 12);
    let alice = scenario.submitter;
    let alice_lamports_pre = find_account(&accounts_after_12, &alice).lamports;
    let pending_lamports = find_account(&accounts_after_12, &scenario.pending_pda).lamports;
    assert!(
        pending_lamports > 0,
        "pending PDA must be rent-funded at 12/19"
    );

    let bob = Pubkey::new_from_array([0xB0u8; 32]);
    let bob_starting_lamports = 50_000_000_000u64;
    let mut accounts = accounts_after_12.clone();
    accounts.push((bob, system_owned_account(bob_starting_lamports)));

    let signature = sign_digest(&scenario.guardians[12], &scenario.signing_digest);
    let ix_data = submit_ix_data(scenario.guardian_set_index, 12, &signature, &scenario.body);

    // Wrong rent_recipient.
    let wrong_metas = vec![
        AccountMeta::new(bob, true),
        AccountMeta::new(scenario.pending_pda, false),
        AccountMeta::new_readonly(scenario.guardian_set_pubkey, false),
        AccountMeta::new(scenario.noreplay_bucket_pubkey, false),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new_readonly(scenario.noreplay_program_pubkey, false),
        AccountMeta::new_readonly(scenario.noreplay_authority_pubkey, false),
        AccountMeta::new(scenario.source_account_pubkey, false),
        AccountMeta::new(scenario.dest_account_pubkey, false),
        AccountMeta::new(bob, false), // rent_recipient = bob (wrong)
        AccountMeta::new_readonly(scenario.chain_registration_pubkey, false),
    ];
    let ix_wrong = Instruction::new_with_bytes(program_id(), &ix_data, wrong_metas);
    let r_wrong = mollusk.process_instruction(&ix_wrong, &accounts);
    match r_wrong.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::PayerMismatch as u32,
                "wrong rent_recipient must fail with PayerMismatch, got {code:?}"
            );
        }
        other => panic!("expected Failure(PayerMismatch), got {other:?}"),
    }

    // Correct rent_recipient.
    let correct_metas = vec![
        AccountMeta::new(bob, true),
        AccountMeta::new(scenario.pending_pda, false),
        AccountMeta::new_readonly(scenario.guardian_set_pubkey, false),
        AccountMeta::new(scenario.noreplay_bucket_pubkey, false),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new_readonly(scenario.noreplay_program_pubkey, false),
        AccountMeta::new_readonly(scenario.noreplay_authority_pubkey, false),
        AccountMeta::new(scenario.source_account_pubkey, false),
        AccountMeta::new(scenario.dest_account_pubkey, false),
        AccountMeta::new(alice, false), // rent_recipient = alice (correct)
        AccountMeta::new_readonly(scenario.chain_registration_pubkey, false),
    ];
    let ix_correct = Instruction::new_with_bytes(program_id(), &ix_data, correct_metas);
    let r_correct = mollusk.process_instruction(&ix_correct, &accounts);
    assert!(
        matches!(r_correct.program_result, ProgramResult::Success),
        "13th submission from a different submitter with correct rent_recipient \
         must succeed, got {:?}",
        r_correct.program_result
    );

    let alice_post = find_account(&r_correct.resulting_accounts, &alice);
    assert_eq!(
        alice_post.lamports,
        alice_lamports_pre + pending_lamports,
        "alice (recorded payer) received the pending-PDA rent refund"
    );
}

/// The routing tuple comes from body header `[8..50]`; a pending PDA for another
/// namespace is rejected.
#[test]
fn submit_observations_routes_by_body_header_not_caller_supplied_prefix() {
    let mollusk = mollusk();

    let body_chain = 2u16;
    let mut body_emitter = [0u8; 32];
    body_emitter[31] = 0x77;
    let body_sequence = 0x42u64;
    let body = build_attest_body(body_chain, &body_emitter, body_sequence);
    let digest = double_keccak256_host(&body);

    let guardians = make_guardians(19, 0x80);
    let signature = sign_digest(&guardians[0], &signing_digest_for(&body));

    // Pending PDA canonical for the attacker namespace, not the body's.
    let attacker_chain = 99u16;
    let attacker_emitter = [0xFFu8; 32];
    let attacker_sequence = 0x9999u64;
    let (attacker_pending_pda, _) = derive_pending_pda(
        attacker_chain,
        &attacker_emitter,
        attacker_sequence,
        &digest,
    );
    let (body_pending_pda, _) =
        derive_pending_pda(body_chain, &body_emitter, body_sequence, &digest);
    assert_ne!(
        body_pending_pda, attacker_pending_pda,
        "test fixture must drive distinct pending PDA addresses"
    );

    let submitter = Pubkey::new_from_array([0x11u8; 32]);
    let guardian_set_index = 4u32;
    let gsi_be = guardian_set_index.to_be_bytes();
    let (guardian_set_pubkey, _) = Pubkey::find_program_address(
        &[GUARDIAN_SET_SEED, &gsi_be],
        &Pubkey::new_from_array(CORE_BRIDGE_PROGRAM_ID),
    );
    let (noreplay_authority_pubkey, _) =
        Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
    let noreplay_bucket_pubkey = derive_canonical_noreplay_bucket(
        &noreplay_authority_pubkey,
        body_chain,
        &body_emitter,
        body_sequence,
    );
    let noreplay_program_pubkey = Pubkey::new_from_array(NOREPLAY_PROGRAM_ID);

    let ix_data = submit_ix_data(guardian_set_index, 0, &signature, &body);

    let (registration_pda, _) = derive_chain_registration_pda(body_chain);

    let guardian_keys: Vec<[u8; 20]> = guardians.iter().map(|g| g.eth_address).collect();
    let accounts = vec![
        (submitter, system_owned_account(50_000_000_000)),
        (attacker_pending_pda, uninitialised_pda_account()),
        (
            guardian_set_pubkey,
            guardian_set_account(4, &guardian_keys, 0, 0),
        ),
        (noreplay_bucket_pubkey, noreplay_bucket_unmarked()),
        keyed_account_for_system_program(),
        keyed_account_for_noreplay_program(),
        (noreplay_authority_pubkey, system_owned_account(0)),
        (
            registration_pda,
            chain_registration_account(body_chain, &body_emitter),
        ),
    ];

    let metas = vec![
        AccountMeta::new(submitter, true),
        AccountMeta::new(attacker_pending_pda, false),
        AccountMeta::new_readonly(guardian_set_pubkey, false),
        AccountMeta::new(noreplay_bucket_pubkey, false),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new_readonly(noreplay_program_pubkey, false),
        AccountMeta::new_readonly(noreplay_authority_pubkey, false),
        AccountMeta::new(noreplay_authority_pubkey, false),
        AccountMeta::new(noreplay_authority_pubkey, false),
        AccountMeta::new(submitter, false),
        AccountMeta::new_readonly(registration_pda, false),
    ];
    let ix = Instruction::new_with_bytes(program_id(), &ix_data, metas);
    let r = mollusk.process_instruction(&ix, &accounts);

    // The init CPI signs only for the body-derived address.
    assert!(
        !matches!(r.program_result, ProgramResult::Success),
        "spoofed pending PDA must be rejected, got {:?}",
        r.program_result
    );
    let result_debug = format!("{:?}", r.program_result);
    assert!(
        result_debug.contains("PrivilegeEscalation"),
        "spoofed pending PDA must reject with PrivilegeEscalation — \
         create_pending_pda signs only for the body-derived canonical address, \
         so the init CPI cannot sign for the attacker's address; got {result_debug}"
    );

    let attacker_after = find_account(&r.resulting_accounts, &attacker_pending_pda);
    assert_eq!(
        attacker_after.owner,
        system_program_id(),
        "attacker-supplied pending PDA must NOT be initialised after rejection"
    );
}

/// No registration PDA for `emitter_chain`: `MissingChainRegistration`.
#[test]
fn submit_observations_rejects_unregistered_chain() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0xA0);

    let (registration_pda, _) = derive_chain_registration_pda(scenario.chain);
    let signature = sign_digest(&scenario.guardians[0], &scenario.signing_digest);

    let mut accounts = scenario.initial_accounts();
    for entry in accounts.iter_mut() {
        if entry.0 == registration_pda {
            entry.1 = uninitialised_pda_account();
            break;
        }
    }

    let metas = vec![
        AccountMeta::new(scenario.submitter, true),
        AccountMeta::new(scenario.pending_pda, false),
        AccountMeta::new_readonly(scenario.guardian_set_pubkey, false),
        AccountMeta::new(scenario.noreplay_bucket_pubkey, false),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new_readonly(scenario.noreplay_program_pubkey, false),
        AccountMeta::new_readonly(scenario.noreplay_authority_pubkey, false),
        AccountMeta::new(scenario.source_account_pubkey, false),
        AccountMeta::new(scenario.dest_account_pubkey, false),
        AccountMeta::new(scenario.submitter, false),
        AccountMeta::new_readonly(registration_pda, false),
    ];
    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(scenario.guardian_set_index, 0, &signature, &scenario.body),
        metas,
    );
    let r = mollusk.process_instruction(&ix, &accounts);
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::MissingChainRegistration as u32,
                "expected MissingChainRegistration, got {code:?}"
            );
        }
        other => panic!("expected Failure(MissingChainRegistration), got {other:?}"),
    }
}

/// Registration holds another emitter: `UnregisteredEmitter`.
#[test]
fn submit_observations_rejects_wrong_emitter_for_registered_chain() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0xA1);

    let registration_pubkey = scenario.chain_registration_pubkey;
    let wrong_emitter = [0xCCu8; 32];
    let mut accounts = scenario.initial_accounts();
    for entry in accounts.iter_mut() {
        if entry.0 == registration_pubkey {
            entry.1 = chain_registration_account(scenario.chain, &wrong_emitter);
            break;
        }
    }

    let signature = sign_digest(&scenario.guardians[0], &scenario.signing_digest);
    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(scenario.guardian_set_index, 0, &signature, &scenario.body),
        scenario.account_metas(),
    );
    let r = mollusk.process_instruction(&ix, &accounts);
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::UnregisteredEmitter as u32,
                "expected UnregisteredEmitter, got {code:?}"
            );
        }
        other => panic!("expected Failure(UnregisteredEmitter), got {other:?}"),
    }
}

/// Wrong-seed registration PDA: `InvalidPda`.
#[test]
fn submit_observations_rejects_spoofed_registration_pda() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0xA2);

    let (spoofed_pda, _) = derive_chain_registration_pda(99);
    assert_ne!(spoofed_pda, scenario.chain_registration_pubkey);

    let mut accounts = scenario.initial_accounts();
    accounts.push((spoofed_pda, chain_registration_account(99, &[0xAA; 32])));

    let mut metas = scenario.account_metas();
    let last_idx = metas.len() - 1;
    metas[last_idx] = AccountMeta::new_readonly(spoofed_pda, false);

    let signature = sign_digest(&scenario.guardians[0], &scenario.signing_digest);
    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(scenario.guardian_set_index, 0, &signature, &scenario.body),
        metas,
    );
    let r = mollusk.process_instruction(&ix, &accounts);
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidPda as u32,
                "expected InvalidPda from canonical-address check, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidPda), got {other:?}"),
    }
}

/// Corrupted signature: `InvalidSignature`.
#[test]
fn submit_with_invalid_signature_fails() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x45);

    let mut signature = sign_digest(&scenario.guardians[0], &scenario.signing_digest);
    signature[0] ^= 0xff;

    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(scenario.guardian_set_index, 0, &signature, &scenario.body),
        scenario.account_metas(),
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

/// Recovery id >= 4: `InvalidSignature` before `secp256k1_recover`.
#[test]
fn submit_with_recovery_id_4_rejects() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x52);
    let mut signature = sign_digest(&scenario.guardians[0], &scenario.signing_digest);
    signature[64] = 4;

    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(scenario.guardian_set_index, 0, &signature, &scenario.body),
        scenario.account_metas(),
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

/// Same guardian index twice: `AlreadySigned`.
#[test]
fn submit_with_duplicate_guardian_index_fails() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x46);

    let mut accounts = scenario.initial_accounts();
    let r1 = scenario.submit_once(&mollusk, accounts.clone(), 0);
    assert!(matches!(r1.program_result, ProgramResult::Success));
    accounts = r1.resulting_accounts.clone();

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

/// Each `read_guardian_key` rejection:
///   (a) truncated header              -> InvalidPda
///   (b) on-chain index != wire index  -> InvalidGuardianIndex
///   (c) guardian_index >= keys_len    -> InvalidGuardianIndex
///   (d) keys array truncated          -> InvalidPda
#[test]
fn submit_with_malformed_guardian_set_rejects() {
    let mollusk = mollusk();
    let core_bridge = Pubkey::new_from_array(CORE_BRIDGE_PROGRAM_ID);

    let truncated_header = Account {
        lamports: 1_000_000,
        data: vec![0u8; 4], // < 8 bytes
        owner: core_bridge,
        executable: false,
        rent_epoch: 0,
    };

    let mismatched_index = {
        let scenario = Scenario::new(19, 4, 0x53);
        guardian_set_account(99, &scenario.guardian_keys(), 0, 0)
    };

    let short_keys_array = {
        // keys_len = 3, guardian_index = 5.
        let scenario = Scenario::new(19, 4, 0x53);
        let truncated: Vec<[u8; 20]> = scenario.guardians[..3]
            .iter()
            .map(|g| g.eth_address)
            .collect();
        guardian_set_account(4, &truncated, 0, 0)
    };

    let truncated_keys_buffer = {
        // keys_len = 19 but 5 keys present.
        let mut data = Vec::with_capacity(8 + 5 * 20);
        data.extend_from_slice(&4u32.to_le_bytes());
        data.extend_from_slice(&19u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 5 * 20]);
        Account {
            lamports: 1_000_000,
            data,
            owner: core_bridge,
            executable: false,
            rent_epoch: 0,
        }
    };

    // Compare the full `u64` so builtin error codes keep their high bits.
    let cases: [(&str, Account, u8, u64); 4] = [
        (
            "truncated header",
            truncated_header,
            0,
            u64::from(ProgramError::InvalidAccountData),
        ),
        (
            "on-chain index mismatch",
            mismatched_index,
            0,
            GlobalAccountantError::InvalidGuardianIndex as u64,
        ),
        (
            "guardian_index >= keys_len",
            short_keys_array,
            5,
            GlobalAccountantError::InvalidGuardianIndex as u64,
        ),
        (
            "keys buffer truncated",
            truncated_keys_buffer,
            18,
            u64::from(ProgramError::InvalidAccountData),
        ),
    ];

    for (label, gs_account, guardian_index, expected_code) in cases {
        let scenario = Scenario::new(19, 4, 0x53);
        let mut accounts = scenario.initial_accounts();
        if let Some(entry) = accounts
            .iter_mut()
            .find(|(k, _)| *k == scenario.guardian_set_pubkey)
        {
            entry.1 = gs_account;
        }

        // Valid signature; the handler must fail before recovery.
        let signature = sign_digest(
            &scenario.guardians[guardian_index as usize],
            &scenario.signing_digest,
        );
        let ix = Instruction::new_with_bytes(
            program_id(),
            &submit_ix_data(
                scenario.guardian_set_index,
                guardian_index,
                &signature,
                &scenario.body,
            ),
            scenario.account_metas(),
        );
        let result = mollusk.process_instruction(&ix, &accounts);
        match result.program_result {
            ProgramResult::Failure(err) => {
                let code = u64::from(err);
                assert_eq!(
                    code, expected_code,
                    "[{label}] expected code {expected_code}, got {code}"
                );
            }
            other => panic!("[{label}] expected Failure, got {other:?}"),
        }
    }
}

/// Pending PDA at GSI=5; observation under GSI=4: `StaleGuardianSet`.
#[test]
fn submit_with_stale_old_set_observation_fails() {
    let mollusk = mollusk();
    let new_scenario = Scenario::new(19, 5, 0x47);
    let accounts_after_first = new_scenario.submit_n(&mollusk, 1);

    let old_guardians = make_guardians(19, 0x48); // distinct keys for GSI=4
    let stale_signature = sign_digest(&old_guardians[1], &new_scenario.signing_digest);

    let gsi_4_be = 4u32.to_be_bytes();
    let (guardian_set_4_pubkey, _) = Pubkey::find_program_address(
        &[b"GuardianSet", &gsi_4_be],
        &Pubkey::new_from_array(CORE_BRIDGE_PROGRAM_ID),
    );
    let old_gs_account = guardian_set_account(
        4,
        &old_guardians
            .iter()
            .map(|g| g.eth_address)
            .collect::<Vec<_>>(),
        0,
        0,
    );
    let mut accounts = accounts_after_first.clone();
    accounts.push((guardian_set_4_pubkey, old_gs_account));

    let mut metas = new_scenario.account_metas().clone();
    metas[2] = AccountMeta::new_readonly(guardian_set_4_pubkey, false);

    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(
            4, // stale index
            1,
            &stale_signature,
            &new_scenario.body,
        ),
        metas,
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

/// Newer GSI wipes the pending PDA and recreates it with one bit set.
#[test]
fn submit_with_new_set_observation_wipes_old_pending() {
    let mollusk = mollusk();
    let old_scenario = Scenario::new(19, 4, 0x4A);
    let accounts_after_first = old_scenario.submit_n(&mollusk, 1);

    // Same routing tuple so the pending PDA address collides.
    let new_guardians = make_guardians(19, 0x4B);
    let new_gs_account = guardian_set_account(
        5,
        &new_guardians
            .iter()
            .map(|g| g.eth_address)
            .collect::<Vec<_>>(),
        0,
        0,
    );
    let gsi_5_be = 5u32.to_be_bytes();
    let (guardian_set_5_pubkey, _) = Pubkey::find_program_address(
        &[b"GuardianSet", &gsi_5_be],
        &Pubkey::new_from_array(CORE_BRIDGE_PROGRAM_ID),
    );
    let mut accounts = accounts_after_first.clone();
    accounts.push((guardian_set_5_pubkey, new_gs_account));

    let signature = sign_digest(&new_guardians[0], &old_scenario.signing_digest);
    let mut metas = old_scenario.account_metas().clone();
    metas[2] = AccountMeta::new_readonly(guardian_set_5_pubkey, false);
    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(5, 0, &signature, &old_scenario.body),
        metas,
    );
    let r = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(r.program_result, ProgramResult::Success),
        "rotation submit must succeed, got {:?}",
        r.program_result
    );

    let pending = find_account(&r.resulting_accounts, &old_scenario.pending_pda);
    assert_eq!(
        pending.owner,
        program_id(),
        "pending PDA still owned by program after rotation"
    );
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

/// A second digest under the same guardian set uses a sibling pending PDA.
#[test]
fn submit_with_different_digest_under_same_set_creates_sibling_bucket() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x4C);

    let accounts_after_first = scenario.submit_n(&mollusk, 1);

    // Mutate consistency_level (offset 50); routing tuple unchanged.
    let mut alternate_body = scenario.body.clone();
    alternate_body[50] = 0xAA;
    let alternate_digest = double_keccak256_host(&alternate_body);
    assert_ne!(
        alternate_digest, scenario.digest,
        "one-byte body change must yield a different digest"
    );
    let signature = sign_digest(&scenario.guardians[1], &signing_digest_for(&alternate_body));

    let (d2_pending_pda, _) = derive_pending_pda(
        scenario.chain,
        &scenario.emitter,
        scenario.sequence,
        &alternate_digest,
    );
    assert_ne!(
        d2_pending_pda, scenario.pending_pda,
        "per-digest PDA seeds must yield distinct addresses for distinct digests"
    );

    let mut accounts = accounts_after_first.clone();
    accounts.push((d2_pending_pda, uninitialised_pda_account()));

    let mut metas = scenario.account_metas();
    metas[1] = AccountMeta::new(d2_pending_pda, false); // slot 1 = D2 sibling
    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(scenario.guardian_set_index, 1, &signature, &alternate_body),
        metas,
    );
    let r = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(r.program_result, ProgramResult::Success),
        "different-digest submit under same GSI must succeed into a sibling \
         bucket, got {:?}",
        r.program_result
    );

    let d1 = find_account(&r.resulting_accounts, &scenario.pending_pda);
    let d1_layout: &PendingObservationsLayout = bytemuck::from_bytes(&d1.data);
    assert_eq!(d1_layout.digest, scenario.digest);
    assert_eq!(
        d1_layout.signatures, 0b1,
        "D1 bucket still at one signature"
    );

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

/// Two digests for one `(chain, emitter, sequence)` race in sibling PDAs.
/// NoReplay is shared, so the loser is reclaimable through `close_pending`.
#[test]
fn fork_recovery_different_digest_same_seq_under_same_set_both_accumulate() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 6, 0x5A);

    let accounts_after_d1 = scenario.submit_n(&mollusk, 7);
    let d1_after = find_account(&accounts_after_d1, &scenario.pending_pda);
    let d1_layout: &PendingObservationsLayout = bytemuck::from_bytes(&d1_after.data);
    assert_eq!(
        d1_layout.signatures.count_ones(),
        7,
        "D1 bucket accumulated 7 sigs before reorg"
    );

    // Mutate consistency_level (byte 50); routing tuple unchanged.
    let mut alternate_body = scenario.body.clone();
    alternate_body[50] = 0xA5;
    let alternate_digest = double_keccak256_host(&alternate_body);
    assert_ne!(alternate_digest, scenario.digest);

    let (d2_pending_pda, _) = derive_pending_pda(
        scenario.chain,
        &scenario.emitter,
        scenario.sequence,
        &alternate_digest,
    );
    assert_ne!(d2_pending_pda, scenario.pending_pda);

    let mut accounts = accounts_after_d1.clone();
    accounts.push((d2_pending_pda, uninitialised_pda_account()));

    for i in 0..13u8 {
        let signature = sign_digest(
            &scenario.guardians[i as usize],
            &signing_digest_for(&alternate_body),
        );
        let mut metas = scenario.account_metas();
        metas[1] = AccountMeta::new(d2_pending_pda, false);
        let ix = Instruction::new_with_bytes(
            program_id(),
            &submit_ix_data(scenario.guardian_set_index, i, &signature, &alternate_body),
            metas,
        );
        let r = mollusk.process_instruction(&ix, &accounts);
        assert!(
            matches!(r.program_result, ProgramResult::Success),
            "D2 submit #{i} expected success, got {:?}",
            r.program_result
        );
        accounts = r.resulting_accounts.clone();
    }

    let bucket = find_account(&accounts, &scenario.noreplay_bucket_pubkey);
    assert_eq!(
        bucket.owner,
        Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        "NoReplay flipped on D2 quorum reach (shared bucket across siblings)"
    );
    assert_eq!(bucket.data.len(), NoReplayBitmapAccount::LEN);

    // Mollusk hides program logs; the surfpool e2e suite checks the commit log.

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

/// Marked NoReplay bucket: abort before signature or PDA work.
#[test]
fn submit_rejected_when_noreplay_already_marked() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x4D);

    let mut accounts = scenario.initial_accounts();
    if let Some(entry) = accounts
        .iter_mut()
        .find(|(k, _)| *k == scenario.noreplay_bucket_pubkey)
    {
        entry.1 = noreplay_bucket_marked(scenario.sequence);
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

/// Quorum derives from the live set size. With 6 guardians the threshold is
/// `(6*2)/3 + 1 = 5`.
#[test]
fn quorum_threshold_tracks_live_guardian_set_size() {
    const N: usize = 6;
    let quorum = PendingObservationsLayout::quorum_for(N as u32) as u8; // 5
    assert_eq!(quorum, 5, "(6*2)/3 + 1 == 5");
    assert_ne!(
        quorum,
        PendingObservationsLayout::quorum_for(19) as u8,
        "must differ from the 19-guardian quorum to prove it is not pinned"
    );

    let mollusk = mollusk();
    // Attest body isolates the quorum gate; the NoReplay flip marks quorum.
    let scenario = Scenario::new(N, 4, 0x9A);

    let accounts = scenario.submit_n(&mollusk, quorum - 1);
    let bucket = find_account(&accounts, &scenario.noreplay_bucket_pubkey);
    assert_eq!(
        bucket.owner,
        system_program_id(),
        "{} of {} observations must not reach quorum",
        quorum - 1,
        quorum
    );

    let result = scenario.submit_once(&mollusk, accounts, quorum - 1);
    assert!(
        matches!(result.program_result, ProgramResult::Success),
        "quorum-completing tx must succeed, got {:?}",
        result.program_result
    );
    let bucket = find_account(&result.resulting_accounts, &scenario.noreplay_bucket_pubkey);
    assert_eq!(
        bucket.owner,
        Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        "quorum must flip the NoReplay bucket to the noreplay program"
    );
}

/// Drive 13 observations against a transfer scenario.
fn drive_transfer_to_quorum(
    mollusk: &Mollusk,
    scenario: &Scenario,
) -> mollusk_svm::result::InstructionResult {
    let mut accounts = scenario.initial_accounts();
    for i in 0..PendingObservationsLayout::quorum_for(19) as u8 {
        let result = scenario.submit_once(mollusk, accounts.clone(), i);
        assert!(
            matches!(result.program_result, ProgramResult::Success),
            "submit #{i} expected success, got {:?}",
            result.program_result
        );
        accounts = result.resulting_accounts.clone();
        if i + 1 == PendingObservationsLayout::quorum_for(19) as u8 {
            return result;
        }
    }
    unreachable!("loop above always returns on the final iteration")
}

/// Ethereum-native USDC (chain 2) to Solana (chain 1): both balance PDAs credit and lazy-init.
#[test]
fn quorum_with_transfer_credits_native_chain_and_mints_wrapped_chain() {
    let mollusk = mollusk();
    let token_address = [0x77u8; 32];
    let scenario = Scenario::with_transfer_body(
        19,
        4,
        0x60,
        500_000u128,
        2, // token_chain = source-native (Ethereum)
        token_address,
        1, // recipient_chain = Solana (wrapped destination)
    );

    let initial = scenario.initial_accounts();
    for pda in [scenario.source_account_pubkey, scenario.dest_account_pubkey] {
        let pre = find_account(&initial, &pda);
        assert_eq!(
            pre.owner,
            system_program_id(),
            "Account PDA starts system-owned"
        );
        assert!(pre.data.is_empty(), "Account PDA starts with zero data");
    }

    let result = drive_transfer_to_quorum(&mollusk, &scenario);
    assert!(
        matches!(result.program_result, ProgramResult::Success),
        "quorum tx must succeed, got {:?}",
        result.program_result
    );

    // Source (chain == token_chain): native lock credits.
    let src = find_account(&result.resulting_accounts, &scenario.source_account_pubkey);
    assert_eq!(
        src.owner,
        program_id(),
        "source Account PDA owned by program"
    );
    assert_eq!(src.data.len(), BalanceAccountLayout::LEN);
    let src_layout: &BalanceAccountLayout = bytemuck::from_bytes(&src.data);
    assert_eq!(src_layout.chain, 2);
    assert_eq!(src_layout.token_chain, 2);
    assert_eq!(src_layout.token_address, token_address);
    assert_eq!(src_layout.balance, Uint256::from_u128(500_000));

    // Dest (chain != token_chain): wrapped mint credits.
    let dst = find_account(&result.resulting_accounts, &scenario.dest_account_pubkey);
    assert_eq!(dst.owner, program_id(), "dest Account PDA owned by program");
    let dst_layout: &BalanceAccountLayout = bytemuck::from_bytes(&dst.data);
    assert_eq!(dst_layout.chain, 1);
    assert_eq!(dst_layout.token_chain, 2);
    assert_eq!(dst_layout.balance, Uint256::from_u128(500_000));
}

/// Solana wUSDC back to Ethereum: the wrapped-source debit underflows from zero.
#[test]
fn quorum_with_transfer_underflows_when_wrapped_chain_has_insufficient_balance() {
    let mollusk = mollusk();
    let token_address = [0x88u8; 32];
    let mut scenario = Scenario::with_transfer_body(
        19,
        4,
        0x61,
        1_000u128,
        2, // token_chain = Ethereum (token-native)
        token_address,
        2, // recipient_chain = Ethereum
    );
    scenario.chain = 1;
    scenario.body = build_transfer_body(
        scenario.chain,
        &scenario.emitter,
        scenario.sequence,
        1_000,
        2,
        &token_address,
        2,
    );
    scenario.digest = double_keccak256_host(&scenario.body);
    scenario.signing_digest = signing_digest_for(&scenario.body);
    let (pending_pda, _) = derive_pending_pda(
        scenario.chain,
        &scenario.emitter,
        scenario.sequence,
        &scenario.digest,
    );
    scenario.pending_pda = pending_pda;
    let (src, _) = derive_balance_account_pda(1, 2, &token_address);
    let (dst, _) = derive_balance_account_pda(2, 2, &token_address);
    let (registration_pda, _) = derive_chain_registration_pda(scenario.chain);
    scenario.chain_registration_pubkey = registration_pda;
    scenario.noreplay_bucket_pubkey = derive_canonical_noreplay_bucket(
        &scenario.noreplay_authority_pubkey,
        scenario.chain,
        &scenario.emitter,
        scenario.sequence,
    );
    scenario.source_account_pubkey = src;
    scenario.dest_account_pubkey = dst;

    let mut accounts = scenario.initial_accounts();
    for i in 0..12u8 {
        let r = scenario.submit_once(&mollusk, accounts.clone(), i);
        assert!(matches!(r.program_result, ProgramResult::Success));
        accounts = r.resulting_accounts;
    }
    let r = scenario.submit_once(&mollusk, accounts, 12);
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::BalanceUnderflow as u32,
                "expected BalanceUnderflow, got {code:?}"
            );
        }
        other => panic!("expected Failure(BalanceUnderflow), got {other:?}"),
    }
    // Rolled back: bucket untouched.
    let bucket = find_account(&r.resulting_accounts, &scenario.noreplay_bucket_pubkey);
    assert_eq!(
        bucket.owner,
        system_program_id(),
        "bucket still system-owned"
    );
    assert!(
        bucket.data.is_empty(),
        "NoReplay must not flip on failed quorum"
    );
}

/// Dust on the destination PDA (below rent minimum) must not block lazy init;
/// `CreateAccountAllowPrefund` tops up the shortfall.
#[test]
fn quorum_with_dusted_destination_account_succeeds() {
    let mollusk = mollusk();
    let token_address = [0x4Du8; 32];
    let scenario = Scenario::with_transfer_body(19, 4, 0x63, 7_777u128, 2, token_address, 1);

    // System-owned, non-zero balance, zero data.
    const DUST: u64 = 1;
    let mut accounts = scenario.initial_accounts();
    accounts
        .iter_mut()
        .find(|(k, _)| *k == scenario.dest_account_pubkey)
        .expect("dest PDA in account list")
        .1 = system_owned_account(DUST);

    let mut result = None;
    for i in 0..PendingObservationsLayout::quorum_for(19) as u8 {
        let r = scenario.submit_once(&mollusk, accounts.clone(), i);
        assert!(
            matches!(r.program_result, ProgramResult::Success),
            "submit #{i} expected success, got {:?}",
            r.program_result
        );
        accounts = r.resulting_accounts.clone();
        result = Some(r);
    }
    let result = result.unwrap();

    let dst_post = find_account(&result.resulting_accounts, &scenario.dest_account_pubkey);
    assert_eq!(
        dst_post.owner,
        program_id(),
        "dusted dest PDA still lazy-inits under the program"
    );
    assert_eq!(dst_post.data.len(), BalanceAccountLayout::LEN);
    assert!(
        dst_post.lamports > DUST,
        "rent shortfall must be topped up over the injected dust"
    );
    let layout: &BalanceAccountLayout = bytemuck::from_bytes(&dst_post.data);
    assert_eq!(layout.balance, Uint256::from_u128(7_777));
}

/// CU guard: the quorum-commit branch stays below `MAX_QUORUM_BRANCH_CU`.
#[test]
fn quorum_branch_cu_stays_below_ceiling() {
    let mollusk = mollusk();
    let token_address = [0xCFu8; 32];
    let scenario = Scenario::with_transfer_body(
        19,
        4,
        0xCF,
        12_345u128,
        2,
        token_address,
        1, // recipient_chain = Solana so both Account PDAs lazy-init
    );

    let mut accounts = scenario.initial_accounts();
    for i in 0..(PendingObservationsLayout::quorum_for(19) as u8 - 1) {
        let r = scenario.submit_once(&mollusk, accounts.clone(), i);
        assert!(matches!(r.program_result, ProgramResult::Success));
        accounts = r.resulting_accounts;
    }

    let result = scenario.submit_once(
        &mollusk,
        accounts,
        PendingObservationsLayout::quorum_for(19) as u8 - 1,
    );
    assert!(
        matches!(result.program_result, ProgramResult::Success),
        "quorum tx must succeed for the CU measurement to be meaningful, got {:?}",
        result.program_result
    );

    assert!(
        result.compute_units_consumed <= MAX_QUORUM_BRANCH_CU,
        "quorum-branch CU ({}) exceeded ceiling ({}); investigate before raising the constant",
        result.compute_units_consumed,
        MAX_QUORUM_BRANCH_CU
    );
}

/// Attest quorum commits but touches neither balance PDA.
#[test]
fn quorum_with_attest_payload_skips_balance_work_but_finishes_commit() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x63);
    let result = drive_transfer_to_quorum(&mollusk, &scenario);
    assert!(
        matches!(result.program_result, ProgramResult::Success),
        "attest quorum tx must succeed, got {:?}",
        result.program_result
    );

    assert_eq!(
        scenario.source_account_pubkey,
        scenario.noreplay_authority_pubkey
    );
    let sentinel = find_account(
        &result.resulting_accounts,
        &scenario.noreplay_authority_pubkey,
    );
    assert_eq!(
        sentinel.owner,
        system_program_id(),
        "sentinel slot stays system-owned across attest commit"
    );
    assert!(
        sentinel.data.is_empty(),
        "sentinel slot data untouched across attest commit"
    );
}

/// Unknown payload action: `UnknownTokenBridgePayload` at quorum; NoReplay mark rolls back.
#[test]
fn quorum_with_unknown_payload_rejects_and_preserves_replay_slot() {
    let mollusk = mollusk();
    let mut scenario = Scenario::new(19, 4, 0x65);
    scenario.body[51] = 0x05; // unknown Token Bridge action byte
    scenario.digest = double_keccak256_host(&scenario.body);
    scenario.signing_digest = signing_digest_for(&scenario.body);
    let (pending_pda, _) = derive_pending_pda(
        scenario.chain,
        &scenario.emitter,
        scenario.sequence,
        &scenario.digest,
    );
    scenario.pending_pda = pending_pda;

    // Payload parses only at quorum.
    let accounts = scenario.submit_n(&mollusk, 12);

    let result = scenario.submit_once(&mollusk, accounts, 12);
    match result.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::UnknownTokenBridgePayload as u32,
                "expected UnknownTokenBridgePayload, got code {code}"
            );
        }
        other => panic!("expected Failure(UnknownTokenBridgePayload), got {other:?}"),
    }

    let bucket = find_account(&result.resulting_accounts, &scenario.noreplay_bucket_pubkey);
    assert_eq!(
        bucket.owner,
        system_program_id(),
        "noreplay bucket must stay system-owned after rejection"
    );
    assert!(
        bucket.data.is_empty(),
        "noreplay bucket data untouched after rejection"
    );

    let pending = find_account(&result.resulting_accounts, &scenario.pending_pda);
    assert_eq!(pending.owner, program_id(), "pending PDA still live");
    let layout: &PendingObservationsLayout = bytemuck::from_bytes(&pending.data);
    assert_eq!(
        layout.signatures.count_ones(),
        12,
        "12 signatures still recorded in the surviving bucket"
    );
}

/// A body changed after signing yields another signing digest: `InvalidSignature`.
#[test]
fn tampered_body_fails_signature_check() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x64);

    let signature = sign_digest(&scenario.guardians[0], &scenario.signing_digest);
    let mut tampered_body = scenario.body.clone();
    tampered_body[0] ^= 0xAA; // mutate the timestamp byte
    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(scenario.guardian_set_index, 0, &signature, &tampered_body),
        scenario.account_metas(),
    );
    let r = mollusk.process_instruction(&ix, &scenario.initial_accounts());
    match r.program_result {
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
    let pending = find_account(&r.resulting_accounts, &scenario.pending_pda);
    assert_eq!(pending.owner, system_program_id());
    assert!(pending.data.is_empty());
}

/// Wrong-seed source balance PDA: `InvalidAccountPda` at quorum.
#[test]
fn quorum_with_invalid_source_account_pda_rejects() {
    let mollusk = mollusk();
    let token_address = [0x99u8; 32];
    let mut scenario = Scenario::with_transfer_body(19, 4, 0x65, 100u128, 2, token_address, 1);
    // Wrong token chain.
    let (spoofed, _) = derive_balance_account_pda(2, 99, &token_address);
    scenario.source_account_pubkey = spoofed;

    let mut accounts = scenario.initial_accounts();
    for i in 0..12u8 {
        let r = scenario.submit_once(&mollusk, accounts.clone(), i);
        assert!(matches!(r.program_result, ProgramResult::Success));
        accounts = r.resulting_accounts;
    }
    let r = scenario.submit_once(&mollusk, accounts, 12);
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidAccountPda as u32,
                "expected InvalidAccountPda, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidAccountPda), got {other:?}"),
    }
}

/// `GuardianSet` not owned by the Core Bridge: `InvalidPda` before signature work.
#[test]
fn spoofed_guardian_set_not_owned_by_core_bridge_rejects() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x66);

    let spoofed_guardian_set = {
        let mut data = Vec::with_capacity(8 + 13 * 20 + 8);
        data.extend_from_slice(&scenario.guardian_set_index.to_le_bytes());
        data.extend_from_slice(&13u32.to_le_bytes()); // keys_len = 13
        for i in 0..13 {
            data.extend_from_slice(&[i as u8; 20]);
        }
        data.extend_from_slice(&0u32.to_le_bytes()); // creation_time
        data.extend_from_slice(&u32::MAX.to_le_bytes()); // expiration_time
        Account {
            lamports: 1_000_000,
            data,
            owner: system_program_id(), // NOT CORE_BRIDGE_PROGRAM_ID
            executable: false,
            rent_epoch: 0,
        }
    };

    let signature = sign_digest(&scenario.guardians[0], &scenario.signing_digest);
    let mut accounts = scenario.initial_accounts();
    accounts[2] = (scenario.guardian_set_pubkey, spoofed_guardian_set);

    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(scenario.guardian_set_index, 0, &signature, &scenario.body),
        scenario.account_metas(),
    );
    let r = mollusk.process_instruction(&ix, &accounts);
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidPda as u32,
                "spoofed guardian set should fail with InvalidPda, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidPda), got {other:?}"),
    }
}

/// Non-canonical pending PDA on the Continue path: `InvalidPda`.
#[test]
fn non_canonical_pending_pda_address_rejects_on_continue() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x67);

    let mut accounts = scenario.initial_accounts();
    let r1 = scenario.submit_once(&mollusk, accounts.clone(), 0);
    assert!(matches!(r1.program_result, ProgramResult::Success));
    accounts = r1.resulting_accounts;

    // Replace the pending PDA with a non-canonical address.
    let spoofed_pending_pubkey = Pubkey::new_unique();
    let pending_layout = *bytemuck::from_bytes::<PendingObservationsLayout>(&accounts[1].1.data);
    let spoofed_pending_account = Account {
        lamports: 1_000_000,
        data: bytemuck::bytes_of(&pending_layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    };
    accounts[1] = (spoofed_pending_pubkey, spoofed_pending_account);

    let signature = sign_digest(&scenario.guardians[1], &scenario.signing_digest);
    let mut metas = scenario.account_metas();
    metas[1] = AccountMeta::new(spoofed_pending_pubkey, false);
    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(scenario.guardian_set_index, 1, &signature, &scenario.body),
        metas,
    );
    let r2 = mollusk.process_instruction(&ix, &accounts);
    match r2.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidPda as u32,
                "non-canonical pending PDA should fail with InvalidPda, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidPda), got {other:?}"),
    }
}

/// A signature over the bare dedup digest `double_keccak256(body)` fails with
/// `InvalidSignature`; the program checks `keccak256(prefix ‖ tx_hash ‖ body)`.
#[test]
fn submit_with_legacy_bare_digest_signature_is_rejected() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x71);

    assert_ne!(
        scenario.digest, scenario.signing_digest,
        "dedup digest and signing digest must differ for the firewall to bite"
    );

    // Sign the bare dedup digest.
    let legacy_signature = sign_digest(&scenario.guardians[0], &scenario.digest);

    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(
            scenario.guardian_set_index,
            0,
            &legacy_signature,
            &scenario.body,
        ),
        scenario.account_metas(),
    );
    let r = mollusk.process_instruction(&ix, &scenario.initial_accounts());
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidSignature as u32,
                "a signature over the legacy bare double_keccak256(body) digest must \
                 be rejected as InvalidSignature, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidSignature), got {other:?}"),
    }

    let pending = find_account(&r.resulting_accounts, &scenario.pending_pda);
    assert_eq!(pending.owner, system_program_id());
    assert!(pending.data.is_empty());
}

/// The signing digest and the dedup digest differ for the same body.
#[test]
fn signing_digest_differs_from_dedup_digest() {
    let body = build_attest_body(2, &[0x77u8; 32], 0x42);
    let signing = observation_signing_digest_host(SUBMIT_OBSERVATION_PREFIX, &TX_HASH, &body);
    let dedup = double_keccak256_host(&body);
    assert_ne!(
        signing, dedup,
        "observation signing digest must be a distinct domain from the dedup digest"
    );
}
