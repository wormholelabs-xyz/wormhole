//! Integration tests for the NTT transfer applicator
//! (`instructions::ntt_transfer::apply_ntt_transfer`), driven end-to-end
//! through `submit_vaas` (the single-call Shim-verified path — no quorum
//! accumulation needed to reach the transfer-apply step).
//!
//! `submit_observations.rs` only exercises the non-relayer branch (sender ==
//! emitter) and the `MissingTransceiverHub` rejection. This file adds:
//!   - the relayer-unwrap path with an actually-registered relayer and a real
//!     `DeliveryInstruction`-wrapped payload;
//!   - a malformed/truncated `DeliveryInstruction` reaching the handler (not
//!     just the pure parser-level unit test in `definitions::ntt`);
//!   - `MissingTransceiverPeer`, distinct from the already-tested
//!     `MissingTransceiverHub`;
//!   - `BalanceOverflow` / `BalanceUnderflow` on the actual transfer-apply step.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        BalanceAccountLayout, GlobalAccountantError, Instruction as IxDiscriminator,
        RelayerChainRegistrationLayout, TransceiverHubLayout, TransceiverPeerLayout, Uint256,
        ACCOUNT_SEED_PREFIX, CORE_BRIDGE_PROGRAM_ID, DELIVERY_INSTRUCTION_PAYLOAD_ID,
        NATIVE_TOKEN_TRANSFER_PREFIX, NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITS_PER_BUCKET,
        NOREPLAY_PROGRAM_ID, RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, TRANSCEIVER_HUB_SEED_PREFIX,
        TRANSCEIVER_MESSAGE_PREFIX, TRANSCEIVER_PEER_SEED_PREFIX, VERIFY_VAA_SHIM_PROGRAM_ID,
    },
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk},
    solana_account::Account,
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

mod common;
use common::guardian_fixtures::{
    derive_guardian_set_pda, guardian_set_account, guardian_signatures_account, make_guardians,
    sign_digest, Guardian, GUARDIAN_PUBKEY_LENGTH,
};
use common::mollusk_fixtures::{
    keyed_account_for_noreplay_program, keyed_account_for_verify_vaa_shim_program,
    mollusk_with_fixtures,
};

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

fn shim_program_id() -> Pubkey {
    Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID)
}

fn double_keccak256_host(body: &[u8]) -> [u8; 32] {
    let inner = solana_keccak_hasher::hashv(&[body]).to_bytes();
    solana_keccak_hasher::hashv(&[&inner]).to_bytes()
}

// ============================================================================
// PDA derivation
// ============================================================================

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
        &[RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, &chain_be],
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

fn build_ntt_message(decimals: u8, raw_amount: u64, to_chain: u16) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&TRANSCEIVER_MESSAGE_PREFIX);
    v.extend_from_slice(&[0xAA; 32]); // source_ntt_manager
    v.extend_from_slice(&[0xBB; 32]); // recipient_ntt_manager
    v.extend_from_slice(&145u16.to_be_bytes());
    v.extend_from_slice(&[0xCC; 32]); // id
    v.extend_from_slice(&[0xDD; 32]); // sender (NTT manager message sender, unused)
    v.extend_from_slice(&79u16.to_be_bytes());
    v.extend_from_slice(&NATIVE_TOKEN_TRANSFER_PREFIX);
    v.push(decimals);
    v.extend_from_slice(&raw_amount.to_be_bytes());
    v.extend_from_slice(&[0xEE; 32]); // source_token (ignored)
    v.extend_from_slice(&[0xFF; 32]); // to (ignored)
    v.extend_from_slice(&to_chain.to_be_bytes());
    v
}

fn build_vaa_body(
    emitter_chain: u16,
    emitter_address: &[u8; 32],
    sequence: u64,
    payload: &[u8],
) -> Vec<u8> {
    let mut body = vec![0u8; 51];
    body[8..10].copy_from_slice(&emitter_chain.to_be_bytes());
    body[10..42].copy_from_slice(emitter_address);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body.extend_from_slice(payload);
    body
}

/// Build a `DeliveryInstruction` (standard-relayer wire format) wrapping
/// `inner` and recovering `sender` on unwrap. Mirrors
/// `definitions::ntt::tests::build_delivery` — see that module's doc comment
/// for the full field-by-field layout.
fn build_delivery_instruction(sender: [u8; 32], inner: &[u8], message_keys: u8) -> Vec<u8> {
    let mut v = Vec::new();
    v.push(DELIVERY_INSTRUCTION_PAYLOAD_ID);
    v.extend_from_slice(&7u16.to_be_bytes()); // target_chain
    v.extend_from_slice(&[0x01; 32]); // target_address
    v.extend_from_slice(&(inner.len() as u32).to_be_bytes());
    v.extend_from_slice(inner);
    v.extend_from_slice(&[0x02; 32]); // requested_reciever_value
    v.extend_from_slice(&[0x03; 32]); // extra_reciever_value
    v.extend_from_slice(&0u32.to_be_bytes()); // exec_info_len = 0
    v.extend_from_slice(&9u16.to_be_bytes()); // refund_chain
    v.extend_from_slice(&[0x04; 32]); // refund_address
    v.extend_from_slice(&[0x05; 32]); // refund_delivery_provider
    v.extend_from_slice(&[0x06; 32]); // source_delivery_provider
    v.extend_from_slice(&sender);
    v.push(message_keys);
    for _ in 0..message_keys {
        v.push(1); // KEY_TYPE_VAA
        v.extend_from_slice(&[0u8; 42]);
    }
    v
}

fn submit_vaas_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 1 + 2 + body.len());
    data.push(IxDiscriminator::SubmitVaas as u8);
    data.push(guardian_set_bump);
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

fn relayer_registration_account(chain: u16, emitter_address: &[u8; 32]) -> Account {
    let mut layout: RelayerChainRegistrationLayout = bytemuck::Zeroable::zeroed();
    layout.tag = RelayerChainRegistrationLayout::TAG;
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

fn balance_account(chain: u16, token_chain: u16, token_address: &[u8; 32], balance: Uint256) -> Account {
    let mut layout: BalanceAccountLayout = bytemuck::Zeroable::zeroed();
    layout.tag = BalanceAccountLayout::TAG;
    layout.chain = chain;
    layout.token_chain = token_chain;
    layout.token_address = *token_address;
    layout.balance = balance;
    Account {
        lamports: 2_000_000,
        data: bytemuck::bytes_of(&layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn guardian_keys(guardians: &[Guardian]) -> Vec<[u8; GUARDIAN_PUBKEY_LENGTH]> {
    guardians.iter().map(|g| g.eth_address).collect()
}

fn find_account<'a>(accounts: &'a [(Pubkey, Account)], key: &Pubkey) -> &'a Account {
    &accounts
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("account {key} not in result list"))
        .1
}

fn expect_failure(r: &mollusk_svm::result::InstructionResult, expected: GlobalAccountantError) {
    match &r.program_result {
        ProgramResult::Failure(e) => {
            let code = u64::from(e.clone()) as u32;
            assert_eq!(
                code, expected as u32,
                "expected {expected:?}, got code {code}"
            );
        }
        other => panic!("expected Failure({expected:?}), got {other:?}"),
    }
}

// ============================================================================
// Scenario: a single `submit_vaas` call driving `apply_ntt_transfer`, with all
// the routing knobs (relayer, hub, peers, balances) independently overridable.
// ============================================================================

struct TransferScenario {
    emitter_chain: u16,
    emitter: [u8; 32],
    /// The routing `sender`: equals `emitter` unless a relayer wrap is used.
    sender: [u8; 32],
    recipient_chain: u16,
    hub_chain: u16,
    hub_address: [u8; 32],
    dest_peer: [u8; 32],

    body: Vec<u8>,
    digest: [u8; 32],
    guardians: Vec<Guardian>,

    submitter: Pubkey,
    guardian_set_pubkey: Pubkey,
    guardian_set_bump: u8,
    guardian_signatures_pubkey: Pubkey,
    noreplay_bucket_pubkey: Pubkey,
    noreplay_authority_pubkey: Pubkey,
    relayer_registration_pubkey: Pubkey,
    hub_pubkey: Pubkey,
    peer_src_pubkey: Pubkey,
    peer_dst_pubkey: Pubkey,
    source_balance_pubkey: Pubkey,
    dest_balance_pubkey: Pubkey,

    /// Whether the relayer-registration PDA should actually be initialised to
    /// register `emitter` (forcing the relayer-unwrap branch).
    relayer_registered: bool,
}

impl TransferScenario {
    /// Non-relayer scenario: `sender == emitter`, routing/hub/peers keyed on
    /// `emitter` directly. `hub_chain` selects native-lock vs wrapped-burn on
    /// the source side (see `apply_balances`' dispatch).
    fn direct(seed: u8, hub_chain: u16) -> Self {
        let emitter_chain: u16 = 2;
        let mut emitter = [0u8; 32];
        emitter[0] = seed;
        emitter[31] = 0x77;
        Self::build(
            emitter_chain,
            emitter,
            emitter, // sender == emitter (no relayer)
            hub_chain,
            false,
            build_ntt_message(3, 1000, 10),
        )
    }

    /// Relayer-wrapped scenario: `emitter` is the registered relayer; `sender`
    /// (the recovered `DeliveryInstruction` sender) is the actual routing key.
    fn via_relayer(seed: u8, sender_seed: u8, hub_chain: u16) -> Self {
        let emitter_chain: u16 = 2;
        let mut emitter = [0u8; 32]; // the relayer's own address
        emitter[0] = seed;
        emitter[31] = 0x99;
        let mut sender = [0u8; 32]; // the real transceiver sender
        sender[0] = sender_seed;
        sender[31] = 0x77;

        let inner = build_ntt_message(3, 1000, 10);
        let delivery = build_delivery_instruction(sender, &inner, 0);
        Self::build(emitter_chain, emitter, sender, hub_chain, true, delivery)
    }

    fn build(
        emitter_chain: u16,
        emitter: [u8; 32],
        sender: [u8; 32],
        hub_chain: u16,
        relayer_registered: bool,
        payload: Vec<u8>,
    ) -> Self {
        let sequence: u64 = 0x42;
        let recipient_chain: u16 = 10;
        let hub_address = [0x33u8; 32];
        let dest_peer = [0x88u8; 32];

        let body = build_vaa_body(emitter_chain, &emitter, sequence, &payload);
        let digest = double_keccak256_host(&body);

        let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
        let submitter = Pubkey::new_from_array([0x11u8; 32]);
        let (guardian_set_pubkey, guardian_set_bump) =
            derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
        let (noreplay_authority_pubkey, _) =
            Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
        let noreplay_bucket_pubkey = derive_canonical_noreplay_bucket(
            &noreplay_authority_pubkey,
            emitter_chain,
            &emitter,
            sequence,
        );
        let (relayer_registration_pubkey, _) = derive_relayer_registration_pda(emitter_chain);
        // Hub/peer routing keys off `sender` (== emitter when unwrapped).
        let (hub_pubkey, _) = derive_hub_pda(emitter_chain, &sender);
        let (peer_src_pubkey, _) = derive_peer_pda(emitter_chain, &sender, recipient_chain);
        let (peer_dst_pubkey, _) = derive_peer_pda(recipient_chain, &dest_peer, emitter_chain);
        let (source_balance_pubkey, _) =
            derive_balance_account_pda(emitter_chain, hub_chain, &hub_address);
        let (dest_balance_pubkey, _) =
            derive_balance_account_pda(recipient_chain, hub_chain, &hub_address);

        Self {
            emitter_chain,
            emitter,
            sender,
            recipient_chain,
            hub_chain,
            hub_address,
            dest_peer,
            body,
            digest,
            guardians,
            submitter,
            guardian_set_pubkey,
            guardian_set_bump,
            guardian_signatures_pubkey: Pubkey::new_from_array([0xC4u8; 32]),
            noreplay_bucket_pubkey,
            noreplay_authority_pubkey,
            relayer_registration_pubkey,
            hub_pubkey,
            peer_src_pubkey,
            peer_dst_pubkey,
            source_balance_pubkey,
            dest_balance_pubkey,
            relayer_registered,
        }
    }

    fn account_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.submitter, true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(self.guardian_set_pubkey, false),
            AccountMeta::new_readonly(self.guardian_signatures_pubkey, false),
            AccountMeta::new(self.noreplay_bucket_pubkey, false),
            AccountMeta::new_readonly(Pubkey::new_from_array(NOREPLAY_PROGRAM_ID), false),
            AccountMeta::new_readonly(self.noreplay_authority_pubkey, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(self.relayer_registration_pubkey, false),
            AccountMeta::new_readonly(self.hub_pubkey, false),
            AccountMeta::new_readonly(self.peer_src_pubkey, false),
            AccountMeta::new_readonly(self.peer_dst_pubkey, false),
            AccountMeta::new(self.source_balance_pubkey, false),
            AccountMeta::new(self.dest_balance_pubkey, false),
        ]
    }

    fn guardian_signatures_for(&self, digest: &[u8; 32]) -> Account {
        let sigs: Vec<(u8, [u8; 65])> = (0..QUORUM)
            .map(|i| (i, sign_digest(&self.guardians[i as usize], digest)))
            .collect();
        guardian_signatures_account(
            GUARDIAN_SET_INDEX,
            &self.submitter,
            &sigs,
            &shim_program_id(),
        )
    }

    /// Full valid topology: relayer (if any), hub, both peers, empty balances.
    fn initial_accounts(&self) -> Vec<(Pubkey, Account)> {
        vec![
            (self.submitter, system_owned_account(50_000_000_000)),
            keyed_account_for_verify_vaa_shim_program(),
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
            (
                self.guardian_signatures_pubkey,
                self.guardian_signatures_for(&self.digest),
            ),
            (self.noreplay_bucket_pubkey, system_owned_account(0)),
            keyed_account_for_noreplay_program(),
            (self.noreplay_authority_pubkey, system_owned_account(0)),
            keyed_account_for_system_program(),
            (
                self.relayer_registration_pubkey,
                if self.relayer_registered {
                    relayer_registration_account(self.emitter_chain, &self.emitter)
                } else {
                    uninitialised_pda_account()
                },
            ),
            (
                self.hub_pubkey,
                hub_account(
                    self.emitter_chain,
                    &self.sender,
                    self.hub_chain,
                    &self.hub_address,
                ),
            ),
            (
                self.peer_src_pubkey,
                peer_account(
                    self.emitter_chain,
                    &self.sender,
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
                    &self.sender,
                ),
            ),
            (self.source_balance_pubkey, uninitialised_pda_account()),
            (self.dest_balance_pubkey, uninitialised_pda_account()),
        ]
    }

    fn submit(
        &self,
        mollusk: &Mollusk,
        starting_accounts: Vec<(Pubkey, Account)>,
    ) -> mollusk_svm::result::InstructionResult {
        let ix = Instruction::new_with_bytes(
            program_id(),
            &submit_vaas_ix_data(self.guardian_set_bump, &self.body),
            self.account_metas(),
        );
        mollusk.process_instruction(&ix, &starting_accounts)
    }
}

// ============================================================================
// Tests
// ============================================================================

/// Relayer-unwrap happy path: `emitter` is a registered relayer, so the body
/// payload is parsed as a `DeliveryInstruction`; the recovered `sender` (NOT
/// the emitter) is the hub/peer routing key, and the transfer applies exactly
/// as in the direct (non-relayer) case.
#[test]
fn ntt_transfer_relayer_unwrap_applies_transfer_from_recovered_sender() {
    let mollusk = mollusk();
    let scenario = TransferScenario::via_relayer(0xC0, 0xC1, 2);
    assert_ne!(
        scenario.sender, scenario.emitter,
        "relayer scenario must route on a sender distinct from the emitter"
    );

    let result = scenario.submit(&mollusk, scenario.initial_accounts());
    assert!(
        matches!(result.program_result, ProgramResult::Success),
        "relayer-unwrapped transfer must succeed, got {:?}",
        result.program_result
    );

    let source = find_account(&result.resulting_accounts, &scenario.source_balance_pubkey);
    let source_layout: &BalanceAccountLayout = bytemuck::from_bytes(&source.data);
    assert_eq!(
        source_layout.balance,
        Uint256::from_u128(100_000_000),
        "source credited the normalized amount via the recovered sender's hub"
    );
    let dest = find_account(&result.resulting_accounts, &scenario.dest_balance_pubkey);
    let dest_layout: &BalanceAccountLayout = bytemuck::from_bytes(&dest.data);
    assert_eq!(dest_layout.balance, Uint256::from_u128(100_000_000));
}

/// A registered relayer whose payload is a truncated `DeliveryInstruction`
/// (cut off mid-`sender_address`) reaches `apply_ntt_transfer` and rejects
/// with `InvalidInstructionData` at the full instruction-handler level — not
/// just the pure-parser unit test in `definitions::ntt`.
#[test]
fn ntt_transfer_malformed_delivery_instruction_from_relayer_rejects() {
    let mollusk = mollusk();
    let mut scenario = TransferScenario::via_relayer(0xC2, 0xC3, 2);

    // Truncate the delivery-instruction payload well before the end (cuts off
    // partway through `sender_address` / the trailing message-key count).
    let payload_start = 51; // VAA_BODY_HEADER_LEN
    let full_len = scenario.body.len();
    scenario.body.truncate(full_len - 20);
    assert!(
        scenario.body.len() > payload_start,
        "truncation must still leave a body longer than the header"
    );
    scenario.digest = double_keccak256_host(&scenario.body);

    let result = scenario.submit(&mollusk, scenario.initial_accounts());
    expect_failure(&result, GlobalAccountantError::InvalidInstructionData);

    // NoReplay stays unmarked: the failure rolls the whole tx back.
    let bucket = find_account(
        &result.resulting_accounts,
        &scenario.noreplay_bucket_pubkey,
    );
    assert_eq!(bucket.owner, system_program_id());
}

/// The reverse peer PDA `(recipient_chain, source_peer, emitter_chain)` is
/// missing (system-owned, never registered) — distinct from
/// `MissingTransceiverHub`: the hub resolves fine, and the source→dest peer
/// resolves fine, but the dest→source cross-check PDA was never written.
#[test]
fn ntt_transfer_missing_peer_rejects() {
    let mollusk = mollusk();
    let scenario = TransferScenario::direct(0xC4, 2);

    let mut accounts = scenario.initial_accounts();
    for entry in accounts.iter_mut() {
        if entry.0 == scenario.peer_dst_pubkey {
            entry.1 = uninitialised_pda_account();
        }
    }

    let result = scenario.submit(&mollusk, accounts);
    expect_failure(&result, GlobalAccountantError::MissingTransceiverPeer);
}

/// Native-lock credit (`hub_chain == emitter_chain`) pushed past
/// `Uint256::MAX` on the source side surfaces `BalanceOverflow` from the
/// actual `lock_or_burn` call in the transfer-apply step.
#[test]
fn ntt_transfer_source_balance_overflow_rejects() {
    let mollusk = mollusk();
    let scenario = TransferScenario::direct(0xC5, 2); // hub_chain == emitter_chain

    let mut max_minus_small = [0xFFu8; 32];
    max_minus_small[31] = 0x00; // Uint256::MAX - 0xFF, still < normalized amount (1e8)
    let mut accounts = scenario.initial_accounts();
    for entry in accounts.iter_mut() {
        if entry.0 == scenario.source_balance_pubkey {
            entry.1 = balance_account(
                scenario.emitter_chain,
                scenario.hub_chain,
                &scenario.hub_address,
                Uint256(max_minus_small),
            );
        }
    }

    let result = scenario.submit(&mollusk, accounts);
    expect_failure(&result, GlobalAccountantError::BalanceOverflow);
}

/// Wrapped-burn debit (`hub_chain != emitter_chain`) against an
/// insufficient source balance surfaces `BalanceUnderflow` from the actual
/// `lock_or_burn` call in the transfer-apply step.
#[test]
fn ntt_transfer_source_balance_underflow_rejects() {
    let mollusk = mollusk();
    // hub_chain (5) != emitter_chain (2) => wrapped burn (debit) on the source.
    let scenario = TransferScenario::direct(0xC6, 5);

    let mut accounts = scenario.initial_accounts();
    for entry in accounts.iter_mut() {
        if entry.0 == scenario.source_balance_pubkey {
            // Pre-fund with less than the normalized transfer amount (1e8).
            entry.1 = balance_account(
                scenario.emitter_chain,
                scenario.hub_chain,
                &scenario.hub_address,
                Uint256::from_u128(1_000),
            );
        }
    }

    let result = scenario.submit(&mollusk, accounts);
    expect_failure(&result, GlobalAccountantError::BalanceUnderflow);
}

/// The source balance account supplied at the instruction's source-balance
/// slot does not match the canonical `(emitter_chain, hub_chain, hub_address)`
/// PDA — `apply_balances` rejects with `InvalidAccountPda` before any balance
/// mutation or lazy-init is attempted.
#[test]
fn ntt_transfer_source_balance_wrong_pda_rejects() {
    let mollusk = mollusk();
    let mut scenario = TransferScenario::direct(0xC7, 2);

    // Substitute an arbitrary pubkey — not the canonical
    // `derive_balance_account_pda(emitter_chain, hub_chain, hub_address)` — at
    // the source-balance slot. Both the account meta and the supplied account
    // key move together since both `account_metas()` and `initial_accounts()`
    // read this field.
    scenario.source_balance_pubkey = Pubkey::new_from_array([0xDEu8; 32]);

    let mut accounts = scenario.initial_accounts();
    for entry in accounts.iter_mut() {
        if entry.0 == scenario.source_balance_pubkey {
            entry.1 = uninitialised_pda_account();
        }
    }

    let result = scenario.submit(&mollusk, accounts);
    expect_failure(&result, GlobalAccountantError::InvalidAccountPda);
}

/// The destination balance account supplied at the instruction's
/// destination-balance slot does not match the canonical
/// `(recipient_chain, hub_chain, hub_address)` PDA — rejected with
/// `InvalidAccountPda` after the source side has already been mutated
/// in-memory (but the failed transaction rolls the whole write back).
#[test]
fn ntt_transfer_dest_balance_wrong_pda_rejects() {
    let mollusk = mollusk();
    let mut scenario = TransferScenario::direct(0xC8, 2);

    scenario.dest_balance_pubkey = Pubkey::new_from_array([0xDFu8; 32]);

    let mut accounts = scenario.initial_accounts();
    for entry in accounts.iter_mut() {
        if entry.0 == scenario.dest_balance_pubkey {
            entry.1 = uninitialised_pda_account();
        }
    }

    let result = scenario.submit(&mollusk, accounts);
    expect_failure(&result, GlobalAccountantError::InvalidAccountPda);
}
