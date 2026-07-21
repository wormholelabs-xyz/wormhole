//! Integration tests for `register_relayer_chain`.
//!
//! Driven against a Mollusk instance with the real `solana_noreplay.so` and
//! `wormhole_verify_vaa_shim.so` loaded at their canonical program IDs (see
//! `common::mollusk_fixtures`). Mirrors the WTT `register_chain` suite, swapping
//! in the `WormholeRelayer` governance module and the `RelayerChainRegistration`
//! destination layout/seed.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        GlobalAccountantError, RelayerChainRegistrationLayout, CORE_BRIDGE_PROGRAM_ID,
        GOVERNANCE_EMITTER, NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITS_PER_BUCKET,
        NOREPLAY_PROGRAM_ID, REGISTER_CHAIN_ACTION, RELAYER_CHAIN_REGISTRATION_SEED_PREFIX,
        RELAYER_GOVERNANCE_MODULE, SOLANA_CHAIN_ID, VERIFY_VAA_SHIM_PROGRAM_ID,
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

/// Host-side `keccak256(keccak256(body))` — the Wormhole digest convention.
fn double_keccak256_host(body: &[u8]) -> [u8; 32] {
    let inner = solana_keccak_hasher::hashv(&[body]).to_bytes();
    solana_keccak_hasher::hashv(&[&inner]).to_bytes()
}

fn derive_relayer_registration_pda(chain: u16) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    Pubkey::find_program_address(
        &[RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, &chain_be],
        &program_id(),
    )
}

/// Host-side derivation of the canonical NoReplay bitmap PDA.
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

fn noreplay_bucket_unmarked() -> Account {
    system_owned_account(0)
}

/// Build a `WormholeRelayer` `RegisterChain` governance VAA body. Layout:
///
/// | offset | size | field             |
/// |--------|------|-------------------|
/// | 8      | 2    | emitter_chain     |  (must be SOLANA_CHAIN_ID)
/// | 10     | 32   | emitter_address   |  (must be GOVERNANCE_EMITTER)
/// | 42     | 8    | sequence          |
/// | 51     | 32   | module            |  (RELAYER_GOVERNANCE_MODULE)
/// | 83     | 1    | action            |  (REGISTER_CHAIN_ACTION = 0x01)
/// | 84     | 2    | target_chain      |  (0 = Any, 1 = Solana)
/// | 86     | 2    | chain_to_register |
/// | 88     | 32   | emitter_to_register
///
/// Total: 120 bytes.
fn build_register_chain_body(
    governance_emitter_chain: u16,
    governance_emitter: &[u8; 32],
    sequence: u64,
    module: &[u8; 32],
    action: u8,
    target_chain: u16,
    chain_to_register: u16,
    emitter_to_register: &[u8; 32],
) -> Vec<u8> {
    let mut body = vec![0u8; 120];
    body[8..10].copy_from_slice(&governance_emitter_chain.to_be_bytes());
    body[10..42].copy_from_slice(governance_emitter);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body[51..83].copy_from_slice(module);
    body[83] = action;
    body[84..86].copy_from_slice(&target_chain.to_be_bytes());
    body[86..88].copy_from_slice(&chain_to_register.to_be_bytes());
    body[88..120].copy_from_slice(emitter_to_register);
    body
}

fn register_chain_ix_data(guardian_set_bump: u8, registration_bump: u8, body: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 1 + 1 + 2 + body.len());
    data.push(IxDiscriminator::RegisterRelayerChain as u8);
    data.push(guardian_set_bump);
    data.push(registration_bump);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
    data
}

/// Account slot order:
///   0. payer (SIGNER, WRITE)
///   1. Verify VAA Shim program
///   2. Core Bridge GuardianSet
///   3. GuardianSignatures
///   4. relayer-registration PDA (WRITE)
///   5. NoReplay bitmap PDA (WRITE)
///   6. NoReplay program
///   7. NoReplay authority PDA owned by this program
///   8. system program
fn build_metas(
    payer: Pubkey,
    guardian_set: Pubkey,
    guardian_signatures: Pubkey,
    registration_pda: Pubkey,
    noreplay_bucket: Pubkey,
    noreplay_authority: Pubkey,
) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(payer, true),
        AccountMeta::new_readonly(shim_program_id(), false),
        AccountMeta::new_readonly(guardian_set, false),
        AccountMeta::new_readonly(guardian_signatures, false),
        AccountMeta::new(registration_pda, false),
        AccountMeta::new(noreplay_bucket, false),
        AccountMeta::new_readonly(Pubkey::new_from_array(NOREPLAY_PROGRAM_ID), false),
        AccountMeta::new_readonly(noreplay_authority, false),
        AccountMeta::new_readonly(system_program_id(), false),
    ]
}

fn build_initial_accounts(
    payer: Pubkey,
    digest: &[u8; 32],
    guardian_set_pubkey: Pubkey,
    guardian_signatures_pubkey: Pubkey,
    guardians: &[Guardian],
    registration_pda: Pubkey,
    registration_state: Account,
    noreplay_bucket: Pubkey,
    noreplay_bucket_state: Account,
    noreplay_authority: Pubkey,
) -> Vec<(Pubkey, Account)> {
    let sigs: Vec<(u8, [u8; 65])> = (0..QUORUM)
        .map(|i| (i, sign_digest(&guardians[i as usize], digest)))
        .collect();
    let keys: Vec<[u8; GUARDIAN_PUBKEY_LENGTH]> = guardians.iter().map(|g| g.eth_address).collect();
    vec![
        (payer, system_owned_account(50_000_000_000)),
        keyed_account_for_verify_vaa_shim_program(),
        (
            guardian_set_pubkey,
            guardian_set_account(GUARDIAN_SET_INDEX, &keys, 0, 0, &core_bridge_program_id()),
        ),
        (
            guardian_signatures_pubkey,
            guardian_signatures_account(GUARDIAN_SET_INDEX, &payer, &sigs, &shim_program_id()),
        ),
        (registration_pda, registration_state),
        (noreplay_bucket, noreplay_bucket_state),
        keyed_account_for_noreplay_program(),
        (noreplay_authority, system_owned_account(0)),
        keyed_account_for_system_program(),
    ]
}

/// Single-call test driver over the standard fixture set.
fn run_register_relayer_chain(
    mollusk: &Mollusk,
    body: &[u8],
    chain_to_register: u16,
    initial_registration: Option<Account>,
    initial_noreplay_bucket: Option<Account>,
) -> mollusk_svm::result::InstructionResult {
    let (registration_pda, registration_bump) = derive_relayer_registration_pda(chain_to_register);
    let payer = Pubkey::new_from_array([0x11u8; 32]);
    let guardian_signatures = Pubkey::new_from_array([0xC3u8; 32]);
    let (guardian_set, guardian_set_bump) =
        derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
    let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
    let digest = double_keccak256_host(body);
    let (noreplay_authority, _) =
        Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
    let vaa_sequence = {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&body[42..50]);
        u64::from_be_bytes(buf)
    };
    let noreplay_bucket = derive_canonical_noreplay_bucket(
        &noreplay_authority,
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        vaa_sequence,
    );

    let accounts = build_initial_accounts(
        payer,
        &digest,
        guardian_set,
        guardian_signatures,
        &guardians,
        registration_pda,
        initial_registration.unwrap_or_else(uninitialised_pda_account),
        noreplay_bucket,
        initial_noreplay_bucket.unwrap_or_else(noreplay_bucket_unmarked),
        noreplay_authority,
    );
    let metas = build_metas(
        payer,
        guardian_set,
        guardian_signatures,
        registration_pda,
        noreplay_bucket,
        noreplay_authority,
    );

    let ix = Instruction::new_with_bytes(
        program_id(),
        &register_chain_ix_data(guardian_set_bump, registration_bump, body),
        metas,
    );
    mollusk.process_instruction(&ix, &accounts)
}

/// Happy path: a `WormholeRelayer` RegisterChain VAA targeting "Any"
/// initialises the canonical `RelayerChainRegistration` PDA.
#[test]
fn register_relayer_chain_via_governance_vaa_initialises_registration_pda() {
    let mollusk = mollusk();
    let chain_to_register: u16 = 2;
    let emitter_to_register = [0x77u8; 32];

    let body = build_register_chain_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x01,
        &RELAYER_GOVERNANCE_MODULE,
        REGISTER_CHAIN_ACTION,
        0, // target_chain = Any
        chain_to_register,
        &emitter_to_register,
    );

    let r = run_register_relayer_chain(&mollusk, &body, chain_to_register, None, None);
    assert!(
        matches!(r.program_result, ProgramResult::Success),
        "register_relayer_chain happy-path must succeed, got {:?}",
        r.program_result
    );

    let (registration_pda, _) = derive_relayer_registration_pda(chain_to_register);
    let post = r
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == registration_pda)
        .expect("registration PDA missing from result");
    assert_eq!(
        post.1.owner,
        program_id(),
        "registration PDA owned by program"
    );
    assert_eq!(
        post.1.data.len(),
        RelayerChainRegistrationLayout::LEN,
        "registration PDA allocated to full layout length"
    );
    let layout: &RelayerChainRegistrationLayout = bytemuck::from_bytes(&post.1.data);
    assert_eq!(
        layout.tag,
        RelayerChainRegistrationLayout::TAG,
        "tag stamped"
    );
    assert_eq!(layout.chain, chain_to_register, "chain field persisted");
    assert_eq!(
        layout.emitter_address, emitter_to_register,
        "emitter_address field persisted"
    );
}

/// A VAA carrying the WTT Token Bridge module (not the relayer module) is
/// rejected with `InvalidGovernanceModule`.
#[test]
fn register_relayer_chain_wrong_governance_module_rejects() {
    let mollusk = mollusk();
    // Any non-relayer module — use an arbitrary distinct 32-byte tag.
    let wrong_module = [0xAAu8; 32];
    let body = build_register_chain_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x02,
        &wrong_module,
        REGISTER_CHAIN_ACTION,
        0,
        2,
        &[0x77u8; 32],
    );
    let r = run_register_relayer_chain(&mollusk, &body, 2, None, None);
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidGovernanceModule as u32,
                "expected InvalidGovernanceModule, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidGovernanceModule), got {other:?}"),
    }
}

/// A VAA whose emitter chain is not `SOLANA_CHAIN_ID` is rejected with
/// `InvalidGovernanceEmitter` before the NoReplay pre-check or module/action
/// validation are reached.
#[test]
fn register_relayer_chain_wrong_emitter_chain_rejects() {
    let mollusk = mollusk();
    let body = build_register_chain_body(
        SOLANA_CHAIN_ID + 1, // wrong emitter chain
        &GOVERNANCE_EMITTER,
        0x03,
        &RELAYER_GOVERNANCE_MODULE,
        REGISTER_CHAIN_ACTION,
        0,
        2,
        &[0x77u8; 32],
    );
    let r = run_register_relayer_chain(&mollusk, &body, 2, None, None);
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidGovernanceEmitter as u32,
                "expected InvalidGovernanceEmitter, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidGovernanceEmitter), got {other:?}"),
    }
}

/// A VAA whose emitter address is not the canonical `GOVERNANCE_EMITTER` is
/// rejected with `InvalidGovernanceEmitter`, even with the correct emitter
/// chain.
#[test]
fn register_relayer_chain_wrong_emitter_address_rejects() {
    let mollusk = mollusk();
    let wrong_emitter = [0x02u8; 32];
    let body = build_register_chain_body(
        SOLANA_CHAIN_ID,
        &wrong_emitter,
        0x04,
        &RELAYER_GOVERNANCE_MODULE,
        REGISTER_CHAIN_ACTION,
        0,
        2,
        &[0x77u8; 32],
    );
    let r = run_register_relayer_chain(&mollusk, &body, 2, None, None);
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidGovernanceEmitter as u32,
                "expected InvalidGovernanceEmitter, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidGovernanceEmitter), got {other:?}"),
    }
}

/// A VAA carrying the correct relayer governance module but a non-`0x01`
/// action byte is rejected with `InvalidGovernanceAction`.
#[test]
fn register_relayer_chain_wrong_governance_action_rejects() {
    let mollusk = mollusk();
    let body = build_register_chain_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x05,
        &RELAYER_GOVERNANCE_MODULE,
        0xFF, // not REGISTER_CHAIN_ACTION
        0,
        2,
        &[0x77u8; 32],
    );
    let r = run_register_relayer_chain(&mollusk, &body, 2, None, None);
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidGovernanceAction as u32,
                "expected InvalidGovernanceAction, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidGovernanceAction), got {other:?}"),
    }
}

/// A VAA whose `target_chain` is neither `Any (0)` nor `SOLANA_CHAIN_ID` is
/// rejected with `GovernanceChainMismatch`.
#[test]
fn register_relayer_chain_wrong_target_chain_rejects() {
    let mollusk = mollusk();
    let body = build_register_chain_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x07,
        &RELAYER_GOVERNANCE_MODULE,
        REGISTER_CHAIN_ACTION,
        SOLANA_CHAIN_ID + 1, // neither Any (0) nor Solana
        2,
        &[0x77u8; 32],
    );
    let r = run_register_relayer_chain(&mollusk, &body, 2, None, None);
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::GovernanceChainMismatch as u32,
                "expected GovernanceChainMismatch, got {code:?}"
            );
        }
        other => panic!("expected Failure(GovernanceChainMismatch), got {other:?}"),
    }
}

/// The same governance VAA submitted twice: the second rejects via the NoReplay
/// pre-check.
#[test]
fn register_relayer_chain_rejects_replay() {
    let mollusk = mollusk();
    let body = build_register_chain_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x06,
        &RELAYER_GOVERNANCE_MODULE,
        REGISTER_CHAIN_ACTION,
        0,
        2,
        &[0x77u8; 32],
    );

    let r1 = run_register_relayer_chain(&mollusk, &body, 2, None, None);
    assert!(
        matches!(r1.program_result, ProgramResult::Success),
        "first registration must succeed, got {:?}",
        r1.program_result
    );

    // Carry the flipped bucket and initialised registration into the replay.
    let (registration_pda, _) = derive_relayer_registration_pda(2);
    let (noreplay_authority_pubkey, _) =
        Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
    let post_registration = r1
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == registration_pda)
        .map(|(_, a)| a.clone())
        .expect("registration PDA missing from first result");
    let post_bucket = r1
        .resulting_accounts
        .iter()
        .find(|(k, _)| {
            *k == derive_canonical_noreplay_bucket(
                &noreplay_authority_pubkey,
                SOLANA_CHAIN_ID,
                &GOVERNANCE_EMITTER,
                0x06,
            )
        })
        .map(|(_, a)| a.clone())
        .expect("noreplay bucket missing from first result");

    let r2 = run_register_relayer_chain(
        &mollusk,
        &body,
        2,
        Some(post_registration),
        Some(post_bucket),
    );
    match r2.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::AlreadyAccounted as u32,
                "expected AlreadyAccounted on replay, got {code:?}"
            );
        }
        other => panic!("expected Failure(AlreadyAccounted), got {other:?}"),
    }
}

/// A higher-sequence VAA registering a different relayer emitter overwrites
/// the existing `RelayerChainRegistration` PDA in place. Replaying the
/// original, now-stale VAA afterward rejects `AlreadyAccounted`.
#[test]
fn register_relayer_chain_rotation_overwrites_at_higher_sequence() {
    let mollusk = mollusk();
    let chain_to_register: u16 = 2;
    let emitter_a = [0x77u8; 32];
    let emitter_b = [0xBBu8; 32];
    let seq_n: u64 = 0x20;
    let seq_rotate: u64 = seq_n + 1024; // distinct noreplay bucket under the real CPI

    let body_a = build_register_chain_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        seq_n,
        &RELAYER_GOVERNANCE_MODULE,
        REGISTER_CHAIN_ACTION,
        0,
        chain_to_register,
        &emitter_a,
    );
    let r1 = run_register_relayer_chain(&mollusk, &body_a, chain_to_register, None, None);
    assert!(
        matches!(r1.program_result, ProgramResult::Success),
        "first registration (emitter_a) must succeed, got {:?}",
        r1.program_result
    );

    let (registration_pda, _) = derive_relayer_registration_pda(chain_to_register);
    let post_registration_a = r1
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == registration_pda)
        .map(|(_, a)| a.clone())
        .expect("registration PDA missing after first register");

    // Rotate to emitter_b via a higher-sequence VAA; use a fresh noreplay
    // bucket since seq_rotate falls in a different bucket.
    let body_b = build_register_chain_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        seq_rotate,
        &RELAYER_GOVERNANCE_MODULE,
        REGISTER_CHAIN_ACTION,
        0,
        chain_to_register,
        &emitter_b,
    );
    let r2 = run_register_relayer_chain(
        &mollusk,
        &body_b,
        chain_to_register,
        Some(post_registration_a),
        Some(noreplay_bucket_unmarked()),
    );
    assert!(
        matches!(r2.program_result, ProgramResult::Success),
        "rotation to emitter_b must succeed, got {:?}",
        r2.program_result
    );
    let post_registration_b = r2
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == registration_pda)
        .map(|(_, a)| a.clone())
        .expect("registration PDA missing after rotation");
    assert_eq!(
        post_registration_b.owner,
        program_id(),
        "registration PDA still program-owned after rotation"
    );
    let layout: &RelayerChainRegistrationLayout = bytemuck::from_bytes(&post_registration_b.data);
    assert_eq!(
        layout.emitter_address, emitter_b,
        "registration overwritten to point at the new relayer emitter"
    );

    // Replaying the ORIGINAL emitter_a@seq_n VAA against the rotated state
    // rejects AlreadyAccounted (the seq_n bucket was flipped by the first call).
    let (noreplay_authority_pubkey, _) =
        Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
    let marked_bucket_n = r1
        .resulting_accounts
        .iter()
        .find(|(k, _)| {
            *k == derive_canonical_noreplay_bucket(
                &noreplay_authority_pubkey,
                SOLANA_CHAIN_ID,
                &GOVERNANCE_EMITTER,
                seq_n,
            )
        })
        .map(|(_, a)| a.clone())
        .expect("seq_n bucket missing after first register");
    let r3 = run_register_relayer_chain(
        &mollusk,
        &body_a,
        chain_to_register,
        Some(post_registration_b),
        Some(marked_bucket_n),
    );
    match r3.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::AlreadyAccounted as u32,
                "replay of the stale emitter_a VAA must reject AlreadyAccounted, got {code:?}"
            );
        }
        other => panic!("expected Failure(AlreadyAccounted), got {other:?}"),
    }
}

/// An existing, program-owned `RelayerChainRegistration` PDA with a corrupted
/// (too-short) data length rejects with `InvalidPda`.
#[test]
fn register_relayer_chain_corrupted_existing_pda_wrong_length_rejects() {
    let mollusk = mollusk();
    let chain_to_register: u16 = 2;
    let body = build_register_chain_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x25,
        &RELAYER_GOVERNANCE_MODULE,
        REGISTER_CHAIN_ACTION,
        0,
        chain_to_register,
        &[0x77u8; 32],
    );

    let corrupted_registration = Account {
        lamports: 1_000_000,
        data: vec![0u8; RelayerChainRegistrationLayout::LEN - 1],
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    };

    let r = run_register_relayer_chain(
        &mollusk,
        &body,
        chain_to_register,
        Some(corrupted_registration),
        None,
    );
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidPda as u32,
                "expected InvalidPda for a corrupted existing registration PDA, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidPda), got {other:?}"),
    }
}
