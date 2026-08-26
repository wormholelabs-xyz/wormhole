//! Mollusk integration tests for `register_chain`, with the real `solana_noreplay.so` and
//! `wormhole_verify_vaa_shim.so` (see `common::mollusk_fixtures`).

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        ChainRegistrationLayout, GlobalAccountantError, Instruction as IxDiscriminator,
        CHAIN_REGISTRATION_SEED_PREFIX, CORE_BRIDGE_PROGRAM_ID, GOVERNANCE_EMITTER,
        NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID,
        REGISTER_CHAIN_ACTION, SOLANA_CHAIN_ID, TOKEN_BRIDGE_GOVERNANCE_MODULE,
        VERIFY_VAA_SHIM_PROGRAM_ID,
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

const PROGRAM_NAME: &str = "global_accountant";
const GUARDIAN_COUNT: usize = 19;
const QUORUM: u8 = 13;
const GUARDIAN_SET_INDEX: u32 = 4;

fn program_id() -> Pubkey {
    Pubkey::new_from_array([7u8; 32])
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

/// Dedup digest `keccak256(keccak256(body))`.
fn double_keccak256_host(body: &[u8]) -> [u8; 32] {
    let inner = solana_keccak_hasher::hashv(&[body]).to_bytes();
    solana_keccak_hasher::hashv(&[&inner]).to_bytes()
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
    // Uninitialised.
    system_owned_account(0)
}

/// Token Bridge `RegisterChain` governance VAA body:
///
/// | offset | size | field             |
/// |--------|------|-------------------|
/// | 0      | 4    | timestamp         |
/// | 4      | 4    | nonce             |
/// | 8      | 2    | emitter_chain     |  (must be SOLANA_CHAIN_ID)
/// | 10     | 32   | emitter_address   |  (must be GOVERNANCE_EMITTER)
/// | 42     | 8    | sequence          |
/// | 50     | 1    | consistency_level |
/// | 51     | 32   | module            |  (TOKEN_BRIDGE_GOVERNANCE_MODULE)
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
    // Wire: discriminator + guardian_set_bump + registration_bump + body_len(u16 LE) + body.
    let mut data = Vec::with_capacity(1 + 1 + 1 + 2 + body.len());
    data.push(IxDiscriminator::RegisterChain as u8);
    data.push(guardian_set_bump);
    data.push(registration_bump);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
    data
}

/// Account list for `register_chain`:
///   0. payer (SIGNER, WRITE)
///   1. Verify VAA Shim program
///   2. Core Bridge GuardianSet
///   3. GuardianSignatures
///   4. chain_registration PDA (WRITE)
///   5. NoReplay bitmap PDA (WRITE)
///   6. NoReplay program
///   7. NoReplay authority PDA
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

/// `RegisterChain` with target chain Any initialises the `ChainRegistration` PDA.
#[test]
fn register_chain_via_governance_vaa_initialises_registration_pda() {
    let mollusk = mollusk();
    let chain_to_register: u16 = 2;
    let emitter_to_register = [0x77u8; 32];

    let body = build_register_chain_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x01,
        &TOKEN_BRIDGE_GOVERNANCE_MODULE,
        REGISTER_CHAIN_ACTION,
        0, // target_chain = Any
        chain_to_register,
        &emitter_to_register,
    );

    let (registration_pda, registration_bump) = derive_chain_registration_pda(chain_to_register);
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
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x01, // vaa_sequence
    );

    let accounts = build_initial_accounts(
        payer,
        &digest,
        guardian_set,
        guardian_signatures,
        &guardians,
        registration_pda,
        uninitialised_pda_account(),
        noreplay_bucket,
        noreplay_bucket_unmarked(),
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
        &register_chain_ix_data(guardian_set_bump, registration_bump, &body),
        metas,
    );
    let r = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(r.program_result, ProgramResult::Success),
        "register_chain happy-path must succeed, got {:?}",
        r.program_result
    );

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
        ChainRegistrationLayout::LEN,
        "registration PDA allocated to full layout length"
    );
    let layout: &ChainRegistrationLayout = bytemuck::from_bytes(&post.1.data);
    assert_eq!(layout.chain, chain_to_register, "chain field persisted");
    assert_eq!(
        layout.emitter_address, emitter_to_register,
        "emitter_address field persisted"
    );
}

/// Single-call driver with optional registration and bucket overrides.
fn run_register_chain(
    mollusk: &Mollusk,
    body: &[u8],
    chain_to_register: u16,
    initial_registration: Option<Account>,
    initial_noreplay_bucket: Option<Account>,
) -> mollusk_svm::result::InstructionResult {
    let (registration_pda, registration_bump) = derive_chain_registration_pda(chain_to_register);
    let payer = Pubkey::new_from_array([0x11u8; 32]);
    let guardian_signatures = Pubkey::new_from_array([0xC3u8; 32]);
    let (guardian_set, guardian_set_bump) =
        derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
    let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
    let digest = double_keccak256_host(body);
    let (noreplay_authority, _) =
        Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
    // `vaa_sequence` (body[42..50]) selects the NoReplay bucket.
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

/// Each case mutates one body field. Signatures are re-signed over the mutated body, so
/// the Shim passes and the rejection comes from the governance-header validation.
#[test]
fn register_chain_governance_header_violations_reject() {
    struct Case {
        label: &'static str,
        emitter: [u8; 32],
        module: [u8; 32],
        action: u8,
        target_chain: u16,
        sequence: u64,
        expected: u32,
    }
    let canonical = Case {
        label: "(canonical baseline — not run)",
        emitter: GOVERNANCE_EMITTER,
        module: TOKEN_BRIDGE_GOVERNANCE_MODULE,
        action: REGISTER_CHAIN_ACTION,
        target_chain: 0,
        sequence: 0,
        expected: 0,
    };
    let cases = [
        Case {
            label: "non-governance emitter",
            emitter: [0xDEu8; 32],
            sequence: 0x02,
            expected: GlobalAccountantError::InvalidGovernanceEmitter as u32,
            ..canonical
        },
        Case {
            label: "wrong governance module",
            module: [0xAAu8; 32],
            sequence: 0x03,
            expected: GlobalAccountantError::InvalidGovernanceModule as u32,
            ..canonical
        },
        Case {
            label: "wrong action byte",
            action: 0x02,
            sequence: 0x04,
            expected: GlobalAccountantError::InvalidGovernanceAction as u32,
            ..canonical
        },
        Case {
            label: "target_chain neither Any nor Solana",
            target_chain: 99,
            sequence: 0x05,
            expected: GlobalAccountantError::GovernanceChainMismatch as u32,
            ..canonical
        },
    ];

    let mollusk = mollusk();
    for case in cases {
        let body = build_register_chain_body(
            SOLANA_CHAIN_ID,
            &case.emitter,
            case.sequence,
            &case.module,
            case.action,
            case.target_chain,
            2,
            &[0x77u8; 32],
        );
        let r = run_register_chain(&mollusk, &body, 2, None, None);
        match r.program_result {
            ProgramResult::Failure(err) => {
                let code = u64::from(err) as u32;
                assert_eq!(
                    code,
                    case.expected,
                    "[{label}] expected {expected:?}, got {code:?}",
                    label = case.label,
                    expected = case.expected,
                );
            }
            other => panic!("[{}] expected Failure, got {other:?}", case.label),
        }
    }
}

/// Same governance VAA twice: `AlreadyAccounted`.
#[test]
fn register_chain_rejects_replay() {
    let mollusk = mollusk();
    let body = build_register_chain_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x06,
        &TOKEN_BRIDGE_GOVERNANCE_MODULE,
        REGISTER_CHAIN_ACTION,
        0,
        2,
        &[0x77u8; 32],
    );

    let r1 = run_register_chain(&mollusk, &body, 2, None, None);
    assert!(
        matches!(r1.program_result, ProgramResult::Success),
        "first registration must succeed, got {:?}",
        r1.program_result
    );

    let (registration_pda, _) = derive_chain_registration_pda(2);
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
                0x06, // vaa_sequence from this test's body
            )
        })
        .map(|(_, a)| a.clone())
        .expect("noreplay bucket missing from first result");

    let r2 = run_register_chain(
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

/// A later governance VAA overwrites the registration with the new emitter.
#[test]
fn register_chain_overwrite_via_new_governance_vaa_succeeds() {
    let mollusk = mollusk();
    let emitter_a = [0x77u8; 32];
    let emitter_b = [0xBBu8; 32];

    let body_a = build_register_chain_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x07,
        &TOKEN_BRIDGE_GOVERNANCE_MODULE,
        REGISTER_CHAIN_ACTION,
        0,
        2,
        &emitter_a,
    );
    let r1 = run_register_chain(&mollusk, &body_a, 2, None, None);
    assert!(matches!(r1.program_result, ProgramResult::Success));

    let (registration_pda, _) = derive_chain_registration_pda(2);
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
                0x07, // body_a's vaa_sequence
            )
        })
        .map(|(_, a)| a.clone())
        .expect("noreplay bucket missing from first result");

    // The second sequence is in another bucket (`sequence / 1024` differs), so its PDA is unmarked.
    let _ = post_bucket;
    let fresh_bucket = noreplay_bucket_unmarked();
    let body_b = build_register_chain_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x407, // 0x07 + 1024 ⇒ different bucket under real noreplay
        &TOKEN_BRIDGE_GOVERNANCE_MODULE,
        REGISTER_CHAIN_ACTION,
        0,
        2,
        &emitter_b,
    );
    let r2 = run_register_chain(
        &mollusk,
        &body_b,
        2,
        Some(post_registration),
        Some(fresh_bucket),
    );
    assert!(
        matches!(r2.program_result, ProgramResult::Success),
        "overwrite with new governance VAA must succeed, got {:?}",
        r2.program_result
    );

    let post = r2
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == registration_pda)
        .expect("registration PDA missing from second result");
    let layout: &ChainRegistrationLayout = bytemuck::from_bytes(&post.1.data);
    assert_eq!(
        layout.emitter_address, emitter_b,
        "registration overwritten to new emitter"
    );
}

/// Register emitter_A at sequence N, rotate to emitter_B, then resubmit the emitter_A VAA:
/// `AlreadyAccounted`. The bucket keys on the governance emitter and VAA sequence.
#[test]
fn register_chain_rotation_then_replay_of_old_vaa_rejects() {
    let mollusk = mollusk();
    let emitter_a = [0x77u8; 32];
    let emitter_b = [0xBBu8; 32];
    let seq_n: u64 = 0x08;
    let seq_rotate: u64 = seq_n + 1024; // different noreplay bucket

    let (noreplay_authority_pubkey, _) =
        Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
    let (registration_pda, _) = derive_chain_registration_pda(2);
    let bucket_n = derive_canonical_noreplay_bucket(
        &noreplay_authority_pubkey,
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        seq_n,
    );

    // 1) emitter_A at sequence N.
    let body_a = build_register_chain_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        seq_n,
        &TOKEN_BRIDGE_GOVERNANCE_MODULE,
        REGISTER_CHAIN_ACTION,
        0,
        2,
        &emitter_a,
    );
    let r1 = run_register_chain(&mollusk, &body_a, 2, None, None);
    assert!(matches!(r1.program_result, ProgramResult::Success));
    let post_registration_a = r1
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == registration_pda)
        .map(|(_, a)| a.clone())
        .expect("registration PDA missing after first register");
    let marked_bucket_n = r1
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == bucket_n)
        .map(|(_, a)| a.clone())
        .expect("seq-N bucket missing after first register");
    assert_eq!(
        marked_bucket_n.owner,
        Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        "seq-N bucket flipped to noreplay ownership by the first register"
    );

    // 2) emitter_B at a higher sequence.
    let body_b = build_register_chain_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        seq_rotate,
        &TOKEN_BRIDGE_GOVERNANCE_MODULE,
        REGISTER_CHAIN_ACTION,
        0,
        2,
        &emitter_b,
    );
    let r2 = run_register_chain(
        &mollusk,
        &body_b,
        2,
        Some(post_registration_a),
        Some(noreplay_bucket_unmarked()),
    );
    assert!(
        matches!(r2.program_result, ProgramResult::Success),
        "rotation to emitter_B must succeed, got {:?}",
        r2.program_result
    );
    let post_registration_b = r2
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == registration_pda)
        .map(|(_, a)| a.clone())
        .expect("registration PDA missing after rotation");

    // 3) Replay the emitter_A VAA with the marked bucket.
    let r3 = run_register_chain(
        &mollusk,
        &body_a,
        2,
        Some(post_registration_b),
        Some(marked_bucket_n),
    );
    match r3.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::AlreadyAccounted as u32,
                "replay of the original seq-N VAA must reject AlreadyAccounted, got {code:?}"
            );
        }
        other => panic!("expected Failure(AlreadyAccounted), got {other:?}"),
    }
}
