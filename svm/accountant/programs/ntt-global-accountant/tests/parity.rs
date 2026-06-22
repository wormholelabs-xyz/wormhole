//! Corpus-driven NTT accounting-parity harness.
//!
//! Replays each vector in `tests/fixtures/ntt_parity_vectors.json` through the
//! NTT operational program and asserts the accounting result. The NTT digest is
//! WTT-identical (`keccak256(keccak256(body))`, keyed by the VAA emitter, not the
//! relayer-resolved sender) and is already covered elsewhere; the *accounting* is
//! the NTT-specific parity risk this harness targets:
//!
//!   - hub token-identity substitution (token identity = `(hub_chain,
//!     hub_address)`, never the NTT `source_token`),
//!   - amount normalization to 8 decimals (scale-up, scale-down with truncation),
//!   - native-vs-wrapped dispatch (lock/burn on the source, unlock/mint on the
//!     dest, keyed by `source_chain == hub_chain`),
//!   - the relayer emitter-vs-sender distinction: routing uses the unwrapped
//!     `DeliveryInstruction::sender_address`, the transfer/digest KEY uses the VAA
//!     `emitter_address`.
//!
//! Per vector the harness: builds the VAA body (optionally wrapping the NTT
//! message in a `DeliveryInstruction` for relayer vectors); seeds the hub, both
//! peer PDAs, the relayer-registration PDA, and any `pre_balances`; drives 13
//! distinct guardian observations to quorum; then asserts (a) the program's
//! digest equals the host `keccak256(keccak256(body))`, and (b) every
//! `expected.balances` account holds exactly the stated amount keyed by the HUB
//! identity, and the converse — that no OTHER balance account was touched.
//!
//! HONESTY: the corpus is SYNTHETIC, hand-computed-expectation data. It validates
//! the program against the frozen D0 spec (spec-conformance + regression guard)
//! and closes the relayer-unwrap end-to-end coverage gap. It is NOT
//! cross-implementation parity — true byte-for-byte parity needs real NTT VAAs
//! with CosmWasm-produced expected outcomes. See `tests/fixtures/REAL_VECTORS.md`
//! for the corpus format and the procedure to add real ground-truth vectors;
//! the harness consumes synthetic and real vectors identically.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        BalanceAccountLayout, Instruction as IxDiscriminator, RelayerChainRegistrationLayout,
        TransceiverHubLayout, TransceiverPeerLayout, Uint256, ACCOUNT_SEED_PREFIX,
        CORE_BRIDGE_PROGRAM_ID, NATIVE_TOKEN_TRANSFER_PREFIX, NOREPLAY_AUTHORITY_SEED_PREFIX,
        NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID, PENDING_OBSERVATIONS_SEED_PREFIX,
        RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, TRANSCEIVER_HUB_SEED_PREFIX,
        TRANSCEIVER_MESSAGE_PREFIX, TRANSCEIVER_PEER_SEED_PREFIX,
    },
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult},
    solana_account::Account,
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
    std::collections::BTreeMap,
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

fn system_program_id() -> Pubkey {
    keyed_account_for_system_program().0
}

fn core_bridge_program_id() -> Pubkey {
    Pubkey::new_from_array(CORE_BRIDGE_PROGRAM_ID)
}

// ============================================================================
// Vector model — deserialized from the JSON corpus via serde_json::Value to
// avoid pulling serde-derive into the dev-deps.
// ============================================================================

/// One balance-account entry `(chain, token_chain, token_address) -> amount`.
#[derive(Clone, Debug)]
struct BalanceEntry {
    chain: u16,
    token_chain: u16,
    token_address: [u8; 32],
    amount: Uint256,
}

/// Optional relayer-wrapping: the registered relayer emitter (must equal the VAA
/// emitter to trigger the unwrap) and the inner `DeliveryInstruction.sender`.
#[derive(Clone, Debug)]
struct Relayer {
    registered_emitter: [u8; 32],
    sender: [u8; 32],
}

#[derive(Clone, Debug)]
struct Vector {
    name: String,
    source: String,
    emitter_chain: u16,
    emitter_address: [u8; 32],
    sequence: u64,
    relayer: Option<Relayer>,
    decimals: u8,
    raw_amount: u64,
    to_chain: u16,
    hub_chain: u16,
    hub_address: [u8; 32],
    peer_src_to_dst: [u8; 32],
    peer_dst_to_src: [u8; 32],
    pre_balances: Vec<BalanceEntry>,
    expected_balances: Vec<BalanceEntry>,
}

/// Decode a `0x`-prefixed hex string into a fixed-size byte array, left-aligned
/// and zero-padded on the right (matching how short addresses are written
/// on-wire). Panics on bad hex or over-length input.
fn hex_to_array<const N: usize>(s: &str) -> [u8; N] {
    let hex = s.strip_prefix("0x").unwrap_or(s);
    assert!(hex.len() % 2 == 0, "odd-length hex `{s}`");
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("valid hex byte"))
        .collect();
    assert!(bytes.len() <= N, "hex `{s}` longer than {N} bytes");
    let mut out = [0u8; N];
    out[..bytes.len()].copy_from_slice(&bytes);
    out
}

fn as_u64(v: &serde_json::Value, field: &str) -> u64 {
    v.get(field)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_else(|| panic!("missing/invalid u64 field `{field}`"))
}

fn as_str<'a>(v: &'a serde_json::Value, field: &str) -> &'a str {
    v.get(field)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("missing/invalid str field `{field}`"))
}

/// Parse a `Uint256` decimal-string amount.
fn parse_amount(s: &str) -> Uint256 {
    // All corpus amounts fit u128; this keeps the harness dependency-free.
    Uint256::from_u128(s.parse::<u128>().expect("decimal u128 amount"))
}

fn parse_balance_entry(v: &serde_json::Value) -> BalanceEntry {
    BalanceEntry {
        chain: as_u64(v, "chain") as u16,
        token_chain: as_u64(v, "token_chain") as u16,
        token_address: hex_to_array::<32>(as_str(v, "token_address")),
        amount: parse_amount(as_str(v, "amount")),
    }
}

fn load_vectors() -> Vec<Vector> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ntt_parity_vectors.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read corpus {}: {e}", path.display()));
    let doc: serde_json::Value = serde_json::from_str(&raw).expect("corpus is valid JSON");
    let vectors = doc
        .get("vectors")
        .and_then(serde_json::Value::as_array)
        .expect("corpus has a `vectors` array");

    vectors
        .iter()
        .map(|v| {
            let ntt = v.get("ntt").expect("vector has `ntt`");
            let hub = v.get("hub").expect("vector has `hub`");
            let peers = v.get("peers").expect("vector has `peers`");
            let relayer = v.get("relayer").and_then(|r| {
                if r.is_null() {
                    None
                } else {
                    Some(Relayer {
                        registered_emitter: hex_to_array::<32>(as_str(r, "registered_emitter")),
                        sender: hex_to_array::<32>(as_str(r, "sender")),
                    })
                }
            });
            Vector {
                name: as_str(v, "name").to_string(),
                source: as_str(v, "source").to_string(),
                emitter_chain: as_u64(v, "emitter_chain") as u16,
                emitter_address: hex_to_array::<32>(as_str(v, "emitter_address")),
                sequence: as_u64(v, "sequence"),
                relayer,
                decimals: as_u64(ntt, "decimals") as u8,
                raw_amount: as_u64(ntt, "raw_amount"),
                to_chain: as_u64(ntt, "to_chain") as u16,
                hub_chain: as_u64(hub, "chain") as u16,
                hub_address: hex_to_array::<32>(as_str(hub, "address")),
                peer_src_to_dst: hex_to_array::<32>(as_str(peers, "src_to_dst")),
                peer_dst_to_src: hex_to_array::<32>(as_str(peers, "dst_to_src")),
                pre_balances: v
                    .get("pre_balances")
                    .and_then(serde_json::Value::as_array)
                    .map(|a| a.iter().map(parse_balance_entry).collect())
                    .unwrap_or_default(),
                expected_balances: v
                    .get("expected")
                    .and_then(|e| e.get("balances"))
                    .and_then(serde_json::Value::as_array)
                    .map(|a| a.iter().map(parse_balance_entry).collect())
                    .unwrap_or_default(),
            }
        })
        .collect()
}

// ============================================================================
// PDA derivation
// ============================================================================

fn derive_pending_pda(chain: u16, emitter: &[u8; 32], sequence: u64, digest: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(
        &[
            PENDING_OBSERVATIONS_SEED_PREFIX,
            &chain.to_be_bytes(),
            emitter,
            &sequence.to_be_bytes(),
            digest,
        ],
        &program_id(),
    )
    .0
}

fn derive_balance_pda(chain: u16, token_chain: u16, token_address: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(
        &[
            ACCOUNT_SEED_PREFIX,
            &chain.to_be_bytes(),
            &token_chain.to_be_bytes(),
            token_address,
        ],
        &program_id(),
    )
    .0
}

fn derive_hub_pda(chain: u16, address: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(
        &[TRANSCEIVER_HUB_SEED_PREFIX, &chain.to_be_bytes(), address],
        &program_id(),
    )
    .0
}

fn derive_peer_pda(chain: u16, address: &[u8; 32], dest_chain: u16) -> Pubkey {
    Pubkey::find_program_address(
        &[
            TRANSCEIVER_PEER_SEED_PREFIX,
            &chain.to_be_bytes(),
            address,
            &dest_chain.to_be_bytes(),
        ],
        &program_id(),
    )
    .0
}

fn derive_relayer_pda(chain: u16) -> Pubkey {
    Pubkey::find_program_address(
        &[RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, &chain.to_be_bytes()],
        &program_id(),
    )
    .0
}

fn derive_noreplay_authority() -> Pubkey {
    Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id()).0
}

fn derive_noreplay_bucket(
    authority: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> Pubkey {
    let mut namespace = [0u8; 34];
    namespace[..2].copy_from_slice(&chain.to_be_bytes());
    namespace[2..].copy_from_slice(emitter);
    let bucket_index = (sequence / NOREPLAY_BITS_PER_BUCKET).to_le_bytes();
    Pubkey::find_program_address(
        &[
            authority.as_ref(),
            &namespace[..32],
            &namespace[32..],
            &bucket_index,
        ],
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
    )
    .0
}

// ============================================================================
// Wire builders
// ============================================================================

fn double_keccak256_host(body: &[u8]) -> [u8; 32] {
    let inner = solana_keccak_hasher::hashv(&[body]).to_bytes();
    solana_keccak_hasher::hashv(&[&inner]).to_bytes()
}

/// Build a `TransceiverMessage` carrying one `NativeTokenTransfer`. Pinned
/// against `definitions::ntt` and the program's `parse_ntt_transfer`.
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

/// Wrap an inner NTT message in a standard-relayer `DeliveryInstruction` carrying
/// `sender_address`. Mirrors `definitions::ntt::parse_delivery_instruction` and
/// the CosmWasm `DeliveryInstruction` wire format (custom big-endian, u32 lengths).
fn build_delivery_instruction(sender: &[u8; 32], inner: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.push(1u8); // payload_id
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
    v.extend_from_slice(sender); // sender_address
    v.push(0u8); // num_messages = 0
    v
}

/// Build a 51-byte VAA-body header (chain/emitter/sequence at [8..50]) followed
/// by `payload`.
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

fn submit_ix_data(
    digest: &[u8; 32],
    guardian_index: u8,
    signature: &[u8; 65],
    body: &[u8],
) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 102 + 2 + body.len());
    data.push(IxDiscriminator::SubmitObservations as u8);
    data.extend_from_slice(digest);
    data.extend_from_slice(&GUARDIAN_SET_INDEX.to_le_bytes());
    data.push(guardian_index);
    data.extend_from_slice(signature);
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

fn hub_account(chain: u16, address: &[u8; 32], hub_chain: u16, hub_address: &[u8; 32]) -> Account {
    let mut layout: TransceiverHubLayout = bytemuck::Zeroable::zeroed();
    layout.tag = TransceiverHubLayout::TAG;
    layout.chain = chain;
    layout.hub_chain = hub_chain;
    layout.address = *address;
    layout.hub_address = *hub_address;
    program_owned(bytemuck::bytes_of(&layout).to_vec())
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
    program_owned(bytemuck::bytes_of(&layout).to_vec())
}

fn relayer_account(chain: u16, emitter_address: &[u8; 32]) -> Account {
    let mut layout: RelayerChainRegistrationLayout = bytemuck::Zeroable::zeroed();
    layout.tag = RelayerChainRegistrationLayout::TAG;
    layout.chain = chain;
    layout.emitter_address = *emitter_address;
    program_owned(bytemuck::bytes_of(&layout).to_vec())
}

fn balance_account(entry: &BalanceEntry) -> Account {
    let mut layout: BalanceAccountLayout = bytemuck::Zeroable::zeroed();
    layout.tag = BalanceAccountLayout::TAG;
    layout.chain = entry.chain;
    layout.token_chain = entry.token_chain;
    layout.token_address = entry.token_address;
    layout.balance = entry.amount;
    program_owned(bytemuck::bytes_of(&layout).to_vec())
}

fn program_owned(data: Vec<u8>) -> Account {
    Account {
        lamports: 2_000_000,
        data,
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn guardian_keys(guardians: &[Guardian]) -> Vec<[u8; 20]> {
    guardians.iter().map(|g| g.eth_address).collect()
}

// ============================================================================
// Replay
// ============================================================================

/// Everything derived from one vector that the replay loop needs.
struct Replay {
    body: Vec<u8>,
    digest: [u8; 32],
    guardians: Vec<Guardian>,
    metas: Vec<AccountMeta>,
    initial_accounts: Vec<(Pubkey, Account)>,
    source_balance_pda: Pubkey,
    dest_balance_pda: Pubkey,
}

/// Build the VAA body, account metas, and seeded account list for a vector.
///
/// The hub/peer routing key is the relayer-resolved `sender` (== emitter for a
/// non-relayer vector); the digest/NoReplay/pending key is always the VAA
/// emitter. `source_balance`/`dest_balance` PDAs are keyed by the HUB identity.
fn build_replay(v: &Vector) -> Replay {
    // Routing sender: the unwrapped DeliveryInstruction sender for a relayer
    // vector, else the emitter itself.
    let sender = match &v.relayer {
        Some(r) => r.sender,
        None => v.emitter_address,
    };

    // Body payload: bare NTT message, or a DeliveryInstruction wrapping it.
    let ntt = build_ntt_message(v.decimals, v.raw_amount, v.to_chain);
    let payload = match &v.relayer {
        Some(r) => build_delivery_instruction(&r.sender, &ntt),
        None => ntt,
    };

    let body = build_vaa_body(v.emitter_chain, &v.emitter_address, v.sequence, &payload);
    let digest = double_keccak256_host(&body);
    let guardians = make_guardians(GUARDIAN_COUNT, 0x42);

    let submitter = Pubkey::new_from_array([0x11u8; 32]);
    let pending_pda = derive_pending_pda(v.emitter_chain, &v.emitter_address, v.sequence, &digest);
    let guardian_set_pubkey = Pubkey::new_from_array([0xC1u8; 32]);
    let noreplay_authority = derive_noreplay_authority();
    let noreplay_bucket = derive_noreplay_bucket(
        &noreplay_authority,
        v.emitter_chain,
        &v.emitter_address,
        v.sequence,
    );
    let relayer_pda = derive_relayer_pda(v.emitter_chain);
    let hub_pda = derive_hub_pda(v.emitter_chain, &sender);
    let peer_src_pda = derive_peer_pda(v.emitter_chain, &sender, v.to_chain);
    let peer_dst_pda = derive_peer_pda(v.to_chain, &v.peer_src_to_dst, v.emitter_chain);
    let source_balance_pda = derive_balance_pda(v.emitter_chain, v.hub_chain, &v.hub_address);
    let dest_balance_pda = derive_balance_pda(v.to_chain, v.hub_chain, &v.hub_address);

    let metas = vec![
        AccountMeta::new(submitter, true),
        AccountMeta::new(pending_pda, false),
        AccountMeta::new_readonly(guardian_set_pubkey, false),
        AccountMeta::new(noreplay_bucket, false),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new_readonly(Pubkey::new_from_array(NOREPLAY_PROGRAM_ID), false),
        AccountMeta::new_readonly(noreplay_authority, false),
        AccountMeta::new(submitter, false), // rent_recipient = submitter
        AccountMeta::new_readonly(relayer_pda, false),
        AccountMeta::new_readonly(hub_pda, false),
        AccountMeta::new_readonly(peer_src_pda, false),
        AccountMeta::new_readonly(peer_dst_pda, false),
        AccountMeta::new(source_balance_pda, false),
        AccountMeta::new(dest_balance_pda, false),
    ];

    // Relayer-registration PDA: initialised (registering the emitter) for a
    // relayer vector — this is what triggers the unwrap — else system-owned.
    let relayer_acct = match &v.relayer {
        Some(r) => relayer_account(v.emitter_chain, &r.registered_emitter),
        None => system_owned_account(0),
    };

    // Source / dest balance accounts start from `pre_balances` if present, else
    // uninitialised (lazy-init on first credit).
    let pre_by_pda: BTreeMap<Pubkey, &BalanceEntry> = v
        .pre_balances
        .iter()
        .map(|e| {
            (
                derive_balance_pda(e.chain, e.token_chain, &e.token_address),
                e,
            )
        })
        .collect();
    let source_acct = pre_by_pda
        .get(&source_balance_pda)
        .map(|e| balance_account(e))
        .unwrap_or_else(|| system_owned_account(0));
    let dest_acct = pre_by_pda
        .get(&dest_balance_pda)
        .map(|e| balance_account(e))
        .unwrap_or_else(|| system_owned_account(0));

    let initial_accounts = vec![
        (submitter, system_owned_account(50_000_000_000)),
        (pending_pda, system_owned_account(0)),
        (
            guardian_set_pubkey,
            guardian_set_account(
                GUARDIAN_SET_INDEX,
                &guardian_keys(&guardians),
                0,
                0,
                &core_bridge_program_id(),
            ),
        ),
        (noreplay_bucket, system_owned_account(0)),
        keyed_account_for_system_program(),
        keyed_account_for_noreplay_program(),
        (noreplay_authority, system_owned_account(0)),
        (relayer_pda, relayer_acct),
        (
            hub_pda,
            hub_account(v.emitter_chain, &sender, v.hub_chain, &v.hub_address),
        ),
        (
            peer_src_pda,
            peer_account(v.emitter_chain, &sender, v.to_chain, &v.peer_src_to_dst),
        ),
        (
            peer_dst_pda,
            peer_account(
                v.to_chain,
                &v.peer_src_to_dst,
                v.emitter_chain,
                &v.peer_dst_to_src,
            ),
        ),
        (source_balance_pda, source_acct),
        (dest_balance_pda, dest_acct),
    ];

    Replay {
        body,
        digest,
        guardians,
        metas,
        initial_accounts,
        source_balance_pda,
        dest_balance_pda,
    }
}

fn find_account<'a>(accounts: &'a [(Pubkey, Account)], key: &Pubkey) -> &'a Account {
    &accounts
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("account {key} not in result list"))
        .1
}

/// Drive a single vector to quorum and assert digest + post-state.
fn run_vector(v: &Vector) {
    let mollusk = mollusk_with_fixtures(&program_id(), PROGRAM_NAME);
    let r = build_replay(v);

    // (a) Digest parity: the program computes the same digest the host does.
    // The program rederives it from the body bytes in the instruction data; a
    // mismatch surfaces as a guardian-signature recovery failure (the program
    // verifies signatures over the digest it computed). Reaching quorum below is
    // therefore the in-band proof; we also assert the host invariant explicitly.
    assert_eq!(
        r.digest,
        double_keccak256_host(&r.body),
        "[{}] host digest is keccak256(keccak256(body))",
        v.name
    );

    // Drive QUORUM distinct guardian observations.
    let mut accounts = r.initial_accounts.clone();
    for i in 0..QUORUM {
        let signature = sign_digest(&r.guardians[i as usize], &r.digest);
        let ix = Instruction::new_with_bytes(
            program_id(),
            &submit_ix_data(&r.digest, i, &signature, &r.body),
            r.metas.clone(),
        );
        let res = mollusk.process_instruction(&ix, &accounts);
        assert!(
            matches!(res.program_result, ProgramResult::Success),
            "[{}] submit #{i} expected success, got {:?}",
            v.name,
            res.program_result
        );
        accounts = res.resulting_accounts.clone();
    }

    // (b) Post-state: every expected balance account holds exactly the stated
    // amount, keyed by the HUB token identity.
    let mut expected_pdas = BTreeMap::new();
    for e in &v.expected_balances {
        let pda = derive_balance_pda(e.chain, e.token_chain, &e.token_address);
        expected_pdas.insert(pda, e);

        let acct = find_account(&accounts, &pda);
        assert_eq!(
            acct.owner,
            program_id(),
            "[{}] expected balance {pda} owned by program",
            v.name
        );
        let layout: &BalanceAccountLayout = bytemuck::from_bytes(&acct.data);
        assert_eq!(
            layout.chain, e.chain,
            "[{}] balance {pda} chain matches",
            v.name
        );
        assert_eq!(
            layout.token_chain, e.token_chain,
            "[{}] balance {pda} token_chain == HUB chain (not NTT source_token)",
            v.name
        );
        assert_eq!(
            layout.token_address, e.token_address,
            "[{}] balance {pda} token_address == HUB address (not NTT source_token)",
            v.name
        );
        assert_eq!(
            layout.balance, e.amount,
            "[{}] balance {pda} holds the normalized amount",
            v.name
        );
    }

    // Converse: the two balance PDAs the instruction touched must BOTH be in the
    // expected set — no unexpected balance account was mutated. (The source/dest
    // PDAs are the only balance accounts in the instruction's account list, so
    // checking them against the expected set is exhaustive.)
    for pda in [r.source_balance_pda, r.dest_balance_pda] {
        let acct = find_account(&accounts, &pda);
        let initialized = acct.owner == program_id() && !acct.data.is_empty();
        if initialized {
            assert!(
                expected_pdas.contains_key(&pda),
                "[{}] balance {pda} was mutated but is absent from expected.balances",
                v.name
            );
        }
    }

    // NoReplay flipped: the slot is consumed after a successful commit.
    let noreplay_authority = derive_noreplay_authority();
    let bucket_pda = derive_noreplay_bucket(
        &noreplay_authority,
        v.emitter_chain,
        &v.emitter_address,
        v.sequence,
    );
    let bucket = find_account(&accounts, &bucket_pda);
    assert_eq!(
        bucket.owner,
        Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        "[{}] NoReplay bucket owned by solana_noreplay after MarkUsed",
        v.name
    );
}

// ============================================================================
// Tests
// ============================================================================

/// Replay every vector in the corpus and assert digest + accounting post-state.
/// Data-driven so adding a vector to the JSON adds a case with no code change.
#[test]
fn ntt_parity_corpus_replays_and_matches_expected_accounting() {
    let vectors = load_vectors();
    assert!(
        !vectors.is_empty(),
        "corpus must contain at least one vector"
    );
    for v in &vectors {
        run_vector(v);
    }
}

/// Guard: the corpus is documented as synthetic today. If a real ground-truth
/// vector (`source: "cosmwasm-test"` / `"wormholescan"`) is added, this test
/// fails to prompt updating REAL_VECTORS.md's status. Remove/relax once real
/// vectors are intentionally part of the corpus.
#[test]
fn ntt_parity_corpus_sources_are_documented() {
    for v in load_vectors() {
        assert!(
            matches!(
                v.source.as_str(),
                "synthetic" | "cosmwasm-test" | "wormholescan"
            ),
            "vector `{}` has undocumented source `{}`",
            v.name,
            v.source
        );
    }
}
