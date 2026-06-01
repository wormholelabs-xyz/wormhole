//! Integration tests for `register_chain`.
//!
//! Gated on the paired `(mock-vaa, test-only-open-digest, mock-noreplay)`
//! feature trio so the in-process mollusk runs skip the Verify VAA Shim CPI
//! and substitute a single-byte NoReplay sentinel for the real CPI. Production
//! builds CPI into the Shim against real guardian signatures; the surfpool
//! e2e suite exercises the real-CPI path.
//!
//! Coverage:
//!   - happy path: governance VAA initialises the canonical
//!     `ChainRegistration` PDA with `(chain, emitter_address)`.
//!   - wrong governance emitter rejects.
//!   - wrong governance module rejects.
//!   - wrong action byte rejects.
//!   - wrong target chain rejects.
//!   - replay rejects (NoReplay marks the governance VAA's sequence).
//!   - overwrite via a new governance VAA succeeds (emitter rotation).

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        ChainRegistrationLayout, GlobalAccountantError, Instruction as IxDiscriminator,
        CHAIN_REGISTRATION_SEED_PREFIX, GOVERNANCE_EMITTER, NOREPLAY_AUTHORITY_SEED_PREFIX,
        NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID, REGISTER_CHAIN_ACTION, SOLANA_CHAIN_ID,
        TOKEN_BRIDGE_GOVERNANCE_MODULE,
    },
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk},
    solana_account::Account,
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

const PROGRAM_NAME: &str = "global_accountant";

fn program_id() -> Pubkey {
    Pubkey::new_from_array([7u8; 32])
}

fn mollusk() -> Mollusk {
    Mollusk::new(&program_id(), PROGRAM_NAME)
}

fn system_program_id() -> Pubkey {
    keyed_account_for_system_program().0
}

fn derive_chain_registration_pda(chain: u16) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    Pubkey::find_program_address(
        &[CHAIN_REGISTRATION_SEED_PREFIX, &chain_be],
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
    Account {
        lamports: 1_000_000,
        data: vec![0u8; 1],
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

/// Build a Token Bridge `RegisterChain` governance VAA body. Layout:
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
/// | 84     | 2    | target_chain      |  (0 = Any, 3104 = Wormchain)
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
    // Wire shape: 1-byte discriminator + 1-byte guardian_set_bump + 1-byte
    // registration_bump + 2-byte body length LE + body bytes.
    let mut data = Vec::with_capacity(1 + 1 + 1 + 2 + body.len());
    data.push(IxDiscriminator::RegisterChain as u8);
    data.push(guardian_set_bump);
    data.push(registration_bump);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
    data
}

/// Build the minimal account list for a `register_chain` call. Slot order:
///   0. payer (SIGNER, WRITE)
///   1. Verify VAA Shim program (sentinel under mock-vaa)
///   2. Core Bridge GuardianSet (sentinel under mock-vaa)
///   3. GuardianSignatures (sentinel under mock-vaa)
///   4. chain_registration PDA (WRITE)
///   5. NoReplay bitmap PDA (WRITE)
///   6. NoReplay program (sentinel under mock-noreplay)
///   7. NoReplay authority PDA owned by this program
///   8. system program
fn build_metas(
    payer: Pubkey,
    shim_program: Pubkey,
    guardian_set: Pubkey,
    guardian_signatures: Pubkey,
    registration_pda: Pubkey,
    noreplay_bucket: Pubkey,
    noreplay_program: Pubkey,
    noreplay_authority: Pubkey,
) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(payer, true),
        AccountMeta::new_readonly(shim_program, false),
        AccountMeta::new_readonly(guardian_set, false),
        AccountMeta::new_readonly(guardian_signatures, false),
        AccountMeta::new(registration_pda, false),
        AccountMeta::new(noreplay_bucket, false),
        AccountMeta::new_readonly(noreplay_program, false),
        AccountMeta::new_readonly(noreplay_authority, false),
        AccountMeta::new_readonly(system_program_id(), false),
    ]
}

fn build_initial_accounts(
    payer: Pubkey,
    shim_program: Pubkey,
    guardian_set: Pubkey,
    guardian_signatures: Pubkey,
    registration_pda: Pubkey,
    noreplay_bucket: Pubkey,
    noreplay_program: Pubkey,
    noreplay_authority: Pubkey,
) -> Vec<(Pubkey, Account)> {
    vec![
        (payer, system_owned_account(50_000_000_000)),
        (shim_program, system_owned_account(0)),
        (guardian_set, system_owned_account(0)),
        (guardian_signatures, system_owned_account(0)),
        (registration_pda, uninitialised_pda_account()),
        (noreplay_bucket, noreplay_bucket_unmarked()),
        (noreplay_program, system_owned_account(0)),
        (noreplay_authority, system_owned_account(0)),
        keyed_account_for_system_program(),
    ]
}

#[test]
fn register_chain_via_governance_vaa_initialises_registration_pda() {
    // Happy path: a Token Bridge governance RegisterChain VAA targeting "Any"
    // initialises the canonical ChainRegistration PDA for the supplied chain
    // with the supplied emitter_address. Mirrors CosmWasm
    // `handle_token_governance_vaa` at `contract.rs:370-397`.
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
    let shim_program = Pubkey::new_from_array([0xC1u8; 32]);
    let guardian_set = Pubkey::new_from_array([0xC2u8; 32]);
    let guardian_signatures = Pubkey::new_from_array([0xC3u8; 32]);
    let noreplay_program = Pubkey::new_from_array([0xC5u8; 32]);
    // Canonical program-derived noreplay authority. Matches what
    // close_pending re-derives internally and what the production CPI signs.
    let (noreplay_authority, _) =
        Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
    let noreplay_bucket = derive_canonical_noreplay_bucket(
        &noreplay_authority,
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x01, // vaa_sequence used in this body
    );

    let accounts = build_initial_accounts(
        payer,
        shim_program,
        guardian_set,
        guardian_signatures,
        registration_pda,
        noreplay_bucket,
        noreplay_program,
        noreplay_authority,
    );
    let metas = build_metas(
        payer,
        shim_program,
        guardian_set,
        guardian_signatures,
        registration_pda,
        noreplay_bucket,
        noreplay_program,
        noreplay_authority,
    );

    let ix = Instruction::new_with_bytes(
        program_id(),
        &register_chain_ix_data(/* guardian_set_bump */ 0, registration_bump, &body),
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
    assert_eq!(post.1.owner, program_id(), "registration PDA owned by program");
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

/// Single-call test driver. Builds the standard fixture set, optionally
/// overrides the initial registration PDA account (used by the overwrite +
/// replay tests), runs the instruction, and returns the result.
fn run_register_chain(
    mollusk: &Mollusk,
    body: &[u8],
    chain_to_register: u16,
    initial_registration: Option<Account>,
    initial_noreplay_bucket: Option<Account>,
) -> mollusk_svm::result::InstructionResult {
    let (registration_pda, registration_bump) = derive_chain_registration_pda(chain_to_register);
    let payer = Pubkey::new_from_array([0x11u8; 32]);
    let shim_program = Pubkey::new_from_array([0xC1u8; 32]);
    let guardian_set = Pubkey::new_from_array([0xC2u8; 32]);
    let guardian_signatures = Pubkey::new_from_array([0xC3u8; 32]);
    let noreplay_program = Pubkey::new_from_array([0xC5u8; 32]);
    // Canonical program-derived noreplay authority. Matches what
    // close_pending re-derives internally and what the production CPI signs.
    let (noreplay_authority, _) =
        Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
    // The body's vaa_sequence determines which canonical noreplay bucket the
    // program will derive. Read it directly from body[42..50] so the test
    // helper doesn't have to be told twice.
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

    let mut accounts = build_initial_accounts(
        payer,
        shim_program,
        guardian_set,
        guardian_signatures,
        registration_pda,
        noreplay_bucket,
        noreplay_program,
        noreplay_authority,
    );
    if let Some(existing) = initial_registration {
        for entry in accounts.iter_mut() {
            if entry.0 == registration_pda {
                entry.1 = existing;
                break;
            }
        }
    }
    if let Some(bucket) = initial_noreplay_bucket {
        for entry in accounts.iter_mut() {
            if entry.0 == noreplay_bucket {
                entry.1 = bucket;
                break;
            }
        }
    }
    let metas = build_metas(
        payer,
        shim_program,
        guardian_set,
        guardian_signatures,
        registration_pda,
        noreplay_bucket,
        noreplay_program,
        noreplay_authority,
    );

    let ix = Instruction::new_with_bytes(
        program_id(),
        &register_chain_ix_data(0, registration_bump, body),
        metas,
    );
    mollusk.process_instruction(&ix, &accounts)
}

#[test]
fn register_chain_rejects_wrong_governance_emitter() {
    // Body claims a non-governance emitter — should refuse with
    // InvalidGovernanceEmitter. Even if the Shim CPI signed off (mock under
    // mock-vaa), the body-header check rejects.
    let mollusk = mollusk();
    let body = build_register_chain_body(
        SOLANA_CHAIN_ID,
        &[0xDEu8; 32], // non-governance emitter
        0x02,
        &TOKEN_BRIDGE_GOVERNANCE_MODULE,
        REGISTER_CHAIN_ACTION,
        0,
        2,
        &[0x77u8; 32],
    );
    let r = run_register_chain(&mollusk, &body, 2, None, None);
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

#[test]
fn register_chain_rejects_wrong_module() {
    // Body's payload module bytes do not match TOKEN_BRIDGE_GOVERNANCE_MODULE.
    let mollusk = mollusk();
    let wrong_module = [0xAAu8; 32];
    let body = build_register_chain_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x03,
        &wrong_module,
        REGISTER_CHAIN_ACTION,
        0,
        2,
        &[0x77u8; 32],
    );
    let r = run_register_chain(&mollusk, &body, 2, None, None);
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

#[test]
fn register_chain_rejects_wrong_action() {
    // Action byte is not REGISTER_CHAIN_ACTION (0x01). Other actions
    // (UpgradeContract, etc.) require their own dispatch.
    let mollusk = mollusk();
    let body = build_register_chain_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x04,
        &TOKEN_BRIDGE_GOVERNANCE_MODULE,
        0x02, // wrong action
        0,
        2,
        &[0x77u8; 32],
    );
    let r = run_register_chain(&mollusk, &body, 2, None, None);
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

#[test]
fn register_chain_rejects_wrong_target_chain() {
    // Target chain is neither 0 (Any) nor Wormchain (3104). Matches CosmWasm
    // `contract.rs:374-377`.
    let mollusk = mollusk();
    let body = build_register_chain_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x05,
        &TOKEN_BRIDGE_GOVERNANCE_MODULE,
        REGISTER_CHAIN_ACTION,
        99, // neither Any nor Wormchain
        2,
        &[0x77u8; 32],
    );
    let r = run_register_chain(&mollusk, &body, 2, None, None);
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

#[test]
fn register_chain_rejects_replay() {
    // Submit the same governance VAA twice. The first succeeds, the second
    // rejects via the NoReplay pre-check at (1, GOVERNANCE_EMITTER, sequence).
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

    // First call — happy path success.
    let r1 = run_register_chain(&mollusk, &body, 2, None, None);
    assert!(
        matches!(r1.program_result, ProgramResult::Success),
        "first registration must succeed, got {:?}",
        r1.program_result
    );

    // Carry the NoReplay-flipped bucket and the freshly-initialised
    // registration PDA into the second call.
    let (registration_pda, _) = derive_chain_registration_pda(2);
    // Authority pubkey must match what `run_register_chain` uses for the
    // canonical bucket derivation.
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

#[test]
fn register_chain_overwrite_via_new_governance_vaa_succeeds() {
    // Emitter rotation: register chain 2 -> emitter A via VAA at sequence 1,
    // then register chain 2 -> emitter B via VAA at sequence 2. The
    // registration PDA must end up holding emitter B (the latest
    // governance VAA wins).
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

    // Carry the freshly-initialised PDA + flipped bucket into the second call.
    let (registration_pda, _) = derive_chain_registration_pda(2);
    // Authority pubkey must match what `run_register_chain` uses for the
    // canonical bucket derivation.
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

    // Second governance VAA uses a sequence in a different NoReplay bucket
    // (sequence / 1024 differs). Under real noreplay each bucket has its own
    // PDA address; under `mock-noreplay` the `is_marked` short-circuit checks
    // only `bucket[0] != 0`, so the test must hand the program a *fresh*
    // unmarked bucket fixture to stand in for the new bucket address. The
    // production path would have routed the second call to a different
    // bucket PDA naturally.
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
