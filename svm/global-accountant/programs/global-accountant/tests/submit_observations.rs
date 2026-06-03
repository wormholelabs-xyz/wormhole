//! Integration tests for `submit_observations` + `close_pending`.
//!
//! Driven against a Mollusk instance with the real `solana_noreplay.so`
//! loaded at the canonical program ID (see `common::mollusk_fixtures`). The
//! `test-only-open-digest` feature exposes the `OpenDigest` arm for the
//! direct-drive close-side tests.
//!
//! Test surface covered:
//!
//! - Single pending PDA per `(chain, emitter, sequence, digest)`.
//! - Bitmap-only signature storage.
//! - Wipe-on-rotation decision table (stale / rotation / forgery).
//! - Inline `secp256k1_recover` verification (good + bad signatures).
//! - Quorum commit flow (NoReplay flip + DigestAccount open + pending close).
//! - `close_pending` triggers (expired set + NoReplay-marked).

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        BalanceAccountLayout, ChainRegistrationLayout, DigestAccountLayout, GlobalAccountantError,
        Instruction as IxDiscriminator, PendingObservationsLayout, Uint256, ACCOUNT_SEED_PREFIX,
        CHAIN_REGISTRATION_SEED_PREFIX, CORE_BRIDGE_PROGRAM_ID, DIGEST_SEED_PREFIX,
        MAX_QUORUM_BRANCH_CU, NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITMAP_BYTES,
        NOREPLAY_BITMAP_OFFSET, NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID, PENDING_SEED_PREFIX,
    },
    libsecp256k1::{sign, Message, PublicKey, SecretKey},
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk},
    solana_account::Account,
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

mod common;
use common::mollusk_fixtures::{keyed_account_for_noreplay_program, mollusk_with_fixtures};

const PROGRAM_NAME: &str = "global_accountant";

fn program_id() -> Pubkey {
    // Fixed program id so PDA derivation in the test matches the program's
    // on-chain view.
    Pubkey::new_from_array([7u8; 32])
}

fn mollusk() -> Mollusk {
    mollusk_with_fixtures(&program_id(), PROGRAM_NAME)
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
        &[
            PENDING_SEED_PREFIX,
            &chain_be,
            emitter,
            &sequence_be,
            digest,
        ],
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

fn derive_account_pda(chain: u16, token_chain: u16, token_address: &[u8; 32]) -> (Pubkey, u8) {
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

/// Host-side derivation of the canonical NoReplay bitmap PDA for
/// `(authority, chain, emitter, sequence)`. Mirrors the on-chain
/// `instructions::noreplay::derive_bucket_pda` and the upstream
/// `solana_noreplay::pda::BitmapPdaSeeds` scheme. Both mock and production
/// noreplay branches now enforce the canonical address (since the
/// mock-noreplay belt-and-braces hardening), so every test fixture that
/// supplies a noreplay bucket account must place it here.
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

/// Build a program-owned chain-registration PDA account fixture containing
/// the given canonical emitter address. Used by tests to pre-populate the
/// registry without going through the `register_chain` governance path.
fn chain_registration_account(chain: u16, emitter_address: &[u8; 32]) -> Account {
    let mut layout: ChainRegistrationLayout = bytemuck::Zeroable::zeroed();
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

/// Host-side `keccak256(keccak256(body))` — the Wormhole VAA digest
/// convention. Mirrors the on-chain `double_keccak256` in
/// `submit_observations.rs`.
fn double_keccak256_host(body: &[u8]) -> [u8; 32] {
    let inner = solana_keccak_hasher::hashv(&[body]).to_bytes();
    solana_keccak_hasher::hashv(&[&inner]).to_bytes()
}

/// Build a VAA body whose `keccak256(keccak256(...))` equals the supplied
/// digest. Since we don't have a digest preimage from the original test
/// fixtures (the digest was made-up), we build the body and then compute the
/// digest from it — the scenario fixture's `digest` field becomes a function
/// of the body, not the other way around.
fn build_attest_body(emitter_chain: u16, emitter_address: &[u8; 32], sequence: u64) -> Vec<u8> {
    // 51-byte header + 1-byte action (0x02). Attest payloads carry more on
    // the wire but the parser only reads the action byte, so 52 bytes is
    // sufficient for our tests.
    let mut body = vec![0u8; 52];
    body[8..10].copy_from_slice(&emitter_chain.to_be_bytes());
    body[10..42].copy_from_slice(emitter_address);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body[51] = 0x02;
    body
}

/// Build a VAA body carrying a Token Bridge transfer (action 0x01) with the
/// supplied fields.
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
    // Token Bridge transfer payload starts at offset 51.
    body[51] = 0x01;
    // amount: 32-byte BE, low 16 bytes hold the u128.
    body[52 + 16..52 + 32].copy_from_slice(&amount.to_be_bytes());
    body[84..116].copy_from_slice(token_address);
    body[116..118].copy_from_slice(&token_chain.to_be_bytes());
    body[118] = 0xAB; // recipient: opaque to the accountant
    body[149] = 0xCD;
    body[150..152].copy_from_slice(&recipient_chain.to_be_bytes());
    // fee at 152..184 stays zero.
    body
}

fn submit_ix_data(
    digest: &[u8; 32],
    guardian_set_index: u32,
    guardian_index: u8,
    signature: &[u8; 65],
    body: &[u8],
) -> Vec<u8> {
    // Wire shape: 1-byte discriminator + 102-byte fixed prefix + 2-byte body
    // length (LE) + body bytes. Mirrors `submit_observations.rs::SUBMIT_FIXED_LEN`.
    // No bump bytes travel in the wire: the program derives both the pending
    // and digest PDA canonical bumps on-chain via `find_program_address`. The
    // routing tuple (chain, emitter, sequence) is sourced exclusively from the
    // body header (offsets [8..50]); no caller-controlled prefix carries them.
    let mut data = Vec::with_capacity(1 + 102 + 2 + body.len());
    data.push(IxDiscriminator::SubmitObservations as u8);
    data.extend_from_slice(digest);
    data.extend_from_slice(&guardian_set_index.to_le_bytes());
    data.push(guardian_index);
    data.extend_from_slice(signature);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
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

/// Fresh (uninitialised) NoReplay bucket: the canonical lazy-create entry
/// state — system-owned, zero data. The real `solana_noreplay` CPI allocates
/// the 129-byte bitmap, assigns ownership to itself, and flips the bit on
/// first `MarkUsed`.
fn noreplay_bucket_unmarked() -> Account {
    system_owned_account(0)
}

/// Pre-marked NoReplay bucket for replay-rejection tests: 129-byte bitmap
/// owned by `solana_noreplay` with the bit at `sequence % 1024` already set.
/// Caller passes the sequence value so the bit lookup matches the bucket
/// the production `is_marked` pre-check will perform.
fn noreplay_bucket_marked(sequence: u64) -> Account {
    let mut data = vec![0u8; NOREPLAY_BITMAP_OFFSET + NOREPLAY_BITMAP_BYTES];
    let bit_index = (sequence % NOREPLAY_BITS_PER_BUCKET) as usize;
    data[NOREPLAY_BITMAP_OFFSET + bit_index / 8] |= 1u8 << (bit_index % 8);
    Account {
        lamports: 1_500_000_000,
        data,
        owner: Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
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
        owner: Pubkey::new_from_array(CORE_BRIDGE_PROGRAM_ID),
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
        out.push(Guardian {
            secret,
            eth_address,
        });
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
    /// VAA body bytes — the source of truth. `digest` is derived from this
    /// via `double_keccak256_host`. The default scenario uses an Attest
    /// payload (action 0x02) so the existing pre-Phase-2.4 tests don't
    /// accidentally trigger balance work; transfer-payload tests build their
    /// own scenario with `Self::with_transfer_body`.
    body: Vec<u8>,
    digest: [u8; 32],
    guardian_set_index: u32,
    guardians: Vec<Guardian>,
    submitter: Pubkey,
    pending_pda: Pubkey,
    digest_pda: Pubkey,
    guardian_set_pubkey: Pubkey,
    noreplay_bucket_pubkey: Pubkey,
    noreplay_program_pubkey: Pubkey,
    /// Stand-in for the global-accountant-owned `noreplay-authority` PDA. In
    /// mollusk runs we never reach the real CPI (gated behind `mock-noreplay`),
    /// so the address only needs to be a stable distinct pubkey the runtime
    /// can include in the account list. The surfpool e2e test
    /// (`surfpool_e2e_submit_observations_real_noreplay.rs`) exercises the
    /// real derivation and the CPI together.
    noreplay_authority_pubkey: Pubkey,
    /// Source-chain Account PDA. Required on every submission (slot 8 of the
    /// account list). For Attest payloads the program never touches it; the
    /// default scenario uses the noreplay-authority pubkey as a sentinel so
    /// the slot is satisfied without standing up a real Account PDA.
    source_account_pubkey: Pubkey,
    /// Destination-chain Account PDA (slot 9). Same semantics as `source`.
    dest_account_pubkey: Pubkey,
    /// Chain-registration PDA at slot 11. Default scenario registers
    /// `chain -> emitter` (mirroring what the on-chain registry would hold
    /// after a `register_chain` governance VAA lands) so existing tests do
    /// not need to pre-populate registrations. Negative tests override the
    /// account fixture to drive `MissingChainRegistration` or
    /// `UnregisteredEmitter`.
    chain_registration_pubkey: Pubkey,
}

impl Scenario {
    fn new(guardian_count: usize, gsi: u32, seed: u8) -> Self {
        let chain: u16 = 2;
        let mut emitter = [0u8; 32];
        emitter[31] = 0x77;
        let sequence: u64 = 0x0000_0000_0000_0042;

        // Default scenario: an attest-payload body. The digest is derived
        // from the body rather than supplied as an arbitrary 32 bytes —
        // matches the wire contract where the program verifies
        // `keccak256(keccak256(body)) == digest` before any state work.
        let body = build_attest_body(chain, &emitter, sequence);
        let digest = double_keccak256_host(&body);

        let guardians = make_guardians(guardian_count, seed);
        let submitter = Pubkey::new_from_array([0x11u8; 32]);
        // Only the PDA addresses are needed for the account metas; the program
        // derives the canonical bumps on-chain, so the bump component is dropped.
        let (pending_pda, _) = derive_pending_pda(chain, &emitter, sequence, &digest);
        let (digest_pda, _) = derive_digest_pda(chain, &emitter, sequence);
        // Use the canonical program-derived noreplay authority so the bucket
        // address agrees with close_pending's internal re-derivation (which
        // does not consult the account list — see close_pending.rs).
        let (noreplay_authority_pubkey, _) =
            Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
        let (chain_registration_pubkey, _) = derive_chain_registration_pda(chain);
        let noreplay_bucket_pubkey =
            derive_canonical_noreplay_bucket(&noreplay_authority_pubkey, chain, &emitter, sequence);

        Self {
            chain,
            emitter,
            sequence,
            body,
            digest,
            guardian_set_index: gsi,
            guardians,
            submitter,
            pending_pda,
            digest_pda,
            guardian_set_pubkey: Pubkey::new_from_array([0xC1u8; 32]),
            noreplay_bucket_pubkey,
            noreplay_program_pubkey: Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
            noreplay_authority_pubkey,
            // Attest payload ⇒ slots 8/9 never touched. Use a sentinel pubkey
            // so the runtime account-meta is satisfied without needing the
            // canonical seeds. Re-using noreplay-authority is the cheapest
            // option and matches the documented "sentinel" pattern.
            source_account_pubkey: noreplay_authority_pubkey,
            dest_account_pubkey: noreplay_authority_pubkey,
            chain_registration_pubkey,
        }
    }

    /// Variant that swaps the default Attest body for a Token Bridge
    /// Transfer body and updates `digest`, `pending_pda`, and `digest_pda`
    /// accordingly. `source_account_pubkey` / `dest_account_pubkey` are also
    /// re-derived from the canonical seeds so the Transfer commit branch's
    /// `verify_account_pda` passes.
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
        let (pending_pda, _) =
            derive_pending_pda(base.chain, &base.emitter, base.sequence, &base.digest);
        base.pending_pda = pending_pda;
        // Account PDAs: source uses VAA emitter chain (== base.chain) for
        // chain; dest uses recipient_chain.
        let (src, _) = derive_account_pda(base.chain, token_chain, &token_address);
        let (dst, _) = derive_account_pda(recipient_chain, token_chain, &token_address);
        base.source_account_pubkey = src;
        base.dest_account_pubkey = dst;
        base
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
                &self.digest,
                self.guardian_set_index,
                guardian_index,
                &signature,
                &self.body,
            ),
            self.account_metas(),
        );
        mollusk.process_instruction(&ix, &starting_accounts)
    }

    /// Account-meta list (12 entries) matching the wire shape documented in
    /// `submit_observations.rs::process`. Slot 10 is the rent_recipient and
    /// slot 11 is the chain-registration PDA. The default scenario uses
    /// `submitter` for rent_recipient (the bucket opener and the only
    /// observer in single-submitter tests) and the canonical chain-
    /// registration PDA address; `initial_accounts` pre-populates that PDA
    /// with `(chain, emitter) -> emitter` so happy-path tests don't have to
    /// register manually. Multi-submitter and registration-negative tests
    /// build their own meta vec inline.
    fn account_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.submitter, true),
            AccountMeta::new(self.pending_pda, false),
            AccountMeta::new_readonly(self.guardian_set_pubkey, false),
            AccountMeta::new(self.noreplay_bucket_pubkey, false),
            AccountMeta::new(self.digest_pda, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(self.noreplay_program_pubkey, false),
            AccountMeta::new_readonly(self.noreplay_authority_pubkey, false),
            AccountMeta::new(self.source_account_pubkey, false),
            AccountMeta::new(self.dest_account_pubkey, false),
            AccountMeta::new(self.submitter, false),
            AccountMeta::new_readonly(self.chain_registration_pubkey, false),
        ]
    }

    /// Build the initial 10-account list with all PDAs uninitialised. Slots
    /// 8 and 9 (source / destination Account PDA) are intentionally
    /// system-owned + empty so the program's lazy-init path kicks in when a
    /// Transfer payload reaches quorum.
    fn initial_accounts(&self) -> Vec<(Pubkey, Account)> {
        let mut accounts = vec![
            (self.submitter, system_owned_account(50_000_000_000)),
            (self.pending_pda, uninitialised_pda_account()),
            (
                self.guardian_set_pubkey,
                guardian_set_account(self.guardian_set_index, &self.guardian_keys(), 0, 0),
            ),
            (self.noreplay_bucket_pubkey, noreplay_bucket_unmarked()),
            (self.digest_pda, uninitialised_pda_account()),
            keyed_account_for_system_program(),
            keyed_account_for_noreplay_program(),
            (self.noreplay_authority_pubkey, system_owned_account(0)),
        ];
        // Slots 8 and 9. Re-use existing entries when the sentinel collapses
        // them onto the noreplay-authority pubkey (Attest scenario); otherwise
        // append fresh uninit slots.
        if self.source_account_pubkey != self.noreplay_authority_pubkey {
            accounts.push((self.source_account_pubkey, uninitialised_pda_account()));
        }
        if self.dest_account_pubkey != self.noreplay_authority_pubkey
            && self.dest_account_pubkey != self.source_account_pubkey
        {
            accounts.push((self.dest_account_pubkey, uninitialised_pda_account()));
        }
        // Slot 11: chain-registration PDA pre-populated with the scenario's
        // emitter so happy-path tests don't have to register manually.
        // Negative tests override this account in the resulting list to
        // drive `MissingChainRegistration` / `UnregisteredEmitter`.
        accounts.push((
            self.chain_registration_pubkey,
            chain_registration_account(self.chain, &self.emitter),
        ));
        accounts
    }

    /// Run `n` observations sequentially from guardian indices `0..n`. Returns
    /// the resulting accounts after the last submission.
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
    assert_eq!(
        layout.payer,
        scenario.submitter.to_bytes(),
        "submitter is the recorded payer"
    );

    // No quorum yet -> digest PDA is untouched.
    let digest = find_account(&accounts, &scenario.digest_pda);
    assert!(
        digest.owner == system_program_id() && digest.data.is_empty(),
        "digest PDA must not be opened before quorum reach"
    );

    // No quorum yet -> NoReplay bucket stays in its lazy-create entry state
    // (system-owned, zero data). The real CPI only fires on the quorum
    // branch.
    let bucket = find_account(&accounts, &scenario.noreplay_bucket_pubkey);
    assert_eq!(
        bucket.owner,
        system_program_id(),
        "bucket still system-owned"
    );
    assert!(bucket.data.is_empty(), "bucket still uninitialised");
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
    // Quorum not reached: bucket still in lazy-create entry state.
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

#[test]
fn submit_13th_observation_reaches_quorum_and_commits() {
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x44);

    let accounts = scenario.submit_n(&mollusk, 13);

    // Pending PDA must be closed (lamports drained, owner reverted to system).
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

    // NoReplay must be flipped: real solana_noreplay CPI lazily allocates
    // the 129-byte bitmap, assigns ownership to itself, and sets the bit at
    // `sequence % 1024`.
    let bucket = find_account(&accounts, &scenario.noreplay_bucket_pubkey);
    assert_eq!(
        bucket.owner,
        Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        "bucket owned by solana_noreplay after MarkUsed"
    );
    assert_eq!(
        bucket.data.len(),
        NOREPLAY_BITMAP_OFFSET + NOREPLAY_BITMAP_BYTES,
        "bucket data sized to bitmap layout"
    );
    let bit = (scenario.sequence % NOREPLAY_BITS_PER_BUCKET) as usize;
    let byte = NOREPLAY_BITMAP_OFFSET + bit / 8;
    let mask = 1u8 << (bit % 8);
    assert_eq!(
        bucket.data[byte] & mask,
        mask,
        "bitmap bit set on quorum reach"
    );
}

#[test]
fn submit_observations_quorum_with_different_submitter_refunds_recorded_payer() {
    // Multi-submitter quorum regression.
    //
    // Wormchain reference (`node/pkg/accountant/submit_obs.go:373-378`): each
    // guardian runs its own node, signs with its own key, and broadcasts using
    // its own `SenderAddress()` on Wormchain. CosmWasm has no rent concept, so
    // the contract does not care who completes quorum. The Solana port has to
    // preserve that operational model because the network does not know in
    // advance which guardian's submission will be the 13th — yet rent for the
    // pending PDA must refund to whoever opened it.
    //
    // The wire shape carries a dedicated `rent_recipient` account (slot 10)
    // that must equal the layout's recorded payer; the submitter (slot 0) may
    // be any signer. Without this separation, the `close_pending_pda` payer
    // check would block any 13th submitter that is not the original bucket
    // creator — structurally unreachable for honest multi-guardian operation.

    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x70);

    // Alice opens and accumulates the first 12 signatures (default scenario
    // uses scenario.submitter == alice for every submission).
    let accounts_after_12 = scenario.submit_n(&mollusk, 12);
    let alice = scenario.submitter;
    let alice_lamports_pre = find_account(&accounts_after_12, &alice).lamports;
    let pending_lamports = find_account(&accounts_after_12, &scenario.pending_pda).lamports;
    assert!(
        pending_lamports > 0,
        "pending PDA must be rent-funded at 12/19"
    );

    // Bob arrives with the 13th signature from a freshly-funded wallet.
    let bob = Pubkey::new_from_array([0xB0u8; 32]);
    let bob_starting_lamports = 50_000_000_000u64;
    let mut accounts = accounts_after_12.clone();
    accounts.push((bob, system_owned_account(bob_starting_lamports)));

    let signature = sign_digest(&scenario.guardians[12], &scenario.digest);
    let ix_data = submit_ix_data(
        &scenario.digest,
        scenario.guardian_set_index,
        12,
        &signature,
        &scenario.body,
    );

    // (1) Wrong rent_recipient (bob points at his own wallet): must fail with
    // PayerMismatch before any state mutation.
    let wrong_metas = vec![
        AccountMeta::new(bob, true),
        AccountMeta::new(scenario.pending_pda, false),
        AccountMeta::new_readonly(scenario.guardian_set_pubkey, false),
        AccountMeta::new(scenario.noreplay_bucket_pubkey, false),
        AccountMeta::new(scenario.digest_pda, false),
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

    // (2) Correct rent_recipient (alice): must succeed and refund alice.
    let correct_metas = vec![
        AccountMeta::new(bob, true),
        AccountMeta::new(scenario.pending_pda, false),
        AccountMeta::new_readonly(scenario.guardian_set_pubkey, false),
        AccountMeta::new(scenario.noreplay_bucket_pubkey, false),
        AccountMeta::new(scenario.digest_pda, false),
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

    // The multi-submitter invariants: rent refund routes to the recorded
    // payer (alice), not the quorum-completing submitter (bob); the
    // DigestAccount records bob as its own (newly-opened) payer. Generic
    // quorum-commit assertions (pending closed, NoReplay flipped, digest
    // PDA opened) are covered by submit_13th_observation_reaches_quorum_and_commits.
    let alice_post = find_account(&r_correct.resulting_accounts, &alice);
    assert_eq!(
        alice_post.lamports,
        alice_lamports_pre + pending_lamports,
        "alice (recorded payer) received the pending-PDA rent refund"
    );
    let digest = find_account(&r_correct.resulting_accounts, &scenario.digest_pda);
    let digest_layout: &DigestAccountLayout = bytemuck::from_bytes(&digest.data);
    assert_eq!(
        digest_layout.payer,
        bob.to_bytes(),
        "digest_pda recorded the quorum-completing submitter (bob) as its payer"
    );
}

#[test]
fn submit_observations_routes_by_body_header_not_caller_supplied_prefix() {
    // Critical security regression. Demonstrates that the bucket key
    // (chain, emitter, sequence) is sourced exclusively from the signed
    // body's header at offsets [8..50] — NEVER from caller-supplied wire
    // prefix bytes.
    //
    // The CosmWasm reference (`cosmwasm/packages/accountant/src/msg.rs` —
    // `Observation::digest`) inlines these fields inside the signed
    // Observation struct; the bucket key and the digest preimage are the
    // same bytes. Pre-fix the Solana port carried (chain, emitter, sequence)
    // in a separate caller-controlled prefix; an attacker could sign body B
    // with header (chainA, emitter_a, seq_a) and submit with prefix
    // (chainB, emitter_b, seq_b). Signatures verify (digest is a function of
    // body bytes), NoReplay slot for chainB is unmarked (different
    // namespace), quorum reaches, and apply_transfer routes the source-side
    // debit/credit at chainB's Account PDA — corrupting the balance ledger.
    //
    // Pre-fix this test fails: the program creates a pending PDA at the
    // attacker-supplied address. Post-fix the wire shape no longer carries
    // routing fields, so the attack cannot be expressed; the program reads
    // (chain, emitter, sequence) from body[8..50].
    let mollusk = mollusk();

    // Body with header (chain=2, emitter_a, seq_a). The body's bytes [8..50]
    // are the authoritative routing tuple; the program must read from there.
    let body_chain = 2u16;
    let mut body_emitter = [0u8; 32];
    body_emitter[31] = 0x77;
    let body_sequence = 0x42u64;
    let body = build_attest_body(body_chain, &body_emitter, body_sequence);
    let digest = double_keccak256_host(&body);

    let guardians = make_guardians(19, 0x80);
    let signature = sign_digest(&guardians[0], &digest);

    // Attempt the attack: feed the program an attacker-derived pending PDA in
    // slot 1, canonical for *that* namespace, not the body's. The wire no
    // longer carries (chain, emitter, sequence) — the program reads them from
    // body[8..50] — so it recomputes the canonical pending PDA address for the
    // body-derived seeds and rejects any mismatch in create_pending_pda. The
    // attacker's pending PDA address cannot collide with the body-derived
    // canonical address under our seed scheme.
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
    let (body_digest_pda, _) = derive_digest_pda(body_chain, &body_emitter, body_sequence);

    let submitter = Pubkey::new_from_array([0x11u8; 32]);
    let guardian_set_pubkey = Pubkey::new_from_array([0xC1u8; 32]);
    let (noreplay_authority_pubkey, _) =
        Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
    let noreplay_bucket_pubkey = derive_canonical_noreplay_bucket(
        &noreplay_authority_pubkey,
        body_chain,
        &body_emitter,
        body_sequence,
    );
    let noreplay_program_pubkey = Pubkey::new_from_array(NOREPLAY_PROGRAM_ID);

    let ix_data = submit_ix_data(&digest, 4, 0, &signature, &body);

    // Pre-populate the registration PDA for the body chain so the new
    // registration check passes — this test exercises the pending-PDA
    // canonical-bump rejection, not the registration path.
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
        (body_digest_pda, uninitialised_pda_account()),
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
        AccountMeta::new(body_digest_pda, false),
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

    // The wire no longer carries a caller-supplied bump: `create_pending_pda`
    // derives the canonical bump from the body-routed seeds and `invoke_signed`
    // only signs for that canonical address. A pending PDA at any other address
    // therefore cannot be created — the init CPI fails the signer-privilege
    // check and the runtime aborts with `PrivilegeEscalation` before any state
    // mutation. (Pre-fix, the attacker's prefix-supplied routing tuple let the
    // program sign for the spoofed address; that path no longer exists.)
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

    // Belt-and-braces: the attacker's pending PDA must still be uninitialised
    // after the rejection (the failed tx unwinds atomically).
    let attacker_after = find_account(&r.resulting_accounts, &attacker_pending_pda);
    assert_eq!(
        attacker_after.owner,
        system_program_id(),
        "attacker-supplied pending PDA must NOT be initialised after rejection"
    );
}

#[test]
fn submit_observations_rejects_unregistered_chain() {
    // Chain-registration parity with CosmWasm
    // (`cosmwasm/contracts/global-accountant/src/contract.rs:158-166`):
    // an observation whose body-header `emitter_chain` has no registration
    // PDA must be refused with `MissingChainRegistration`. Only an explicit
    // Token Bridge `RegisterChain` governance VAA can populate a
    // `ChainRegistration` PDA — otherwise we have no way to distinguish a
    // real Token Bridge emitter from a spoof at the same chain.
    //
    // Pre-impl this test fails because the program does not yet take a
    // chain-registration account slot. Post-impl the program inspects slot
    // 11 (a system-owned, zero-data account in this test) and rejects.
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0xA0);

    // Construct the meta list inline to add the chain-registration slot at
    // position 11. Mirrors `Scenario::account_metas` but routes the new
    // slot through an uninitialised PDA at the canonical seed for the body
    // chain.
    let (registration_pda, _) = derive_chain_registration_pda(scenario.chain);
    let signature = sign_digest(&scenario.guardians[0], &scenario.digest);

    // Replace the default scenario registration (pre-populated by
    // `initial_accounts`) with an uninitialised system-owned account at the
    // canonical PDA address. This drives the
    // `MissingChainRegistration` path in `chain_registration::load`.
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
        AccountMeta::new(scenario.digest_pda, false),
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
        &submit_ix_data(
            &scenario.digest,
            scenario.guardian_set_index,
            0,
            &signature,
            &scenario.body,
        ),
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

#[test]
fn submit_observations_rejects_wrong_emitter_for_registered_chain() {
    // The registration PDA exists and is canonical, but its recorded
    // `emitter_address` doesn't match the body header's. Mirrors CosmWasm
    // `contract.rs:163-166` "unknown emitter address" ensure check.
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0xA1);

    let registration_pubkey = scenario.chain_registration_pubkey;
    // Overwrite the default scenario registration with a different emitter
    // (everything-0xCC) so the on-disk emitter mismatches the body header
    // (scenario.emitter with byte 31 = 0x77).
    let wrong_emitter = [0xCCu8; 32];
    let mut accounts = scenario.initial_accounts();
    for entry in accounts.iter_mut() {
        if entry.0 == registration_pubkey {
            entry.1 = chain_registration_account(scenario.chain, &wrong_emitter);
            break;
        }
    }

    let signature = sign_digest(&scenario.guardians[0], &scenario.digest);
    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(
            &scenario.digest,
            scenario.guardian_set_index,
            0,
            &signature,
            &scenario.body,
        ),
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

#[test]
fn submit_observations_rejects_spoofed_registration_pda() {
    // Caller supplies a wrong-seed PDA in the registration slot. The
    // canonical-address check inside `verify_chain_registration` rejects
    // before any data read. Mirrors the bump/address pattern used for the
    // pending PDA and the noreplay bucket.
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0xA2);

    // Spoofed registration PDA: derived from a different chain ID so the
    // address mismatches the canonical for body chain.
    let (spoofed_pda, _) = derive_chain_registration_pda(99);
    assert_ne!(spoofed_pda, scenario.chain_registration_pubkey);

    let mut accounts = scenario.initial_accounts();
    // Add the spoofed PDA fixture (mollusk requires every meta-referenced
    // account to appear in the account list).
    accounts.push((spoofed_pda, chain_registration_account(99, &[0xAA; 32])));

    let mut metas = scenario.account_metas();
    let last_idx = metas.len() - 1;
    metas[last_idx] = AccountMeta::new_readonly(spoofed_pda, false);

    let signature = sign_digest(&scenario.guardians[0], &scenario.digest);
    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(
            &scenario.digest,
            scenario.guardian_set_index,
            0,
            &signature,
            &scenario.body,
        ),
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
            &scenario.digest,
            scenario.guardian_set_index,
            0,
            &signature,
            &scenario.body,
        ),
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

#[test]
fn submit_with_recovery_id_4_rejects() {
    // Recovery id must be in {0, 1, 2, 3}; byte 64 set to 4 trips the
    // short-circuit before `secp256k1_recover` is called.
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x52);
    let mut signature = sign_digest(&scenario.guardians[0], &scenario.digest);
    signature[64] = 4;

    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(
            &scenario.digest,
            scenario.guardian_set_index,
            0,
            &signature,
            &scenario.body,
        ),
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
fn submit_with_malformed_guardian_set_rejects() {
    // Drives each branch of `read_guardian_key`:
    //   (a) data shorter than the 8-byte header        -> InvalidPda
    //   (b) on_chain_index != wire-supplied index      -> InvalidGuardianIndex
    //   (c) guardian_index >= declared keys_len        -> InvalidGuardianIndex
    //   (d) keys array truncated before guardian_index -> InvalidPda
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
        // Same keys as the scenario, but the header encodes a different index.
        guardian_set_account(99, &scenario.guardian_keys(), 0, 0)
    };

    let short_keys_array = {
        // Declared keys_len = 3 but the wire-supplied guardian_index will be 5.
        let scenario = Scenario::new(19, 4, 0x53);
        let truncated: Vec<[u8; 20]> = scenario.guardians[..3]
            .iter()
            .map(|g| g.eth_address)
            .collect();
        guardian_set_account(4, &truncated, 0, 0)
    };

    let truncated_keys_buffer = {
        // Declared keys_len = 19 but the buffer only holds 5 keys, so an
        // 18th-key read overruns the slice.
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

    let cases: [(&str, Account, u8, u32); 4] = [
        (
            "truncated header",
            truncated_header,
            0,
            GlobalAccountantError::InvalidPda as u32,
        ),
        (
            "on-chain index mismatch",
            mismatched_index,
            0,
            GlobalAccountantError::InvalidGuardianIndex as u32,
        ),
        (
            "guardian_index >= keys_len",
            short_keys_array,
            5,
            GlobalAccountantError::InvalidGuardianIndex as u32,
        ),
        (
            "keys buffer truncated",
            truncated_keys_buffer,
            18,
            GlobalAccountantError::InvalidPda as u32,
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

        // Signature is valid for the chosen guardian — handler must short-circuit
        // before secp256k1_recover runs.
        let signature = sign_digest(
            &scenario.guardians[guardian_index as usize],
            &scenario.digest,
        );
        let ix = Instruction::new_with_bytes(
            program_id(),
            &submit_ix_data(
                &scenario.digest,
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
                let code = u64::from(err) as u32;
                assert_eq!(
                    code, expected_code,
                    "[{label}] expected code {expected_code}, got {code}"
                );
            }
            other => panic!("[{label}] expected Failure, got {other:?}"),
        }
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
        &old_guardians
            .iter()
            .map(|g| g.eth_address)
            .collect::<Vec<_>>(),
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
            &new_scenario.digest,
            4, // stale index
            1,
            &stale_signature,
            &new_scenario.body,
        ),
        new_scenario.account_metas(),
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
        &new_guardians
            .iter()
            .map(|g| g.eth_address)
            .collect::<Vec<_>>(),
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
        &submit_ix_data(&old_scenario.digest, 5, 0, &signature, &old_scenario.body),
        old_scenario.account_metas(),
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

#[test]
fn submit_with_different_digest_under_same_set_creates_sibling_bucket() {
    // Per-digest PDA seeds + digest-mismatch under the same guardian set
    // creates a sibling bucket, not a rejection. Source-chain reorgs that
    // change the VAA body's `timestamp` produce a different digest for the
    // same `(chain, emitter, sequence)`; the design routes those observations
    // into a distinct pending PDA whose seeds include the digest, letting
    // both digests race to quorum independently.
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x4C);

    // First observation under digest D1 — succeeds in the D1 bucket.
    let accounts_after_first = scenario.submit_n(&mollusk, 1);

    // Second observation under digest D2 with same GSI. Must SUCCEED into a
    // sibling PDA at a different canonical address (derived from the new
    // digest). The wire shape requires a body whose
    // `keccak256(keccak256(body))` matches the digest, so we mutate one byte
    // of the original body (the consistency_level field at offset 50) and
    // recompute the digest from there.
    let mut alternate_body = scenario.body.clone();
    alternate_body[50] = 0xAA;
    let alternate_digest = double_keccak256_host(&alternate_body);
    assert_ne!(
        alternate_digest, scenario.digest,
        "one-byte body change must yield a different digest"
    );
    let signature = sign_digest(&scenario.guardians[1], &alternate_digest);

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

    // Add the D2 pending PDA as an uninitialised slot in the account list.
    let mut accounts = accounts_after_first.clone();
    accounts.push((d2_pending_pda, uninitialised_pda_account()));

    let mut metas = scenario.account_metas();
    // Slot 1 is the pending PDA. Replace it with the D2 sibling.
    metas[1] = AccountMeta::new(d2_pending_pda, false);
    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(
            &alternate_digest,
            scenario.guardian_set_index,
            1,
            &signature,
            &alternate_body,
        ),
        metas,
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
    assert_eq!(
        d1_layout.signatures, 0b1,
        "D1 bucket still at one signature"
    );

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
    // (different address) under the same GSI. The wire shape requires a real
    // body whose double-keccak matches the digest, so we mutate two bytes of
    // the original body and recompute.
    let mut alternate_body = scenario.body.clone();
    // Mutate only consistency_level at byte 50 — bytes [42..50] hold the
    // sequence in the body header, and post-routing-fix the program reads the
    // bucket key directly from body[8..50]. Touching those bytes would change
    // the on-chain sequence to a value different from `scenario.sequence`,
    // mis-routing the pending PDA away from the seeds the test derives below.
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

    // (3 + 4) Drive 13 distinct D2 guardians: indices 0..13 from the same
    // set 6 guardian fixture, just signing D2 instead of D1.
    for i in 0..13u8 {
        let signature = sign_digest(&scenario.guardians[i as usize], &alternate_digest);
        let mut metas = scenario.account_metas();
        metas[1] = AccountMeta::new(d2_pending_pda, false);
        let ix = Instruction::new_with_bytes(
            program_id(),
            &submit_ix_data(
                &alternate_digest,
                scenario.guardian_set_index,
                i,
                &signature,
                &alternate_body,
            ),
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

    // (5) Post-quorum assertions:
    //   - NoReplay flipped for the shared `(chain, emitter, sequence)`.
    //   - DigestAccount opened with D2 (not D1).
    //   - D2 pending PDA closed (lamports drained, system-owned).
    //   - D1 pending PDA still exists with its 7 sigs.
    let bucket = find_account(&accounts, &scenario.noreplay_bucket_pubkey);
    assert_eq!(
        bucket.owner,
        Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        "NoReplay flipped on D2 quorum reach (shared bucket across siblings)"
    );
    assert_eq!(
        bucket.data.len(),
        NOREPLAY_BITMAP_OFFSET + NOREPLAY_BITMAP_BYTES
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
    // "NoReplay-marked" — must reclaim the D1 rent.

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
        entry.1 = noreplay_bucket_marked(scenario.sequence);
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
    assert_eq!(
        d1.lamports, 0,
        "stranded D1 lamports drained to recorded payer"
    );
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
        entry.1 = noreplay_bucket_marked(scenario.sequence);
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
fn close_pending_with_overflowing_rent_recipient_rejects() {
    // checked_add: rent_recipient at u64::MAX cannot accept any refund without
    // overflowing. Mirrors the analogous guard in submit_observations'
    // commit-close path; the close_pending entrypoint is the only place this
    // branch is host-testable.
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x60);
    let accounts_after_first = scenario.submit_n(&mollusk, 1);

    // Saturate the rent recipient and pre-mark noreplay so trigger (b) fires.
    let mut accounts = accounts_after_first.clone();
    if let Some(entry) = accounts
        .iter_mut()
        .find(|(k, _)| *k == scenario.noreplay_bucket_pubkey)
    {
        entry.1 = noreplay_bucket_marked(scenario.sequence);
    }
    if let Some(entry) = accounts.iter_mut().find(|(k, _)| *k == scenario.submitter) {
        entry.1.lamports = u64::MAX;
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
    let debug = format!("{:?}", r.program_result);
    assert!(
        debug.contains("ArithmeticOverflow"),
        "expected ArithmeticOverflow, got {debug}"
    );

    // Pending PDA must be untouched.
    let pending = find_account(&r.resulting_accounts, &scenario.pending_pda);
    assert_eq!(
        pending.owner,
        program_id(),
        "pending PDA still owned by program"
    );
    assert!(!pending.data.is_empty(), "pending PDA data preserved");
}

#[test]
fn close_pending_rejects_spoofed_guardian_set_owner() {
    // Regression test for a permanent-DoS vector: without verifying the
    // `guardian_set` account is owned by the Core Bridge, an attacker can
    // construct an account at an arbitrary address with bytes claiming the
    // set is "expired" (e.g. recording an index different from the pending
    // PDA's recorded index — the strict-superset branch of trigger (a)).
    // The current code would return `expired=true`, close the pending PDA,
    // and force defenders to re-submit every signature gathered so far.
    // Repeated indefinitely, this DoSes a `(chain, emitter, sequence, digest)`
    // bucket from ever reaching quorum.
    //
    // After the fix, `guardian_set_expired` checks the account owner against
    // the Core Bridge program ID up front and rejects with `InvalidPda` before
    // reading any bytes.
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x6A);
    let accounts_after_first = scenario.submit_n(&mollusk, 1);

    // Spoofed gs account: bytes encode index=99 (which mismatches recorded
    // index=4), owner = attacker pubkey (NOT the Core Bridge).
    let spoofed_gs = {
        let mut data = Vec::with_capacity(8 + scenario.guardian_keys().len() * 20 + 8);
        data.extend_from_slice(&99u32.to_le_bytes()); // index ≠ recorded
        data.extend_from_slice(&(scenario.guardian_keys().len() as u32).to_le_bytes());
        for key in scenario.guardian_keys() {
            data.extend_from_slice(&key);
        }
        data.extend_from_slice(&0u32.to_le_bytes()); // creation_time
        data.extend_from_slice(&0u32.to_le_bytes()); // expiration_time
        Account {
            lamports: 1_000_000,
            data,
            owner: Pubkey::new_from_array([0xDE; 32]), // attacker-controlled
            executable: false,
            rent_epoch: 0,
        }
    };
    let mut accounts = accounts_after_first.clone();
    if let Some(entry) = accounts
        .iter_mut()
        .find(|(k, _)| *k == scenario.guardian_set_pubkey)
    {
        entry.1 = spoofed_gs;
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
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidPda as u32,
                "spoofed-owner guardian_set must reject with InvalidPda, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidPda), got {other:?}"),
    }

    // Pending PDA must be untouched — the close path never reached the
    // lamport drain.
    let pending = find_account(&r.resulting_accounts, &scenario.pending_pda);
    assert_eq!(
        pending.owner,
        program_id(),
        "pending PDA still owned by program"
    );
    assert!(!pending.data.is_empty(), "pending PDA data preserved");
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

// ============================================================================
// Balance-accounting tests.
//
// The quorum-completing observation parses the body's Token Bridge payload
// and routes balance updates through `BalanceAccountLayout::lock_or_burn` /
// `unlock_or_mint` against the source-chain and destination-chain Account
// PDAs. Mirrors CosmWasm `commit_transfer`
// (`cosmwasm/packages/accountant/src/contract.rs:109-126`).
// ============================================================================

/// Helper: stamp out 13 distinct guardian observations against a Token
/// Bridge transfer scenario and return the post-tx account list. Used by all
/// of the balance-accounting happy-path tests so each scenario doesn't repeat
/// the 13-iteration accumulator loop.
fn drive_transfer_to_quorum(
    mollusk: &Mollusk,
    scenario: &Scenario,
) -> mollusk_svm::result::InstructionResult {
    let mut accounts = scenario.initial_accounts();
    for i in 0..PendingObservationsLayout::QUORUM_THRESHOLD as u8 {
        let result = scenario.submit_once(mollusk, accounts.clone(), i);
        assert!(
            matches!(result.program_result, ProgramResult::Success),
            "submit #{i} expected success, got {:?}",
            result.program_result
        );
        accounts = result.resulting_accounts.clone();
        // After the 13th the program returns success but PDA mutations are
        // applied; we want the final InstructionResult, not just the account
        // list, so the caller can inspect program_result + CU consumption.
        if i + 1 == PendingObservationsLayout::QUORUM_THRESHOLD as u8 {
            return result;
        }
    }
    unreachable!("loop above always returns on the final iteration")
}

#[test]
fn quorum_with_transfer_credits_native_chain_and_mints_wrapped_chain() {
    // Scenario: Ethereum (chain=2) emits a transfer of USDC (token_chain=2,
    // i.e. Ethereum-native) to Solana (chain=1). The accountant must
    //   - lock_or_burn on the source (Ethereum, USDC) Account: native ⇒ CREDIT,
    //   - unlock_or_mint on the dest (Solana, USDC) Account: wrapped ⇒ CREDIT.
    // Both Account PDAs lazy-init on this tx since they don't exist yet.
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
    let result = drive_transfer_to_quorum(&mollusk, &scenario);
    assert!(
        matches!(result.program_result, ProgramResult::Success),
        "quorum tx must succeed, got {:?}",
        result.program_result
    );

    // Source-chain Account: chain == token_chain == 2 ⇒ native lock ⇒ credited.
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

    // Destination-chain Account: chain (1) != token_chain (2) ⇒ wrapped mint ⇒ credited.
    let dst = find_account(&result.resulting_accounts, &scenario.dest_account_pubkey);
    assert_eq!(dst.owner, program_id(), "dest Account PDA owned by program");
    let dst_layout: &BalanceAccountLayout = bytemuck::from_bytes(&dst.data);
    assert_eq!(dst_layout.chain, 1);
    assert_eq!(dst_layout.token_chain, 2);
    assert_eq!(dst_layout.balance, Uint256::from_u128(500_000));
}

#[test]
fn quorum_with_transfer_underflows_when_wrapped_chain_has_insufficient_balance() {
    // Reverse direction: Solana (chain=1) sends wUSDC back to Ethereum.
    //   - Source = (Solana, chain != token_chain) ⇒ lock_or_burn DEBITs.
    //   - Source starts at zero (no prior mint observed) ⇒ underflow.
    // The whole tx must revert: NoReplay must not flip, DigestAccount must
    // not open, pending PDA must not close.
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
    // Re-seed `chain` so the VAA emitter is Solana, not Ethereum.
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
    let (pending_pda, _) = derive_pending_pda(
        scenario.chain,
        &scenario.emitter,
        scenario.sequence,
        &scenario.digest,
    );
    scenario.pending_pda = pending_pda;
    let (digest_pda, _) = derive_digest_pda(scenario.chain, &scenario.emitter, scenario.sequence);
    scenario.digest_pda = digest_pda;
    let (src, _) = derive_account_pda(1, 2, &token_address);
    let (dst, _) = derive_account_pda(2, 2, &token_address);
    // Re-derive the registration PDA for the new body chain — the default
    // scenario registered chain=2; this test's body is chain=1.
    let (registration_pda, _) = derive_chain_registration_pda(scenario.chain);
    scenario.chain_registration_pubkey = registration_pda;
    // Re-derive the noreplay bucket for the new chain (sequence/1024 bucket
    // also changes if scenario.chain shifts the namespace).
    scenario.noreplay_bucket_pubkey = derive_canonical_noreplay_bucket(
        &scenario.noreplay_authority_pubkey,
        scenario.chain,
        &scenario.emitter,
        scenario.sequence,
    );
    scenario.source_account_pubkey = src;
    scenario.dest_account_pubkey = dst;

    // Drive 12 observations through (pending PDA accumulates, no balance work
    // yet).
    let mut accounts = scenario.initial_accounts();
    for i in 0..12u8 {
        let r = scenario.submit_once(&mollusk, accounts.clone(), i);
        assert!(matches!(r.program_result, ProgramResult::Success));
        accounts = r.resulting_accounts;
    }
    // 13th observation: quorum reach + balance work → underflow.
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
    // Transactional integrity: the failed quorum tx must NOT have committed
    // partial state. The NoReplay bit, DigestAccount, and pending PDA stay
    // untouched (Solana txs are all-or-nothing).
    // Tx rolled back: bucket stays in lazy-create entry state.
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
    let digest = find_account(&r.resulting_accounts, &scenario.digest_pda);
    assert!(
        digest.data.is_empty(),
        "DigestAccount must not open on failed quorum"
    );
}

#[test]
fn quorum_with_lazy_init_destination_account_succeeds() {
    // Destination Account PDA doesn't exist yet — `init_or_upgrade_pda` lazy
    // creates it under the program. Same expectations as the happy-path test,
    // verified explicitly: pre-tx slot is system-owned with zero data; post-tx
    // slot is program-owned with `BalanceAccountLayout::LEN` bytes.
    let mollusk = mollusk();
    let token_address = [0x42u8; 32];
    let scenario = Scenario::with_transfer_body(19, 4, 0x62, 9_999u128, 2, token_address, 1);

    let initial = scenario.initial_accounts();
    let dst_pre = find_account(&initial, &scenario.dest_account_pubkey);
    assert_eq!(
        dst_pre.owner,
        system_program_id(),
        "dest Account PDA must start system-owned"
    );
    assert_eq!(
        dst_pre.data.len(),
        0,
        "dest Account PDA must start with zero data"
    );

    let result = drive_transfer_to_quorum(&mollusk, &scenario);
    assert!(matches!(result.program_result, ProgramResult::Success));

    let dst_post = find_account(&result.resulting_accounts, &scenario.dest_account_pubkey);
    assert_eq!(
        dst_post.owner,
        program_id(),
        "dest lazy-init flips owner to program"
    );
    assert_eq!(
        dst_post.data.len(),
        BalanceAccountLayout::LEN,
        "dest data sized to full layout"
    );
    let layout: &BalanceAccountLayout = bytemuck::from_bytes(&dst_post.data);
    assert_eq!(layout.balance, Uint256::from_u128(9_999));
}

#[test]
fn quorum_branch_cu_stays_below_ceiling() {
    // CU regression guard. The 13th observation commit branch with a Transfer
    // payload that lazy-inits both source and destination Account PDAs is the
    // most expensive hot-path tx in the program. Pinned against
    // `MAX_QUORUM_BRANCH_CU` so any future fat addition trips CI loudly.
    //
    // The const includes ~55% headroom over today's observed peak (~51K CU
    // under mock-noreplay) plus an estimated ~5K delta for the real NoReplay
    // CPI. If a regression appears, investigate before bumping the constant —
    // the budget is intentionally tight enough to catch unintended growth.
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

    // Drive the first 12 observations without checking CU — those are the
    // accumulator-only path (~30K CU each, no commit-branch fat).
    let mut accounts = scenario.initial_accounts();
    for i in 0..(PendingObservationsLayout::QUORUM_THRESHOLD as u8 - 1) {
        let r = scenario.submit_once(&mollusk, accounts.clone(), i);
        assert!(matches!(r.program_result, ProgramResult::Success));
        accounts = r.resulting_accounts;
    }

    // 13th observation — quorum reach, full commit branch.
    let result = scenario.submit_once(
        &mollusk,
        accounts,
        PendingObservationsLayout::QUORUM_THRESHOLD as u8 - 1,
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

#[test]
fn quorum_with_attest_payload_skips_balance_work_but_finishes_commit() {
    // Action 0x02 (Attest) carries no transfer data — the program must run
    // every other step of the commit branch (NoReplay flip, DigestAccount
    // open, pending close) but touch neither Account PDA. We re-use the
    // default `Scenario::new` which builds an Attest body.
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x63);
    let result = drive_transfer_to_quorum(&mollusk, &scenario);
    assert!(
        matches!(result.program_result, ProgramResult::Success),
        "attest quorum tx must succeed, got {:?}",
        result.program_result
    );

    // NoReplay flipped, DigestAccount opened — same as the existing quorum-
    // completing test. We don't re-assert all of that here; we just confirm
    // the sentinel Account PDA slots (== noreplay-authority) were not
    // touched (they remain system-owned with the dummy 0 lamports they
    // started with).
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

#[test]
fn quorum_with_unknown_payload_rejects_and_preserves_replay_slot() {
    // An action byte outside {0x01, 0x02, 0x03} must reject the
    // quorum-completing submission with `UnknownTokenBridgePayload`,
    // mirroring CosmWasm's `bail!("Unknown tokenbridge payload")`. The
    // failure rolls back the in-tx NoReplay mark, so the
    // `(chain, emitter, sequence)` slot stays unconsumed and a future
    // program upgrade that understands the action can still account the VAA.
    let mollusk = mollusk();
    let mut scenario = Scenario::new(19, 4, 0x65);
    scenario.body[51] = 0x05; // unknown Token Bridge action byte
    scenario.digest = double_keccak256_host(&scenario.body);
    let (pending_pda, _) = derive_pending_pda(
        scenario.chain,
        &scenario.emitter,
        scenario.sequence,
        &scenario.digest,
    );
    scenario.pending_pda = pending_pda;

    // The payload parse only happens on the quorum-completing branch, so the
    // first 12 observations accumulate normally.
    let accounts = scenario.submit_n(&mollusk, 12);

    // The 13th submission trips the commit branch and must reject.
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

    // The rejection reverts the NoReplay mark: the bucket stays uninitialised
    // (system-owned, no data), i.e. the replay slot is not consumed.
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

    // The pending bucket survives with its 12 accumulated signatures, ready
    // for `close_pending` cleanup once the guardian set expires.
    let pending = find_account(&result.resulting_accounts, &scenario.pending_pda);
    assert_eq!(pending.owner, program_id(), "pending PDA still live");
    let layout: &PendingObservationsLayout = bytemuck::from_bytes(&pending.data);
    assert_eq!(
        layout.signatures.count_ones(),
        12,
        "12 signatures still recorded in the surviving bucket"
    );
}

#[test]
fn quorum_with_body_digest_mismatch_rejects() {
    // Caller supplies the right digest in the fixed prefix but a body that
    // hashes to a different value. The pre-balance-work keccak check must
    // refuse the submission with `BodyDigestMismatch` and leave all state
    // untouched.
    let mollusk = mollusk();
    let scenario = Scenario::new(19, 4, 0x64);

    let mut tampered_body = scenario.body.clone();
    tampered_body[0] ^= 0xAA; // mutate the timestamp byte
    let signature = sign_digest(&scenario.guardians[0], &scenario.digest);
    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(
            &scenario.digest,
            scenario.guardian_set_index,
            0,
            &signature,
            &tampered_body, // body doesn't double-keccak to `digest`
        ),
        scenario.account_metas(),
    );
    let r = mollusk.process_instruction(&ix, &scenario.initial_accounts());
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::BodyDigestMismatch as u32,
                "expected BodyDigestMismatch, got {code:?}"
            );
        }
        other => panic!("expected Failure(BodyDigestMismatch), got {other:?}"),
    }
    // Pending PDA must remain uninitialised (the check fires before any PDA
    // work).
    let pending = find_account(&r.resulting_accounts, &scenario.pending_pda);
    assert_eq!(pending.owner, system_program_id());
    assert!(pending.data.is_empty());
}

#[test]
fn quorum_with_invalid_source_account_pda_rejects() {
    // Caller supplies a wrong-seed Account PDA in slot 8. The
    // `derive_account_pda` recompute inside the Transfer commit branch
    // catches the mismatch and rejects with `InvalidAccountPda`. The whole
    // tx unwinds (Solana atomicity).
    let mollusk = mollusk();
    let token_address = [0x99u8; 32];
    let mut scenario = Scenario::with_transfer_body(19, 4, 0x65, 100u128, 2, token_address, 1);
    // Replace source_account_pubkey with a spoofed address (same seeds but
    // wrong token chain). The verify check must reject.
    let (spoofed, _) = derive_account_pda(2, 99, &token_address);
    scenario.source_account_pubkey = spoofed;

    // Drive 12 observations. The submitter slot doesn't write through to
    // slot 8 in the non-quorum branch, so those submissions succeed.
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
