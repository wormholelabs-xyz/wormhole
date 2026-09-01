//! Integration tests for `register_hub` (NTT transceiver hub registration).
//!
//! Driven against a Mollusk instance with the real `solana_noreplay.so` and
//! `wormhole_verify_vaa_shim.so` loaded at their canonical program IDs (see
//! `common::mollusk_fixtures`). A hub VAA is a transfer-class NTT VAA whose
//! inner payload leads with `WormholeTransceiver::INFO_PREFIX`; only Locking
//! mode registers a hub. Mirrors the `register_relayer_chain` test mechanics.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        GlobalAccountantError, TransceiverHubLayout, CORE_BRIDGE_PROGRAM_ID,
        NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID,
        TRANSCEIVER_HUB_SEED_PREFIX, TRANSCEIVER_INFO_PREFIX, VaaBodyHeader,
        VERIFY_VAA_SHIM_PROGRAM_ID,
    },
    global_accountant_definitions::ntt_global_accountant::Instruction as IxDiscriminator,
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk},
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

/// Build a VAA body whose header carries `(emitter_chain, emitter_address,
/// sequence)` and whose payload (offset 51) is a `WormholeTransceiverInfo`
/// (`INFO_PREFIX`): prefix(4) | manager_address(32) | mode(1) | token(32) |
/// decimals(1).
fn build_hub_body(
    emitter_chain: u16,
    emitter_address: &[u8; 32],
    sequence: u64,
    mode: u8,
) -> Vec<u8> {
    let mut body = vec![0u8; VaaBodyHeader::LEN];
    body[8..10].copy_from_slice(&emitter_chain.to_be_bytes());
    body[10..42].copy_from_slice(emitter_address);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    // Payload (info message).
    body.extend_from_slice(&TRANSCEIVER_INFO_PREFIX);
    body.extend_from_slice(&[0x11u8; 32]); // manager_address
    body.push(mode); // 0 = Locking, 1 = Burning
    body.extend_from_slice(&[0x22u8; 32]); // token_address
    body.push(8); // token_decimals
    body
}

fn register_hub_ix_data(guardian_set_bump: u8, hub_bump: u8, body: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 1 + 1 + 2 + body.len());
    data.push(IxDiscriminator::RegisterHub as u8);
    data.push(guardian_set_bump);
    data.push(hub_bump);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
    data
}

/// Account slot order (see `register_hub::process`):
///   0. payer (SIGNER, WRITE)
///   1. Verify VAA Shim program
///   2. Core Bridge GuardianSet
///   3. GuardianSignatures
///   4. hub PDA (WRITE)
///   5. NoReplay bitmap PDA (WRITE)
///   6. NoReplay program
///   7. NoReplay authority PDA owned by this program
///   8. system program
fn build_metas(
    payer: Pubkey,
    guardian_set: Pubkey,
    guardian_signatures: Pubkey,
    hub_pda: Pubkey,
    noreplay_bucket: Pubkey,
    noreplay_authority: Pubkey,
) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(payer, true),
        AccountMeta::new_readonly(shim_program_id(), false),
        AccountMeta::new_readonly(guardian_set, false),
        AccountMeta::new_readonly(guardian_signatures, false),
        AccountMeta::new(hub_pda, false),
        AccountMeta::new(noreplay_bucket, false),
        AccountMeta::new_readonly(Pubkey::new_from_array(NOREPLAY_PROGRAM_ID), false),
        AccountMeta::new_readonly(noreplay_authority, false),
        AccountMeta::new_readonly(system_program_id(), false),
    ]
}

fn run_register_hub(
    mollusk: &Mollusk,
    body: &[u8],
    emitter_chain: u16,
    emitter_address: &[u8; 32],
    initial_hub: Option<Account>,
    initial_noreplay_bucket: Option<Account>,
) -> mollusk_svm::result::InstructionResult {
    let (hub_pda, hub_bump) = derive_hub_pda(emitter_chain, emitter_address);
    let payer = Pubkey::new_from_array([0x11u8; 32]);
    let guardian_signatures = Pubkey::new_from_array([0xC3u8; 32]);
    let (guardian_set, guardian_set_bump) =
        derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
    let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
    let digest = double_keccak256_host(body);
    let (noreplay_authority, _) =
        Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
    let sequence = {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&body[42..50]);
        u64::from_be_bytes(buf)
    };
    let noreplay_bucket = derive_canonical_noreplay_bucket(
        &noreplay_authority,
        emitter_chain,
        emitter_address,
        sequence,
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
            hub_pda,
            initial_hub.unwrap_or_else(uninitialised_pda_account),
        ),
        (
            noreplay_bucket,
            initial_noreplay_bucket.unwrap_or_else(|| system_owned_account(0)),
        ),
        keyed_account_for_noreplay_program(),
        (noreplay_authority, system_owned_account(0)),
        keyed_account_for_system_program(),
    ];
    let metas = build_metas(
        payer,
        guardian_set,
        guardian_signatures,
        hub_pda,
        noreplay_bucket,
        noreplay_authority,
    );
    let ix = Instruction::new_with_bytes(
        program_id(),
        &register_hub_ix_data(guardian_set_bump, hub_bump, body),
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

/// Happy path: a Locking-mode info VAA initialises the hub PDA pointing the
/// transceiver `(chain, address)` at itself.
#[test]
fn register_hub_locking_initialises_self_referential_hub() {
    let mollusk = mollusk();
    let emitter_chain: u16 = 2;
    let emitter_address = [0x77u8; 32];
    let body = build_hub_body(emitter_chain, &emitter_address, 0x01, 0); // Locking

    let r = run_register_hub(&mollusk, &body, emitter_chain, &emitter_address, None, None);
    assert!(
        matches!(r.program_result, ProgramResult::Success),
        "register_hub Locking happy-path must succeed, got {:?}",
        r.program_result
    );

    let (hub_pda, _) = derive_hub_pda(emitter_chain, &emitter_address);
    let post = r
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == hub_pda)
        .expect("hub PDA missing from result");
    assert_eq!(post.1.owner, program_id(), "hub PDA owned by program");
    assert_eq!(post.1.data.len(), TransceiverHubLayout::LEN);
    let layout: &TransceiverHubLayout = bytemuck::from_bytes(&post.1.data);
    assert_eq!(layout.tag, TransceiverHubLayout::TAG);
    assert_eq!(layout.chain, emitter_chain);
    assert_eq!(layout.hub_chain, emitter_chain, "hub points to itself");
    assert_eq!(layout.address, emitter_address);
    assert_eq!(layout.hub_address, emitter_address, "hub points to itself");
}

/// A Burning-mode info VAA is rejected with `NotLockingHub` (CosmWasm "ignoring
/// non-locking NTT initialization").
#[test]
fn register_hub_burning_rejects() {
    let mollusk = mollusk();
    let emitter_chain: u16 = 2;
    let emitter_address = [0x77u8; 32];
    let body = build_hub_body(emitter_chain, &emitter_address, 0x02, 1); // Burning
    let r = run_register_hub(&mollusk, &body, emitter_chain, &emitter_address, None, None);
    expect_failure(&r, GlobalAccountantError::NotLockingHub);
}

/// A second Locking VAA for an already-registered hub rejects via the NoReplay
/// pre-check when reusing the same sequence.
#[test]
fn register_hub_rejects_replay() {
    let mollusk = mollusk();
    let emitter_chain: u16 = 2;
    let emitter_address = [0x77u8; 32];
    let body = build_hub_body(emitter_chain, &emitter_address, 0x06, 0);

    let r1 = run_register_hub(&mollusk, &body, emitter_chain, &emitter_address, None, None);
    assert!(
        matches!(r1.program_result, ProgramResult::Success),
        "first hub registration must succeed, got {:?}",
        r1.program_result
    );

    let (hub_pda, _) = derive_hub_pda(emitter_chain, &emitter_address);
    let (noreplay_authority, _) =
        Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
    let post_hub = r1
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == hub_pda)
        .map(|(_, a)| a.clone())
        .expect("hub PDA missing from first result");
    let bucket = derive_canonical_noreplay_bucket(
        &noreplay_authority,
        emitter_chain,
        &emitter_address,
        0x06,
    );
    let post_bucket = r1
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == bucket)
        .map(|(_, a)| a.clone())
        .expect("noreplay bucket missing from first result");

    let r2 = run_register_hub(
        &mollusk,
        &body,
        emitter_chain,
        &emitter_address,
        Some(post_hub),
        Some(post_bucket),
    );
    expect_failure(&r2, GlobalAccountantError::AlreadyAccounted);
}

/// A fresh VAA (new sequence) targeting an already-initialised hub PDA rejects
/// with `DuplicateTransceiverHub` — re-registering a hub is not allowed.
#[test]
fn register_hub_duplicate_rejects() {
    let mollusk = mollusk();
    let emitter_chain: u16 = 2;
    let emitter_address = [0x77u8; 32];

    // Seed an already-initialised hub PDA (as if a prior registration ran).
    let mut layout: TransceiverHubLayout = bytemuck::Zeroable::zeroed();
    layout.tag = TransceiverHubLayout::TAG;
    layout.chain = emitter_chain;
    layout.hub_chain = emitter_chain;
    layout.address = emitter_address;
    layout.hub_address = emitter_address;
    let existing_hub = Account {
        lamports: 2_000_000,
        data: bytemuck::bytes_of(&layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    };

    // Use a NEW sequence so the NoReplay pre-check passes and we reach the
    // duplicate-hub check.
    let body = build_hub_body(emitter_chain, &emitter_address, 0x09, 0);
    let r = run_register_hub(
        &mollusk,
        &body,
        emitter_chain,
        &emitter_address,
        Some(existing_hub),
        None,
    );
    expect_failure(&r, GlobalAccountantError::DuplicateTransceiverHub);
}
