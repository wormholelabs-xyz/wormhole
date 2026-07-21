//! Integration tests for `register_peer` (NTT transceiver peer registration).
//!
//! Driven against a Mollusk instance with the real `solana_noreplay.so` and
//! `wormhole_verify_vaa_shim.so` loaded at their canonical program IDs (see
//! `common::mollusk_fixtures`). A peer VAA is a transfer-class NTT VAA whose
//! inner payload leads with `WormholeTransceiver::PEER_INFO_PREFIX`. Mirrors the
//! `register_relayer_chain` / `register_hub` test mechanics and exercises the
//! CosmWasm hub-match logic (contract.rs:577-633).

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        GlobalAccountantError, TransceiverHubLayout, TransceiverPeerLayout, CORE_BRIDGE_PROGRAM_ID,
        NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID,
        TRANSCEIVER_HUB_SEED_PREFIX, TRANSCEIVER_PEER_INFO_PREFIX, TRANSCEIVER_PEER_SEED_PREFIX,
        VAA_BODY_HEADER_LEN, VERIFY_VAA_SHIM_PROGRAM_ID,
    },
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk},
    ntt_global_accountant::Instruction as IxDiscriminator,
    solana_account::Account,
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

mod common;
use common::guardian_fixtures::{
    derive_guardian_set_pda, guardian_set_account, guardian_signatures_account, make_guardians,
    sign_digest, GUARDIAN_PUBKEY_LENGTH,
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

/// A program-owned hub account whose value is `(hub_chain, hub_address)`.
fn hub_account(chain: u16, address: &[u8; 32], hub_chain: u16, hub_address: &[u8; 32]) -> Account {
    let mut layout: TransceiverHubLayout = bytemuck::Zeroable::zeroed();
    layout.tag = TransceiverHubLayout::TAG;
    layout.chain = chain;
    layout.hub_chain = hub_chain;
    layout.address = *address;
    layout.hub_address = *hub_address;
    Account {
        lamports: 2_000_000,
        data: bytemuck::bytes_of(&layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

/// Build a VAA body whose payload (offset 51) is a
/// `WormholeTransceiverRegistration` (`PEER_INFO_PREFIX`): prefix(4) |
/// chain_id(u16) | transceiver_address(32).
fn build_peer_body(
    emitter_chain: u16,
    emitter_address: &[u8; 32],
    sequence: u64,
    dest_chain: u16,
    peer_address: &[u8; 32],
) -> Vec<u8> {
    let mut body = vec![0u8; VAA_BODY_HEADER_LEN];
    body[8..10].copy_from_slice(&emitter_chain.to_be_bytes());
    body[10..42].copy_from_slice(emitter_address);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body.extend_from_slice(&TRANSCEIVER_PEER_INFO_PREFIX);
    body.extend_from_slice(&dest_chain.to_be_bytes());
    body.extend_from_slice(peer_address);
    body
}

fn register_peer_ix_data(
    guardian_set_bump: u8,
    this_hub_bump: u8,
    peer_bump: u8,
    body: &[u8],
) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 1 + 1 + 1 + 2 + body.len());
    data.push(IxDiscriminator::RegisterPeer as u8);
    data.push(guardian_set_bump);
    data.push(this_hub_bump);
    data.push(peer_bump);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
    data
}

/// Account slot order (see `register_peer::process`):
///   0. payer (SIGNER, WRITE)
///   1. Verify VAA Shim program
///   2. Core Bridge GuardianSet
///   3. GuardianSignatures
///   4. peer's hub PDA `(dest_chain, peer_address)` (read)
///   5. this transceiver's own hub PDA `(emitter_chain, emitter_address)` (write/adopt)
///   6. peer PDA `(emitter_chain, emitter_address, dest_chain)` (WRITE)
///   7. NoReplay bitmap PDA (WRITE)
///   8. NoReplay program
///   9. NoReplay authority PDA owned by this program
///  10. system program
fn build_metas(
    payer: Pubkey,
    guardian_set: Pubkey,
    guardian_signatures: Pubkey,
    peer_hub_pda: Pubkey,
    own_hub_pda: Pubkey,
    peer_pda: Pubkey,
    noreplay_bucket: Pubkey,
    noreplay_authority: Pubkey,
) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(payer, true),
        AccountMeta::new_readonly(shim_program_id(), false),
        AccountMeta::new_readonly(guardian_set, false),
        AccountMeta::new_readonly(guardian_signatures, false),
        AccountMeta::new_readonly(peer_hub_pda, false),
        AccountMeta::new(own_hub_pda, false),
        AccountMeta::new(peer_pda, false),
        AccountMeta::new(noreplay_bucket, false),
        AccountMeta::new_readonly(Pubkey::new_from_array(NOREPLAY_PROGRAM_ID), false),
        AccountMeta::new_readonly(noreplay_authority, false),
        AccountMeta::new_readonly(system_program_id(), false),
    ]
}

struct PeerScenario {
    emitter_chain: u16,
    emitter_address: [u8; 32],
    sequence: u64,
    dest_chain: u16,
    peer_address: [u8; 32],
    /// State of the peer's hub PDA `(dest_chain, peer_address)`.
    peer_hub: Option<Account>,
    /// State of this transceiver's own hub PDA `(emitter_chain, emitter_address)`.
    own_hub: Option<Account>,
    /// State of the peer PDA to be written.
    peer: Option<Account>,
}

fn run_register_peer(
    mollusk: &Mollusk,
    s: &PeerScenario,
) -> mollusk_svm::result::InstructionResult {
    let body = build_peer_body(
        s.emitter_chain,
        &s.emitter_address,
        s.sequence,
        s.dest_chain,
        &s.peer_address,
    );
    let (peer_hub_pda, _) = derive_hub_pda(s.dest_chain, &s.peer_address);
    let (own_hub_pda, own_hub_bump) = derive_hub_pda(s.emitter_chain, &s.emitter_address);
    let (peer_pda, peer_bump) = derive_peer_pda(s.emitter_chain, &s.emitter_address, s.dest_chain);

    let payer = Pubkey::new_from_array([0x11u8; 32]);
    let guardian_signatures = Pubkey::new_from_array([0xC3u8; 32]);
    let (guardian_set, guardian_set_bump) =
        derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
    let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
    let digest = double_keccak256_host(&body);
    let (noreplay_authority, _) =
        Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
    let noreplay_bucket = derive_canonical_noreplay_bucket(
        &noreplay_authority,
        s.emitter_chain,
        &s.emitter_address,
        s.sequence,
    );

    let sigs: Vec<(u8, [u8; 65])> = (0..QUORUM)
        .map(|i| (i, sign_digest(&guardians[i as usize], &digest)))
        .collect();
    let keys: Vec<[u8; GUARDIAN_PUBKEY_LENGTH]> = guardians.iter().map(|g| g.eth_address).collect();

    let accounts = vec![
        (payer, system_owned_account(50_000_000_000)),
        keyed_account_for_verify_vaa_shim_program(),
        (
            guardian_set,
            guardian_set_account(GUARDIAN_SET_INDEX, &keys, 0, 0, &core_bridge_program_id()),
        ),
        (
            guardian_signatures,
            guardian_signatures_account(GUARDIAN_SET_INDEX, &payer, &sigs, &shim_program_id()),
        ),
        (
            peer_hub_pda,
            s.peer_hub.clone().unwrap_or_else(uninitialised_pda_account),
        ),
        (
            own_hub_pda,
            s.own_hub.clone().unwrap_or_else(uninitialised_pda_account),
        ),
        (
            peer_pda,
            s.peer.clone().unwrap_or_else(uninitialised_pda_account),
        ),
        (noreplay_bucket, system_owned_account(0)),
        keyed_account_for_noreplay_program(),
        (noreplay_authority, system_owned_account(0)),
        keyed_account_for_system_program(),
    ];
    let metas = build_metas(
        payer,
        guardian_set,
        guardian_signatures,
        peer_hub_pda,
        own_hub_pda,
        peer_pda,
        noreplay_bucket,
        noreplay_authority,
    );
    let ix = Instruction::new_with_bytes(
        program_id(),
        &register_peer_ix_data(guardian_set_bump, own_hub_bump, peer_bump, &body),
        metas,
    );
    mollusk.process_instruction(&ix, &accounts)
}

fn expect_failure(r: &mollusk_svm::result::InstructionResult, expected: GlobalAccountantError) {
    match &r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err.clone()) as u32;
            assert_eq!(
                code, expected as u32,
                "expected {expected:?}, got code {code}"
            );
        }
        other => panic!("expected Failure({expected:?}), got {other:?}"),
    }
}

/// Variant of `run_register_peer` that lets a test override the wire-supplied
/// `this_hub_bump` / `peer_bump` independently of the canonical values, to
/// exercise the on-chain bump-mismatch rejections.
fn run_register_peer_with_bumps(
    mollusk: &Mollusk,
    s: &PeerScenario,
    this_hub_bump_override: Option<u8>,
    peer_bump_override: Option<u8>,
) -> mollusk_svm::result::InstructionResult {
    let body = build_peer_body(
        s.emitter_chain,
        &s.emitter_address,
        s.sequence,
        s.dest_chain,
        &s.peer_address,
    );
    let (peer_hub_pda, _) = derive_hub_pda(s.dest_chain, &s.peer_address);
    let (own_hub_pda, own_hub_bump) = derive_hub_pda(s.emitter_chain, &s.emitter_address);
    let (peer_pda, peer_bump) = derive_peer_pda(s.emitter_chain, &s.emitter_address, s.dest_chain);

    let payer = Pubkey::new_from_array([0x11u8; 32]);
    let guardian_signatures = Pubkey::new_from_array([0xC3u8; 32]);
    let (guardian_set, guardian_set_bump) =
        derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
    let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
    let digest = double_keccak256_host(&body);
    let (noreplay_authority, _) =
        Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
    let noreplay_bucket = derive_canonical_noreplay_bucket(
        &noreplay_authority,
        s.emitter_chain,
        &s.emitter_address,
        s.sequence,
    );

    let sigs: Vec<(u8, [u8; 65])> = (0..QUORUM)
        .map(|i| (i, sign_digest(&guardians[i as usize], &digest)))
        .collect();
    let keys: Vec<[u8; GUARDIAN_PUBKEY_LENGTH]> = guardians.iter().map(|g| g.eth_address).collect();

    let accounts = vec![
        (payer, system_owned_account(50_000_000_000)),
        keyed_account_for_verify_vaa_shim_program(),
        (
            guardian_set,
            guardian_set_account(GUARDIAN_SET_INDEX, &keys, 0, 0, &core_bridge_program_id()),
        ),
        (
            guardian_signatures,
            guardian_signatures_account(GUARDIAN_SET_INDEX, &payer, &sigs, &shim_program_id()),
        ),
        (
            peer_hub_pda,
            s.peer_hub.clone().unwrap_or_else(uninitialised_pda_account),
        ),
        (
            own_hub_pda,
            s.own_hub.clone().unwrap_or_else(uninitialised_pda_account),
        ),
        (
            peer_pda,
            s.peer.clone().unwrap_or_else(uninitialised_pda_account),
        ),
        (noreplay_bucket, system_owned_account(0)),
        keyed_account_for_noreplay_program(),
        (noreplay_authority, system_owned_account(0)),
        keyed_account_for_system_program(),
    ];
    let metas = build_metas(
        payer,
        guardian_set,
        guardian_signatures,
        peer_hub_pda,
        own_hub_pda,
        peer_pda,
        noreplay_bucket,
        noreplay_authority,
    );
    let ix = Instruction::new_with_bytes(
        program_id(),
        &register_peer_ix_data(
            guardian_set_bump,
            this_hub_bump_override.unwrap_or(own_hub_bump),
            peer_bump_override.unwrap_or(peer_bump),
            &body,
        ),
        metas,
    );
    mollusk.process_instruction(&ix, &accounts)
}

/// A non-canonical `peer_bump` (wrong bump value for an otherwise-valid peer
/// PDA address) rejects with `InvalidPda`, regardless of the adopt/match
/// branch outcome — the bump check runs before either.
#[test]
fn register_peer_non_canonical_peer_bump_rejects() {
    let mollusk = mollusk();
    let emitter_chain = 2;
    let emitter_address = [0xAAu8; 32];
    let dest_chain = 4;
    let peer_address = [0xBBu8; 32];
    let peer_hub = hub_account(dest_chain, &peer_address, dest_chain, &peer_address);

    let s = PeerScenario {
        emitter_chain,
        emitter_address,
        sequence: 0x07,
        dest_chain,
        peer_address,
        peer_hub: Some(peer_hub),
        own_hub: None,
        peer: None,
    };
    let (_, canonical_peer_bump) = derive_peer_pda(emitter_chain, &emitter_address, dest_chain);
    let r = run_register_peer_with_bumps(
        &mollusk,
        &s,
        None,
        Some(canonical_peer_bump.wrapping_sub(1)),
    );
    expect_failure(&r, GlobalAccountantError::InvalidPda);
}

/// A non-canonical `this_hub_bump` in the adopt branch (source has no known
/// hub and the peer is itself a hub) rejects with `InvalidPda` before the own
/// hub PDA is initialised.
#[test]
fn register_peer_non_canonical_this_hub_bump_rejects() {
    let mollusk = mollusk();
    let emitter_chain = 2;
    let emitter_address = [0xAAu8; 32];
    let dest_chain = 4;
    let peer_address = [0xBBu8; 32];
    // Peer's hub points at itself => adopt branch is taken.
    let peer_hub = hub_account(dest_chain, &peer_address, dest_chain, &peer_address);

    let s = PeerScenario {
        emitter_chain,
        emitter_address,
        sequence: 0x08,
        dest_chain,
        peer_address,
        peer_hub: Some(peer_hub),
        own_hub: None,
        peer: None,
    };
    let (_, canonical_own_hub_bump) = derive_hub_pda(emitter_chain, &emitter_address);
    let r = run_register_peer_with_bumps(
        &mollusk,
        &s,
        Some(canonical_own_hub_bump.wrapping_sub(1)),
        None,
    );
    expect_failure(&r, GlobalAccountantError::InvalidPda);
}

/// The source's own hub PDA is program-owned (so the "already has a hub"
/// branch is taken) but its data is a truncated length — `read_hub` rejects
/// with `InvalidPda` before comparing hub identities.
#[test]
fn register_peer_corrupted_own_hub_wrong_length_rejects() {
    let mollusk = mollusk();
    let emitter_chain = 2;
    let emitter_address = [0xAAu8; 32];
    let dest_chain = 4;
    let peer_address = [0xBBu8; 32];
    let peer_hub = hub_account(dest_chain, &peer_address, 9, &[0xCCu8; 32]);

    // Program-owned own-hub PDA, but truncated (one byte short of the full
    // TransceiverHubLayout).
    let corrupted_own_hub = Account {
        lamports: 2_000_000,
        data: vec![0u8; TransceiverHubLayout::LEN - 1],
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    };

    let s = PeerScenario {
        emitter_chain,
        emitter_address,
        sequence: 0x09,
        dest_chain,
        peer_address,
        peer_hub: Some(peer_hub),
        own_hub: Some(corrupted_own_hub),
        peer: None,
    };
    let r = run_register_peer(&mollusk, &s);
    expect_failure(&r, GlobalAccountantError::InvalidPda);
}

/// The source's own hub PDA is program-owned and correctly sized, but stamped
/// with the wrong account-type tag — `read_hub` rejects with `InvalidPda`.
#[test]
fn register_peer_corrupted_own_hub_wrong_tag_rejects() {
    let mollusk = mollusk();
    let emitter_chain = 2;
    let emitter_address = [0xAAu8; 32];
    let dest_chain = 4;
    let peer_address = [0xBBu8; 32];
    let peer_hub = hub_account(dest_chain, &peer_address, 9, &[0xCCu8; 32]);

    let mut layout: TransceiverHubLayout = bytemuck::Zeroable::zeroed();
    layout.tag = TransceiverHubLayout::TAG.wrapping_add(1); // wrong tag
    layout.chain = emitter_chain;
    layout.hub_chain = 9;
    layout.address = emitter_address;
    layout.hub_address = [0xCCu8; 32];
    let corrupted_own_hub = Account {
        lamports: 2_000_000,
        data: bytemuck::bytes_of(&layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    };

    let s = PeerScenario {
        emitter_chain,
        emitter_address,
        sequence: 0x0A,
        dest_chain,
        peer_address,
        peer_hub: Some(peer_hub),
        own_hub: Some(corrupted_own_hub),
        peer: None,
    };
    let r = run_register_peer(&mollusk, &s);
    expect_failure(&r, GlobalAccountantError::InvalidPda);
}

/// Adopt case: the source transceiver has no known hub, and the peer it is
/// registering IS its own hub (peer_hub points at the peer). The source adopts
/// the peer as its hub and the peer PDA is written.
#[test]
fn register_peer_adopts_hub_when_peer_is_self_hub() {
    let mollusk = mollusk();
    let emitter_chain = 2;
    let emitter_address = [0xAAu8; 32];
    let dest_chain = 4;
    let peer_address = [0xBBu8; 32];

    // Peer's hub points at itself => it is a hub.
    let peer_hub = hub_account(dest_chain, &peer_address, dest_chain, &peer_address);

    let s = PeerScenario {
        emitter_chain,
        emitter_address,
        sequence: 0x01,
        dest_chain,
        peer_address,
        peer_hub: Some(peer_hub),
        own_hub: None, // no known hub yet
        peer: None,
    };
    let r = run_register_peer(&mollusk, &s);
    assert!(
        matches!(r.program_result, ProgramResult::Success),
        "adopt happy-path must succeed, got {:?}",
        r.program_result
    );

    // Peer PDA written with peer_address.
    let (peer_pda, _) = derive_peer_pda(emitter_chain, &emitter_address, dest_chain);
    let post_peer = r
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == peer_pda)
        .expect("peer PDA missing");
    assert_eq!(post_peer.1.owner, program_id());
    let pl: &TransceiverPeerLayout = bytemuck::from_bytes(&post_peer.1.data);
    assert_eq!(pl.tag, TransceiverPeerLayout::TAG);
    assert_eq!(pl.chain, emitter_chain);
    assert_eq!(pl.dest_chain, dest_chain);
    assert_eq!(pl.address, emitter_address);
    assert_eq!(pl.peer_address, peer_address);

    // Own hub PDA adopted (= peer's hub = (dest_chain, peer_address)).
    let (own_hub_pda, _) = derive_hub_pda(emitter_chain, &emitter_address);
    let post_hub = r
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == own_hub_pda)
        .expect("own hub PDA missing");
    assert_eq!(post_hub.1.owner, program_id());
    let hl: &TransceiverHubLayout = bytemuck::from_bytes(&post_hub.1.data);
    assert_eq!(hl.chain, emitter_chain);
    assert_eq!(hl.address, emitter_address);
    assert_eq!(hl.hub_chain, dest_chain, "adopted hub_chain");
    assert_eq!(hl.hub_address, peer_address, "adopted hub_address");
}

/// Match case: the source transceiver already has a hub equal to the peer's hub.
/// The peer PDA is written; no adoption.
#[test]
fn register_peer_succeeds_when_existing_hub_matches() {
    let mollusk = mollusk();
    let emitter_chain = 2;
    let emitter_address = [0xAAu8; 32];
    let dest_chain = 4;
    let peer_address = [0xBBu8; 32];

    // Shared hub on a third chain.
    let hub_chain = 9;
    let hub_address = [0xCCu8; 32];

    // Peer's hub and the source's own hub both point at the shared hub.
    let peer_hub = hub_account(dest_chain, &peer_address, hub_chain, &hub_address);
    let own_hub = hub_account(emitter_chain, &emitter_address, hub_chain, &hub_address);

    let s = PeerScenario {
        emitter_chain,
        emitter_address,
        sequence: 0x02,
        dest_chain,
        peer_address,
        peer_hub: Some(peer_hub),
        own_hub: Some(own_hub),
        peer: None,
    };
    let r = run_register_peer(&mollusk, &s);
    assert!(
        matches!(r.program_result, ProgramResult::Success),
        "matching-hub happy-path must succeed, got {:?}",
        r.program_result
    );
    let (peer_pda, _) = derive_peer_pda(emitter_chain, &emitter_address, dest_chain);
    let post_peer = r
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == peer_pda)
        .expect("peer PDA missing");
    let pl: &TransceiverPeerLayout = bytemuck::from_bytes(&post_peer.1.data);
    assert_eq!(pl.peer_address, peer_address);
}

/// The peer's hub PDA does not exist — `MissingTransceiverHub` (CosmWasm
/// `MissingHubRegistration`).
#[test]
fn register_peer_missing_peer_hub_rejects() {
    let mollusk = mollusk();
    let s = PeerScenario {
        emitter_chain: 2,
        emitter_address: [0xAAu8; 32],
        sequence: 0x03,
        dest_chain: 4,
        peer_address: [0xBBu8; 32],
        peer_hub: None, // missing
        own_hub: None,
        peer: None,
    };
    let r = run_register_peer(&mollusk, &s);
    expect_failure(&r, GlobalAccountantError::MissingTransceiverHub);
}

/// The source already has a hub that differs from the peer's hub —
/// `PeerRegistrationMismatch` (CosmWasm "peer hub does not match").
#[test]
fn register_peer_hub_mismatch_rejects() {
    let mollusk = mollusk();
    let emitter_chain = 2;
    let emitter_address = [0xAAu8; 32];
    let dest_chain = 4;
    let peer_address = [0xBBu8; 32];

    // Peer's hub on chain 9; source's own hub on a DIFFERENT chain 10.
    let peer_hub = hub_account(dest_chain, &peer_address, 9, &[0xCCu8; 32]);
    let own_hub = hub_account(emitter_chain, &emitter_address, 10, &[0xDDu8; 32]);

    let s = PeerScenario {
        emitter_chain,
        emitter_address,
        sequence: 0x04,
        dest_chain,
        peer_address,
        peer_hub: Some(peer_hub),
        own_hub: Some(own_hub),
        peer: None,
    };
    let r = run_register_peer(&mollusk, &s);
    expect_failure(&r, GlobalAccountantError::PeerRegistrationMismatch);
}

/// The source has no known hub and the peer is NOT its own hub —
/// `PeerBeforeHub` (CosmWasm "ignoring attempt to register peer before hub").
#[test]
fn register_peer_before_hub_rejects() {
    let mollusk = mollusk();
    let emitter_chain = 2;
    let emitter_address = [0xAAu8; 32];
    let dest_chain = 4;
    let peer_address = [0xBBu8; 32];

    // Peer's hub points at a DIFFERENT hub (chain 9), so the peer is not itself
    // a hub; the source has no hub to match against.
    let peer_hub = hub_account(dest_chain, &peer_address, 9, &[0xCCu8; 32]);

    let s = PeerScenario {
        emitter_chain,
        emitter_address,
        sequence: 0x05,
        dest_chain,
        peer_address,
        peer_hub: Some(peer_hub),
        own_hub: None,
        peer: None,
    };
    let r = run_register_peer(&mollusk, &s);
    expect_failure(&r, GlobalAccountantError::PeerBeforeHub);
}

/// A peer PDA that already exists rejects with `DuplicateTransceiverPeer`
/// (CosmWasm "peer entry for this chain already exists").
#[test]
fn register_peer_duplicate_rejects() {
    let mollusk = mollusk();
    let emitter_chain = 2;
    let emitter_address = [0xAAu8; 32];
    let dest_chain = 4;
    let peer_address = [0xBBu8; 32];

    let peer_hub = hub_account(dest_chain, &peer_address, dest_chain, &peer_address);

    // Pre-existing peer PDA.
    let mut pl: TransceiverPeerLayout = bytemuck::Zeroable::zeroed();
    pl.tag = TransceiverPeerLayout::TAG;
    pl.chain = emitter_chain;
    pl.dest_chain = dest_chain;
    pl.address = emitter_address;
    pl.peer_address = peer_address;
    let existing_peer = Account {
        lamports: 2_000_000,
        data: bytemuck::bytes_of(&pl).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    };

    let s = PeerScenario {
        emitter_chain,
        emitter_address,
        sequence: 0x06,
        dest_chain,
        peer_address,
        peer_hub: Some(peer_hub),
        own_hub: None,
        peer: Some(existing_peer),
    };
    let r = run_register_peer(&mollusk, &s);
    expect_failure(&r, GlobalAccountantError::DuplicateTransceiverPeer);
}
