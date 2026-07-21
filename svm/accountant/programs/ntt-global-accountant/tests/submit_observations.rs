//! Integration tests for the NTT `submit_observations` transfer flow.
//!
//! Driven against a Mollusk instance with the real `solana_noreplay.so` loaded
//! at its canonical program ID (see `common::mollusk_fixtures`). Mirrors the WTT
//! `submit_observations` harness mechanics, swapping the WTT Token Bridge
//! account layout / payload for the NTT account layout (relayer / hub / two
//! peers / source+dest balance) and an NTT `TransceiverMessage` body.
//!
//! Covered:
//!   - happy-path transfer accounting (13 guardians → quorum; source/dest
//!     balances credited the NORMALIZED amount against the HUB token identity);
//!   - missing-hub rejection (`MissingTransceiverHub`);
//!   - peer cross-registration mismatch rejection (`PeerRegistrationMismatch`).

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        BalanceAccountLayout, GlobalAccountantError, Instruction as IxDiscriminator,
        TransceiverHubLayout, TransceiverPeerLayout, Uint256, ACCOUNT_SEED_PREFIX,
        CORE_BRIDGE_PROGRAM_ID, GUARDIAN_SET_SEED, NATIVE_TOKEN_TRANSFER_PREFIX,
        NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID,
        NTT_SUBMIT_OBSERVATION_PREFIX, PENDING_OBSERVATIONS_SEED_PREFIX, TRANSCEIVER_HUB_SEED_PREFIX,
        TRANSCEIVER_MESSAGE_PREFIX, TRANSCEIVER_PEER_SEED_PREFIX,
    },
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk},
    solana_account::Account,
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

mod common;
use common::guardian_fixtures::{guardian_set_account, make_guardians, sign_digest, Guardian};
use common::mollusk_fixtures::{keyed_account_for_noreplay_program, mollusk_with_fixtures};

const PROGRAM_NAME: &str = "ntt_global_accountant";
const GUARDIAN_COUNT: usize = 19;
const QUORUM: u8 = 13;
const GUARDIAN_SET_INDEX: u32 = 4;

fn program_id() -> Pubkey {
    Pubkey::new_from_array([9u8; 32])
}

fn mollusk() -> Mollusk {
    mollusk_with_fixtures(&program_id(), PROGRAM_NAME)
}

fn system_program_id() -> Pubkey {
    keyed_account_for_system_program().0
}

fn core_bridge_program_id() -> Pubkey {
    Pubkey::new_from_array(CORE_BRIDGE_PROGRAM_ID)
}

// ============================================================================
// PDA derivation
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

fn derive_hub_pda(chain: u16, address: &[u8; 32]) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    Pubkey::find_program_address(
        &[TRANSCEIVER_HUB_SEED_PREFIX, &chain_be, address],
        &program_id(),
    )
}

fn derive_peer_pda(chain: u16, address: &[u8; 32], dest_chain: u16) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    let dest_chain_be = dest_chain.to_be_bytes();
    Pubkey::find_program_address(
        &[
            TRANSCEIVER_PEER_SEED_PREFIX,
            &chain_be,
            address,
            &dest_chain_be,
        ],
        &program_id(),
    )
}

fn derive_relayer_registration_pda(chain: u16) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    Pubkey::find_program_address(
        &[
            global_accountant_definitions::RELAYER_CHAIN_REGISTRATION_SEED_PREFIX,
            &chain_be,
        ],
        &program_id(),
    )
}

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

// ============================================================================
// Wire builders
// ============================================================================

fn double_keccak256_host(body: &[u8]) -> [u8; 32] {
    let inner = solana_keccak_hasher::hashv(&[body]).to_bytes();
    solana_keccak_hasher::hashv(&[&inner]).to_bytes()
}

/// Deterministic source-chain transaction id carried in the wire format and
/// folded into the signing digest.
const TX_HASH: [u8; 32] = [0xA9_u8; 32];

/// Host mirror of `observation_signing_digest` under the NTT prefix:
/// `keccak256(NTT_SUBMIT_OBSERVATION_PREFIX ‖ tx_hash ‖ body)`. The digest a
/// guardian signs on the NTT observation path — distinct from the dedup digest.
fn signing_digest_for(body: &[u8]) -> [u8; 32] {
    solana_keccak_hasher::hashv(&[NTT_SUBMIT_OBSERVATION_PREFIX, &TX_HASH, body]).to_bytes()
}

/// Build a `TransceiverMessage` carrying one `NativeTokenTransfer`. Shape pinned
/// against `definitions::ntt` test builders. `decimals`/`raw_amount` feed the
/// `TrimmedAmount`; `to_chain` is the recipient chain.
fn build_ntt_message(decimals: u8, raw_amount: u64, to_chain: u16) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&TRANSCEIVER_MESSAGE_PREFIX);
    v.extend_from_slice(&[0xAA; 32]); // source_ntt_manager
    v.extend_from_slice(&[0xBB; 32]); // recipient_ntt_manager
    v.extend_from_slice(&145u16.to_be_bytes()); // ntt_manager_payload_len (informational)
    v.extend_from_slice(&[0xCC; 32]); // id
    v.extend_from_slice(&[0xDD; 32]); // sender (NTT manager message sender — unused by accountant)
    v.extend_from_slice(&79u16.to_be_bytes()); // inner payload_len (informational)
    v.extend_from_slice(&NATIVE_TOKEN_TRANSFER_PREFIX);
    v.push(decimals);
    v.extend_from_slice(&raw_amount.to_be_bytes());
    v.extend_from_slice(&[0xEE; 32]); // source_token (ignored: hub identity is used)
    v.extend_from_slice(&[0xFF; 32]); // to (ignored)
    v.extend_from_slice(&to_chain.to_be_bytes());
    v
}

/// Build a VAA body: 51-byte header (chain/emitter/sequence at [8..50]) followed
/// by the NTT message payload.
fn build_vaa_body(
    emitter_chain: u16,
    emitter_address: &[u8; 32],
    sequence: u64,
    ntt_payload: &[u8],
) -> Vec<u8> {
    let mut body = vec![0u8; 51];
    body[8..10].copy_from_slice(&emitter_chain.to_be_bytes());
    body[10..42].copy_from_slice(emitter_address);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body.extend_from_slice(ntt_payload);
    body
}

fn submit_ix_data(
    guardian_set_index: u32,
    guardian_index: u8,
    signature: &[u8; 65],
    body: &[u8],
) -> Vec<u8> {
    // Wire: discriminator + 70-byte fixed prefix + tx_hash(32) + 2-byte body len
    // (LE) + body. No digest or PDA bumps travel; the dedup digest and the
    // routing tuple are derived on-chain from the body header [8..50], and the
    // signing digest is reconstructed on-chain from NTT prefix ‖ tx_hash ‖ body.
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

fn noreplay_bucket_unmarked() -> Account {
    system_owned_account(0)
}

/// Program-owned `TransceiverHub` PDA registering `(chain, address) ->
/// (hub_chain, hub_address)`.
fn hub_account(chain: u16, address: &[u8; 32], hub_chain: u16, hub_address: &[u8; 32]) -> Account {
    let mut layout: TransceiverHubLayout = bytemuck::Zeroable::zeroed();
    layout.tag = TransceiverHubLayout::TAG;
    layout.chain = chain;
    layout.hub_chain = hub_chain;
    layout.address = *address;
    layout.hub_address = *hub_address;
    Account {
        lamports: 1_000_000,
        data: bytemuck::bytes_of(&layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

/// Program-owned `TransceiverPeer` PDA registering `(chain, address,
/// dest_chain) -> peer_address`.
fn peer_account(
    chain: u16,
    address: &[u8; 32],
    dest_chain: u16,
    peer_address: &[u8; 32],
) -> Account {
    let mut layout: TransceiverPeerLayout = bytemuck::Zeroable::zeroed();
    layout.tag = TransceiverPeerLayout::TAG;
    layout.chain = chain;
    layout.dest_chain = dest_chain;
    layout.address = *address;
    layout.peer_address = *peer_address;
    Account {
        lamports: 1_000_000,
        data: bytemuck::bytes_of(&layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn guardian_keys(guardians: &[Guardian]) -> Vec<[u8; 20]> {
    guardians.iter().map(|g| g.eth_address).collect()
}

// ============================================================================
// Scenario
// ============================================================================

/// A non-relayer NTT transfer scenario. The emitter is the transceiver itself
/// (so `sender == emitter`), routing through a hub on a third chain.
struct Scenario {
    /// Emitter chain == source chain.
    emitter_chain: u16,
    /// Emitter address == transceiver address == hub/peer routing `sender`.
    emitter: [u8; 32],
    /// Recipient chain (NTT `to_chain`).
    recipient_chain: u16,
    /// Hub token identity.
    hub_chain: u16,
    hub_address: [u8; 32],
    /// Peer transceiver address on the recipient chain.
    dest_peer: [u8; 32],
    /// Normalized transfer amount (the `TrimmedAmount` inputs feed `new()` only).
    normalized: Uint256,

    body: Vec<u8>,
    digest: [u8; 32],
    /// Signing digest = `keccak256(NTT prefix ‖ tx_hash ‖ body)`. What each
    /// guardian signature is verified against; distinct from `digest`.
    signing_digest: [u8; 32],
    guardians: Vec<Guardian>,

    submitter: Pubkey,
    pending_pda: Pubkey,
    guardian_set_pubkey: Pubkey,
    noreplay_bucket_pubkey: Pubkey,
    noreplay_authority_pubkey: Pubkey,
    relayer_registration_pubkey: Pubkey,
    hub_pubkey: Pubkey,
    peer_src_pubkey: Pubkey,
    peer_dst_pubkey: Pubkey,
    source_balance_pubkey: Pubkey,
    dest_balance_pubkey: Pubkey,
}

impl Scenario {
    /// Default scenario: hub native chain == emitter (source) chain, so the
    /// source side is a native lock (credit) and both balances start at zero and
    /// credit cleanly.
    fn new() -> Self {
        Self::with_hub_chain(2)
    }

    /// `hub_chain` controls the source-side native/wrapped dispatch: equal to
    /// the emitter chain ⇒ native lock (credit); different ⇒ wrapped burn
    /// (debit, requires a pre-seeded source balance).
    fn with_hub_chain(hub_chain: u16) -> Self {
        let emitter_chain: u16 = 2;
        let mut emitter = [0u8; 32];
        emitter[31] = 0x77;
        let sequence: u64 = 0x42;
        let recipient_chain: u16 = 10;
        let hub_address = [0x33u8; 32];
        let dest_peer = [0x88u8; 32];

        // 3 decimals → 8: ×10^5. 1000 → 100_000_000.
        let decimals = 3u8;
        let raw_amount = 1000u64;
        let normalized = Uint256::from_u128(100_000_000);

        let ntt = build_ntt_message(decimals, raw_amount, recipient_chain);
        let body = build_vaa_body(emitter_chain, &emitter, sequence, &ntt);
        let digest = double_keccak256_host(&body);
        let signing_digest = signing_digest_for(&body);

        let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
        let submitter = Pubkey::new_from_array([0x11u8; 32]);
        let (pending_pda, _) = derive_pending_pda(emitter_chain, &emitter, sequence, &digest);
        let (noreplay_authority_pubkey, _) =
            Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
        let noreplay_bucket_pubkey = derive_canonical_noreplay_bucket(
            &noreplay_authority_pubkey,
            emitter_chain,
            &emitter,
            sequence,
        );
        let (relayer_registration_pubkey, _) = derive_relayer_registration_pda(emitter_chain);
        let (hub_pubkey, _) = derive_hub_pda(emitter_chain, &emitter);
        let (peer_src_pubkey, _) = derive_peer_pda(emitter_chain, &emitter, recipient_chain);
        let (peer_dst_pubkey, _) = derive_peer_pda(recipient_chain, &dest_peer, emitter_chain);
        let (source_balance_pubkey, _) =
            derive_balance_account_pda(emitter_chain, hub_chain, &hub_address);
        let (dest_balance_pubkey, _) =
            derive_balance_account_pda(recipient_chain, hub_chain, &hub_address);
        // Canonical Guardian Set PDA; verify_signature pins the guardian-set
        // account to this address, so a non-canonical fixture would fail with
        // InvalidPda regardless of signature validity.
        let gsi_be = GUARDIAN_SET_INDEX.to_be_bytes();
        let (guardian_set_pubkey, _) = Pubkey::find_program_address(
            &[GUARDIAN_SET_SEED, &gsi_be],
            &core_bridge_program_id(),
        );

        Self {
            emitter_chain,
            emitter,
            recipient_chain,
            hub_chain,
            hub_address,
            dest_peer,
            normalized,
            body,
            digest,
            signing_digest,
            guardians,
            submitter,
            pending_pda,
            guardian_set_pubkey,
            noreplay_bucket_pubkey,
            noreplay_authority_pubkey,
            relayer_registration_pubkey,
            hub_pubkey,
            peer_src_pubkey,
            peer_dst_pubkey,
            source_balance_pubkey,
            dest_balance_pubkey,
        }
    }

    fn account_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.submitter, true),
            AccountMeta::new(self.pending_pda, false),
            AccountMeta::new_readonly(self.guardian_set_pubkey, false),
            AccountMeta::new(self.noreplay_bucket_pubkey, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(Pubkey::new_from_array(NOREPLAY_PROGRAM_ID), false),
            AccountMeta::new_readonly(self.noreplay_authority_pubkey, false),
            AccountMeta::new(self.submitter, false), // rent_recipient = submitter
            AccountMeta::new_readonly(self.relayer_registration_pubkey, false),
            AccountMeta::new_readonly(self.hub_pubkey, false),
            AccountMeta::new_readonly(self.peer_src_pubkey, false),
            AccountMeta::new_readonly(self.peer_dst_pubkey, false),
            AccountMeta::new(self.source_balance_pubkey, false),
            AccountMeta::new(self.dest_balance_pubkey, false),
        ]
    }

    /// Initial account list. Hub + both peers pre-seeded for a valid topology;
    /// negative tests override entries before calling `submit_n`.
    fn initial_accounts(&self) -> Vec<(Pubkey, Account)> {
        vec![
            (self.submitter, system_owned_account(50_000_000_000)),
            (self.pending_pda, uninitialised_pda_account()),
            (
                self.guardian_set_pubkey,
                guardian_set_account(
                    GUARDIAN_SET_INDEX,
                    &guardian_keys(&self.guardians),
                    0,
                    0,
                    &core_bridge_program_id(),
                ),
            ),
            (self.noreplay_bucket_pubkey, noreplay_bucket_unmarked()),
            keyed_account_for_system_program(),
            keyed_account_for_noreplay_program(),
            (self.noreplay_authority_pubkey, system_owned_account(0)),
            // relayer registration: unregistered (system-owned) ⇒ sender == emitter.
            (
                self.relayer_registration_pubkey,
                uninitialised_pda_account(),
            ),
            (
                self.hub_pubkey,
                hub_account(
                    self.emitter_chain,
                    &self.emitter,
                    self.hub_chain,
                    &self.hub_address,
                ),
            ),
            (
                self.peer_src_pubkey,
                peer_account(
                    self.emitter_chain,
                    &self.emitter,
                    self.recipient_chain,
                    &self.dest_peer,
                ),
            ),
            (
                self.peer_dst_pubkey,
                peer_account(
                    self.recipient_chain,
                    &self.dest_peer,
                    self.emitter_chain,
                    &self.emitter,
                ),
            ),
            (self.source_balance_pubkey, uninitialised_pda_account()),
            (self.dest_balance_pubkey, uninitialised_pda_account()),
        ]
    }

    fn submit_once(
        &self,
        mollusk: &Mollusk,
        starting_accounts: Vec<(Pubkey, Account)>,
        guardian_index: u8,
    ) -> mollusk_svm::result::InstructionResult {
        let signature = sign_digest(&self.guardians[guardian_index as usize], &self.signing_digest);
        let ix = Instruction::new_with_bytes(
            program_id(),
            &submit_ix_data(GUARDIAN_SET_INDEX, guardian_index, &signature, &self.body),
            self.account_metas(),
        );
        mollusk.process_instruction(&ix, &starting_accounts)
    }

    /// Drive `n` successful observations from indices `0..n`.
    fn submit_n(&self, mollusk: &Mollusk, n: u8) -> Vec<(Pubkey, Account)> {
        let mut accounts = self.initial_accounts();
        for i in 0..n {
            let r = self.submit_once(mollusk, accounts.clone(), i);
            assert!(
                matches!(r.program_result, ProgramResult::Success),
                "submit #{i} expected success, got {:?}",
                r.program_result
            );
            accounts = r.resulting_accounts.clone();
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

/// Happy path: 13 observations reach quorum; the source and destination balance
/// accounts are credited the NORMALIZED amount, keyed by the HUB token identity
/// `(hub_chain, hub_address)` — never the NTT `source_token`.
#[test]
fn ntt_transfer_credits_hub_token_identity_on_quorum() {
    let mollusk = mollusk();
    let scenario = Scenario::new();

    let accounts = scenario.submit_n(&mollusk, QUORUM);

    // Pending PDA closed on quorum.
    let pending = find_account(&accounts, &scenario.pending_pda);
    assert_eq!(pending.lamports, 0, "pending PDA drained on commit");
    assert_eq!(
        pending.owner,
        system_program_id(),
        "pending PDA reassigned to system on close"
    );

    // Source balance keyed by (emitter_chain, hub_chain, hub_address). Here
    // hub_chain == emitter_chain ⇒ native lock ⇒ credited the normalized amount.
    // The identity is the HUB token, never the NTT `source_token`.
    let source = find_account(&accounts, &scenario.source_balance_pubkey);
    let source_layout: &BalanceAccountLayout = bytemuck::from_bytes(&source.data);
    assert_eq!(
        source.owner,
        program_id(),
        "source balance owned by program"
    );
    assert_eq!(
        source_layout.token_chain, scenario.hub_chain,
        "source balance keyed by HUB chain, not NTT source_token"
    );
    assert_eq!(
        source_layout.token_address, scenario.hub_address,
        "source balance keyed by HUB address, not NTT source_token"
    );
    assert_eq!(
        source_layout.chain, scenario.emitter_chain,
        "source balance chain == emitter (source) chain"
    );
    assert_eq!(
        source_layout.balance, scenario.normalized,
        "source balance credited the NORMALIZED amount (native lock)"
    );

    let dest = find_account(&accounts, &scenario.dest_balance_pubkey);
    let dest_layout: &BalanceAccountLayout = bytemuck::from_bytes(&dest.data);
    assert_eq!(
        dest_layout.token_chain, scenario.hub_chain,
        "dest balance keyed by HUB chain"
    );
    assert_eq!(
        dest_layout.chain, scenario.recipient_chain,
        "dest balance chain == recipient chain"
    );
    // recipient_chain (10) != hub_chain (5) ⇒ wrapped mint ⇒ credit the
    // normalized amount.
    assert_eq!(
        dest_layout.balance, scenario.normalized,
        "dest balance credited the NORMALIZED amount (raw 1000 @ 3 decimals → 1e8)"
    );

    // NoReplay flipped.
    let bucket = find_account(&accounts, &scenario.noreplay_bucket_pubkey);
    assert_eq!(
        bucket.owner,
        Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        "bucket owned by solana_noreplay after MarkUsed"
    );
}

/// Source balance starts non-zero (pre-seeded wrapped holdings) so the
/// wrapped-burn on the source side succeeds and the resulting balances are
/// exactly debited / credited the normalized amount.
#[test]
fn ntt_transfer_debits_source_credits_dest_normalized() {
    let mollusk = mollusk();
    // Hub on a third chain (5) ≠ source chain (2) ⇒ source side is a wrapped
    // burn (debit); recipient chain (10) ≠ hub_chain ⇒ dest is a wrapped mint.
    let scenario = Scenario::with_hub_chain(5);

    // Pre-seed the source balance with enough wrapped holdings to burn.
    let mut accounts = scenario.initial_accounts();
    let pre = Uint256::from_u128(500_000_000);
    for entry in accounts.iter_mut() {
        if entry.0 == scenario.source_balance_pubkey {
            let mut layout: BalanceAccountLayout = bytemuck::Zeroable::zeroed();
            layout.tag = BalanceAccountLayout::TAG;
            layout.chain = scenario.emitter_chain;
            layout.token_chain = scenario.hub_chain;
            layout.token_address = scenario.hub_address;
            layout.balance = pre;
            entry.1 = Account {
                lamports: 2_000_000,
                data: bytemuck::bytes_of(&layout).to_vec(),
                owner: program_id(),
                executable: false,
                rent_epoch: 0,
            };
        }
    }

    for i in 0..QUORUM {
        let r = scenario.submit_once(&mollusk, accounts.clone(), i);
        assert!(
            matches!(r.program_result, ProgramResult::Success),
            "submit #{i} expected success, got {:?}",
            r.program_result
        );
        accounts = r.resulting_accounts.clone();
    }

    let source = find_account(&accounts, &scenario.source_balance_pubkey);
    let source_layout: &BalanceAccountLayout = bytemuck::from_bytes(&source.data);
    // source chain (2) != hub_chain (5) ⇒ wrapped burn ⇒ debit.
    assert_eq!(
        source_layout.balance,
        pre.checked_sub(scenario.normalized).unwrap(),
        "source balance debited the normalized amount (wrapped burn)"
    );

    let dest = find_account(&accounts, &scenario.dest_balance_pubkey);
    let dest_layout: &BalanceAccountLayout = bytemuck::from_bytes(&dest.data);
    assert_eq!(
        dest_layout.balance, scenario.normalized,
        "dest balance credited the normalized amount (wrapped mint)"
    );
}

/// Missing hub PDA: the transfer flow rejects with `MissingTransceiverHub`,
/// rolling back the NoReplay mark (so the slot stays unconsumed).
#[test]
fn ntt_transfer_missing_hub_rejects() {
    let mollusk = mollusk();
    let scenario = Scenario::new();

    // Accumulate 12 sub-quorum signatures (hub not yet needed).
    let mut accounts = scenario.submit_n(&mollusk, QUORUM - 1);

    // Wipe the hub PDA back to uninitialised before the quorum-completing submit.
    for entry in accounts.iter_mut() {
        if entry.0 == scenario.hub_pubkey {
            entry.1 = uninitialised_pda_account();
        }
    }

    let r = scenario.submit_once(&mollusk, accounts, QUORUM - 1);
    match r.program_result {
        ProgramResult::Failure(e) => {
            let code = u64::from(e) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::MissingTransceiverHub as u32,
                "expected MissingTransceiverHub, got {code:?}"
            );
        }
        other => panic!("expected Failure(MissingTransceiverHub), got {other:?}"),
    }
}

/// SECURITY regression: on the observations path the guardian-set account is the
/// sole authenticity anchor (no shim here). A forged guardian-set account — not
/// owned by the Core Bridge — must be rejected by `verify_signature` BEFORE any
/// signature is counted, even when it carries the real guardian keys and the
/// observation is signed by a genuine guardian. Without the owner check an
/// attacker could substitute their own keys/account and self-sign to quorum on a
/// fabricated transfer.
#[test]
fn ntt_transfer_forged_guardian_set_owner_rejects() {
    let mollusk = mollusk();
    let scenario = Scenario::new();

    let mut accounts = scenario.initial_accounts();
    // Re-own the guardian-set account to a non-Core-Bridge program, keeping the
    // exact same (well-formed, real) guardian keys/index/expiry bytes so that
    // ONLY the ownership constraint can be what rejects it.
    for entry in accounts.iter_mut() {
        if entry.0 == scenario.guardian_set_pubkey {
            entry.1 = guardian_set_account(
                GUARDIAN_SET_INDEX,
                &guardian_keys(&scenario.guardians),
                0,
                0,
                &program_id(), // attacker-chosen owner != Core Bridge
            );
        }
    }

    // Signed by a real guardian (index 0) against the real digest, so the
    // signature itself is valid — rejection is purely the owner check.
    let r = scenario.submit_once(&mollusk, accounts, 0);
    match r.program_result {
        ProgramResult::Failure(e) => {
            let code = u64::from(e) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidPda as u32,
                "expected InvalidPda for forged guardian-set owner, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidPda), got {other:?}"),
    }
}

/// Reverse peer registers a different transceiver than `sender`: the
/// cross-registration check rejects with `PeerRegistrationMismatch`.
#[test]
fn ntt_transfer_peer_cross_registration_mismatch_rejects() {
    let mollusk = mollusk();
    let scenario = Scenario::new();

    let mut accounts = scenario.submit_n(&mollusk, QUORUM - 1);

    // Corrupt the reverse peer so it registers a wrong source transceiver.
    for entry in accounts.iter_mut() {
        if entry.0 == scenario.peer_dst_pubkey {
            entry.1 = peer_account(
                scenario.recipient_chain,
                &scenario.dest_peer,
                scenario.emitter_chain,
                &[0xDE; 32], // != sender
            );
        }
    }

    let r = scenario.submit_once(&mollusk, accounts, QUORUM - 1);
    match r.program_result {
        ProgramResult::Failure(e) => {
            let code = u64::from(e) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::PeerRegistrationMismatch as u32,
                "expected PeerRegistrationMismatch, got {code:?}"
            );
        }
        other => panic!("expected Failure(PeerRegistrationMismatch), got {other:?}"),
    }
}

/// Domain-separation firewall (NTT): a guardian signature over the OLD bare
/// dedup digest `double_keccak256(body)` must be rejected. The wire carries the
/// correct dedup `digest` (so the body cross-check passes), but the program
/// verifies the signature against `keccak256(NTT prefix ‖ tx_hash ‖ body)`, so a
/// bare-digest signature recovers a different key and the observation is rejected
/// as `InvalidSignature`.
#[test]
fn submit_with_legacy_bare_digest_signature_is_rejected() {
    let mollusk = mollusk();
    let scenario = Scenario::new();

    assert_ne!(
        scenario.digest, scenario.signing_digest,
        "dedup digest and signing digest must differ for the firewall to bite"
    );

    let legacy_signature = sign_digest(&scenario.guardians[0], &scenario.digest);
    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_ix_data(GUARDIAN_SET_INDEX, 0, &legacy_signature, &scenario.body),
        scenario.account_metas(),
    );
    let r = mollusk.process_instruction(&ix, &scenario.initial_accounts());
    match r.program_result {
        ProgramResult::Failure(e) => {
            let code = u64::from(e) as u32;
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

/// Pins the NTT signing digest to a distinct domain from the bare dedup digest.
#[test]
fn signing_digest_differs_from_dedup_digest() {
    let scenario = Scenario::new();
    let signing = signing_digest_for(&scenario.body);
    let dedup = double_keccak256_host(&scenario.body);
    assert_ne!(
        signing, dedup,
        "NTT observation signing digest must be a distinct domain from the dedup digest"
    );
}
