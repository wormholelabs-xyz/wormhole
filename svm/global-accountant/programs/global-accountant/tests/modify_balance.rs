//! Integration tests for `modify_balance`.
//!
//! Gated on the paired `(mock-vaa, test-only-open-digest, mock-noreplay)`
//! feature trio so the in-process mollusk runs skip the Verify VAA Shim CPI.
//! Production builds CPI into the Shim against real guardian signatures; the
//! surfpool e2e suite is the canonical real-CPI guard.
//!
//! Coverage:
//!   - happy path Add on uninit BalanceAccount: lazy-inits + credits.
//!   - happy path Add on existing BalanceAccount: credits without re-init.
//!   - happy path Sub on existing BalanceAccount with sufficient balance: debits.
//!   - rejection: wrong governance emitter / module / action / target chain /
//!     kind byte / balance_pda_bump.
//!   - rejection: Sub on uninit BalanceAccount underflows before allocation.
//!   - rejection: Add overflow against a near-max balance.
//!   - rejection: duplicate modification sequence (PDA already exists).

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        BalanceAccountLayout, GlobalAccountantError, Instruction as IxDiscriminator,
        ModificationLogLayout, Uint256, ACCOUNT_SEED_PREFIX, ACCOUNTANT_GOVERNANCE_MODULE,
        GOVERNANCE_EMITTER, MODIFICATION_SEED_PREFIX, MODIFY_BALANCE_ACTION, SOLANA_CHAIN_ID,
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

fn derive_balance_pda(chain: u16, token_chain: u16, token_address: &[u8; 32]) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    let token_chain_be = token_chain.to_be_bytes();
    Pubkey::find_program_address(
        &[ACCOUNT_SEED_PREFIX, &chain_be, &token_chain_be, token_address],
        &program_id(),
    )
}

fn derive_modification_pda(sequence: u64) -> (Pubkey, u8) {
    let seq_be = sequence.to_be_bytes();
    Pubkey::find_program_address(&[MODIFICATION_SEED_PREFIX, &seq_be], &program_id())
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

/// Build a `BalanceAccount` PDA fixture pre-funded with `balance`. Used by
/// the Sub-on-existing-balance and overflow-edge tests.
fn balance_account(
    chain: u16,
    token_chain: u16,
    token_address: &[u8; 32],
    balance: Uint256,
) -> Account {
    let mut layout: BalanceAccountLayout = bytemuck::Zeroable::zeroed();
    layout.chain = chain;
    layout.token_chain = token_chain;
    layout.token_address = *token_address;
    layout.balance = balance;
    Account {
        lamports: 1_000_000,
        data: bytemuck::bytes_of(&layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

/// Build a `ModifyBalance` governance VAA body. Layout (after the 51-byte
/// VAA header):
///
/// | offset | size | field             |
/// |--------|------|-------------------|
/// | 51     | 32   | module            |
/// | 83     | 1    | action            |
/// | 84     | 2    | target_chain      |
/// | 86     | 8    | sequence (payload)|
/// | 94     | 2    | chain_id          |
/// | 96     | 2    | token_chain       |
/// | 98     | 32   | token_address     |
/// | 130    | 1    | kind              |
/// | 131    | 32   | amount (BE u256)  |
/// | 163    | 32   | reason            |
///
/// Total body: 195 bytes.
fn build_modify_balance_body(
    governance_emitter_chain: u16,
    governance_emitter: &[u8; 32],
    vaa_sequence: u64,
    module: &[u8; 32],
    action: u8,
    target_chain: u16,
    payload_sequence: u64,
    chain_id: u16,
    token_chain: u16,
    token_address: &[u8; 32],
    kind: u8,
    amount: Uint256,
    reason: &[u8; 32],
) -> Vec<u8> {
    let mut body = vec![0u8; 195];
    body[8..10].copy_from_slice(&governance_emitter_chain.to_be_bytes());
    body[10..42].copy_from_slice(governance_emitter);
    body[42..50].copy_from_slice(&vaa_sequence.to_be_bytes());
    body[51..83].copy_from_slice(module);
    body[83] = action;
    body[84..86].copy_from_slice(&target_chain.to_be_bytes());
    body[86..94].copy_from_slice(&payload_sequence.to_be_bytes());
    body[94..96].copy_from_slice(&chain_id.to_be_bytes());
    body[96..98].copy_from_slice(&token_chain.to_be_bytes());
    body[98..130].copy_from_slice(token_address);
    body[130] = kind;
    body[131..163].copy_from_slice(&amount.0);
    body[163..195].copy_from_slice(reason);
    body
}

fn modify_balance_ix_data(
    guardian_set_bump: u8,
    balance_pda_bump: u8,
    modification_bump: u8,
    body: &[u8],
) -> Vec<u8> {
    // Wire shape: 1-byte discriminator + 1-byte guardian_set_bump + 1-byte
    // balance_pda_bump + 1-byte modification_bump + 2-byte body length LE +
    // body bytes.
    let mut data = Vec::with_capacity(1 + 1 + 1 + 1 + 2 + body.len());
    data.push(IxDiscriminator::ModifyBalance as u8);
    data.push(guardian_set_bump);
    data.push(balance_pda_bump);
    data.push(modification_bump);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
    data
}

/// Account fixtures + meta vec for the `modify_balance` ix. Slot order:
///   0. payer (SIGNER, WRITE)
///   1. Verify VAA Shim program (sentinel under mock-vaa)
///   2. Core Bridge GuardianSet (sentinel under mock-vaa)
///   3. GuardianSignatures (sentinel under mock-vaa)
///   4. BalanceAccount PDA (WRITE)
///   5. system program
///   6. ModificationLog PDA (WRITE)
fn build_metas(
    payer: Pubkey,
    shim_program: Pubkey,
    guardian_set: Pubkey,
    guardian_signatures: Pubkey,
    balance_pda: Pubkey,
    modification_pda: Pubkey,
) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(payer, true),
        AccountMeta::new_readonly(shim_program, false),
        AccountMeta::new_readonly(guardian_set, false),
        AccountMeta::new_readonly(guardian_signatures, false),
        AccountMeta::new(balance_pda, false),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new(modification_pda, false),
    ]
}

fn build_initial_accounts(
    payer: Pubkey,
    shim_program: Pubkey,
    guardian_set: Pubkey,
    guardian_signatures: Pubkey,
    balance_pda: Pubkey,
    balance_pda_state: Account,
    modification_pda: Pubkey,
    modification_pda_state: Account,
) -> Vec<(Pubkey, Account)> {
    vec![
        (payer, system_owned_account(50_000_000_000)),
        (shim_program, system_owned_account(0)),
        (guardian_set, system_owned_account(0)),
        (guardian_signatures, system_owned_account(0)),
        (balance_pda, balance_pda_state),
        keyed_account_for_system_program(),
        (modification_pda, modification_pda_state),
    ]
}

/// Single driver covering the standard fixture set. Negative tests override
/// individual fields via the closure-free arguments to keep the call sites
/// readable.
#[allow(clippy::too_many_arguments)]
fn run_modify_balance(
    mollusk: &Mollusk,
    body: &[u8],
    chain_id: u16,
    token_chain: u16,
    token_address: &[u8; 32],
    payload_sequence: u64,
    balance_pda_bump_override: Option<u8>,
    balance_initial: Option<Account>,
    modification_initial: Option<Account>,
) -> mollusk_svm::result::InstructionResult {
    let (balance_pda, canonical_balance_bump) =
        derive_balance_pda(chain_id, token_chain, token_address);
    let (modification_pda, modification_bump) = derive_modification_pda(payload_sequence);

    let payer = Pubkey::new_from_array([0x11u8; 32]);
    let shim_program = Pubkey::new_from_array([0xC1u8; 32]);
    let guardian_set = Pubkey::new_from_array([0xC2u8; 32]);
    let guardian_signatures = Pubkey::new_from_array([0xC3u8; 32]);

    let balance_pda_bump = balance_pda_bump_override.unwrap_or(canonical_balance_bump);

    let accounts = build_initial_accounts(
        payer,
        shim_program,
        guardian_set,
        guardian_signatures,
        balance_pda,
        balance_initial.unwrap_or_else(uninitialised_pda_account),
        modification_pda,
        modification_initial.unwrap_or_else(uninitialised_pda_account),
    );
    let metas = build_metas(
        payer,
        shim_program,
        guardian_set,
        guardian_signatures,
        balance_pda,
        modification_pda,
    );

    let ix = Instruction::new_with_bytes(
        program_id(),
        &modify_balance_ix_data(
            /* guardian_set_bump */ 0,
            balance_pda_bump,
            modification_bump,
            body,
        ),
        metas,
    );
    mollusk.process_instruction(&ix, &accounts)
}

// ============================================================================
// Rejection tests (red first — pre-impl these fail to compile until
// `Instruction::ModifyBalance` dispatches to a real handler).
// ============================================================================

#[test]
fn modify_balance_rejects_wrong_governance_emitter() {
    // Body header carries a non-governance emitter. Even with the Shim mock
    // CPI accepting any digest, the body-header check rejects.
    let mollusk = mollusk();
    let token_address = [0x77u8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &[0xDEu8; 32], // non-governance emitter
        0x01,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        3104, // Wormchain
        100,
        2,
        2,
        &token_address,
        1, // Add
        Uint256::from_u128(1_000),
        &[0u8; 32],
    );
    let r = run_modify_balance(&mollusk, &body, 2, 2, &token_address, 100, None, None, None);
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
fn modify_balance_rejects_wrong_module() {
    let mollusk = mollusk();
    let token_address = [0x77u8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x02,
        &[0xAAu8; 32], // wrong module
        MODIFY_BALANCE_ACTION,
        3104,
        101,
        2,
        2,
        &token_address,
        1,
        Uint256::from_u128(1_000),
        &[0u8; 32],
    );
    let r = run_modify_balance(&mollusk, &body, 2, 2, &token_address, 101, None, None, None);
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
fn modify_balance_rejects_wrong_action() {
    let mollusk = mollusk();
    let token_address = [0x77u8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x03,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        0x02, // wrong action
        3104,
        102,
        2,
        2,
        &token_address,
        1,
        Uint256::from_u128(1_000),
        &[0u8; 32],
    );
    let r = run_modify_balance(&mollusk, &body, 2, 2, &token_address, 102, None, None, None);
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
fn modify_balance_rejects_wrong_target_chain() {
    // CosmWasm `handle_accountant_governance_vaa` accepts *only* Wormchain
    // (not Any). `Any (0)` here must reject.
    let mollusk = mollusk();
    let token_address = [0x77u8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x04,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        0, // Any — rejected on this path
        103,
        2,
        2,
        &token_address,
        1,
        Uint256::from_u128(1_000),
        &[0u8; 32],
    );
    let r = run_modify_balance(&mollusk, &body, 2, 2, &token_address, 103, None, None, None);
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
fn modify_balance_rejects_invalid_kind() {
    let mollusk = mollusk();
    let token_address = [0x77u8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x05,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        3104,
        104,
        2,
        2,
        &token_address,
        0, // invalid kind byte (Unknown(0) in SDK)
        Uint256::from_u128(1_000),
        &[0u8; 32],
    );
    let r = run_modify_balance(&mollusk, &body, 2, 2, &token_address, 104, None, None, None);
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidModificationKind as u32,
                "expected InvalidModificationKind, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidModificationKind), got {other:?}"),
    }
}

#[test]
fn modify_balance_add_on_uninit_pda_initialises_and_credits() {
    // Happy path: governance Add on a fresh (chain, token_chain, token_address)
    // triple lazy-inits the BalanceAccount PDA, stamps the layout, and writes
    // `balance = amount`. ModificationLog PDA is created alongside.
    let mollusk = mollusk();
    let token_address = [0x77u8; 32];
    let reason = *b"audit-log: post-incident credit ";
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x10,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        3104,
        200,
        2,
        2,
        &token_address,
        1,
        Uint256::from_u128(1_000_000),
        &reason,
    );
    let r = run_modify_balance(&mollusk, &body, 2, 2, &token_address, 200, None, None, None);
    assert!(
        matches!(r.program_result, ProgramResult::Success),
        "happy-path Add on uninit must succeed, got {:?}",
        r.program_result
    );

    let (balance_pda, _) = derive_balance_pda(2, 2, &token_address);
    let post_balance = r
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == balance_pda)
        .expect("balance PDA missing from result");
    assert_eq!(post_balance.1.owner, program_id(), "balance PDA owned by program");
    let layout: &BalanceAccountLayout = bytemuck::from_bytes(&post_balance.1.data);
    assert_eq!(layout.chain, 2);
    assert_eq!(layout.token_chain, 2);
    assert_eq!(layout.token_address, token_address);
    assert_eq!(layout.balance, Uint256::from_u128(1_000_000));

    let (modification_pda, _) = derive_modification_pda(200);
    let post_log = r
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == modification_pda)
        .expect("modification PDA missing from result");
    assert_eq!(post_log.1.owner, program_id(), "modification PDA owned by program");
    let log: &ModificationLogLayout = bytemuck::from_bytes(&post_log.1.data);
    assert_eq!(log.sequence, 200);
    assert_eq!(log.chain_id, 2);
    assert_eq!(log.token_chain, 2);
    assert_eq!(log.kind, 1); // Add
    assert_eq!(log.amount, Uint256::from_u128(1_000_000));
    assert_eq!(log.reason, reason, "reason persisted to the modification log");
}

#[test]
fn modify_balance_sub_on_existing_pda_debits() {
    // Happy path: Sub against a pre-funded BalanceAccount. balance 5000,
    // Sub 1500 → balance 3500.
    let mollusk = mollusk();
    let token_address = [0x88u8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x11,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        3104,
        201,
        2,
        2,
        &token_address,
        2, // Subtract
        Uint256::from_u128(1_500),
        &[0u8; 32],
    );
    let pre_balance = balance_account(2, 2, &token_address, Uint256::from_u128(5_000));
    let r = run_modify_balance(
        &mollusk,
        &body,
        2,
        2,
        &token_address,
        201,
        None,
        Some(pre_balance),
        None,
    );
    assert!(
        matches!(r.program_result, ProgramResult::Success),
        "happy-path Sub on existing must succeed, got {:?}",
        r.program_result
    );

    let (balance_pda, _) = derive_balance_pda(2, 2, &token_address);
    let post = r
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == balance_pda)
        .expect("balance PDA missing from result");
    let layout: &BalanceAccountLayout = bytemuck::from_bytes(&post.1.data);
    assert_eq!(layout.balance, Uint256::from_u128(3_500), "5000 - 1500 = 3500");
}

#[test]
fn modify_balance_add_on_existing_pda_credits() {
    // Happy path: Add against an existing BalanceAccount. balance 100,
    // Add 50 → balance 150.
    let mollusk = mollusk();
    let token_address = [0x99u8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x12,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        3104,
        202,
        2,
        2,
        &token_address,
        1, // Add
        Uint256::from_u128(50),
        &[0u8; 32],
    );
    let pre_balance = balance_account(2, 2, &token_address, Uint256::from_u128(100));
    let r = run_modify_balance(
        &mollusk,
        &body,
        2,
        2,
        &token_address,
        202,
        None,
        Some(pre_balance),
        None,
    );
    assert!(matches!(r.program_result, ProgramResult::Success));

    let (balance_pda, _) = derive_balance_pda(2, 2, &token_address);
    let post = r
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == balance_pda)
        .expect("balance PDA missing from result");
    let layout: &BalanceAccountLayout = bytemuck::from_bytes(&post.1.data);
    assert_eq!(layout.balance, Uint256::from_u128(150));
}

#[test]
fn modify_balance_sub_on_uninit_pda_rejects_underflow() {
    // Sub on an uninitialised BalanceAccount underflows from zero balance.
    // Rejected BEFORE allocation so the payer doesn't pay rent on a guaranteed-
    // failed mutation.
    let mollusk = mollusk();
    let token_address = [0xAAu8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x13,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        3104,
        203,
        2,
        2,
        &token_address,
        2, // Subtract
        Uint256::from_u128(1),
        &[0u8; 32],
    );
    let r = run_modify_balance(&mollusk, &body, 2, 2, &token_address, 203, None, None, None);
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::ModifyBalanceUnderflow as u32,
                "expected ModifyBalanceUnderflow, got {code:?}"
            );
        }
        other => panic!("expected Failure(ModifyBalanceUnderflow), got {other:?}"),
    }

    // BalanceAccount and ModificationLog must remain uninitialised after the
    // pre-allocation rejection.
    let (balance_pda, _) = derive_balance_pda(2, 2, &token_address);
    let post_balance = r
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == balance_pda)
        .expect("balance PDA missing from result");
    assert_eq!(post_balance.1.owner, system_program_id());
    assert!(post_balance.1.data.is_empty());
}

#[test]
fn modify_balance_add_overflow_rejects() {
    // Add against a near-MAX balance overflows the Uint256. Rejected with
    // ModifyBalanceOverflow.
    let mollusk = mollusk();
    let token_address = [0xBBu8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x14,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        3104,
        204,
        2,
        2,
        &token_address,
        1, // Add
        Uint256::from_u128(2),
        &[0u8; 32],
    );
    // Pre-fund with Uint256::MAX - 1 so Add 2 overflows.
    let mut max_minus_one_bytes = [0xFFu8; 32];
    max_minus_one_bytes[31] = 0xFE;
    let pre_balance = balance_account(2, 2, &token_address, Uint256(max_minus_one_bytes));
    let r = run_modify_balance(
        &mollusk,
        &body,
        2,
        2,
        &token_address,
        204,
        None,
        Some(pre_balance),
        None,
    );
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::ModifyBalanceOverflow as u32,
                "expected ModifyBalanceOverflow, got {code:?}"
            );
        }
        other => panic!("expected Failure(ModifyBalanceOverflow), got {other:?}"),
    }
}

#[test]
fn modify_balance_rejects_duplicate_modification_sequence() {
    // Replay protection: a second governance VAA with the same payload
    // sequence collides on the ModificationLog PDA address. Mirrors CosmWasm
    // `MODIFICATIONS.has` short-circuit at
    // `packages/accountant/src/contract.rs:248-250`.
    let mollusk = mollusk();
    let token_address = [0xCCu8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x15,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        3104,
        205,
        2,
        2,
        &token_address,
        1,
        Uint256::from_u128(100),
        &[0u8; 32],
    );

    // First call lands a fresh modification.
    let r1 = run_modify_balance(&mollusk, &body, 2, 2, &token_address, 205, None, None, None);
    assert!(matches!(r1.program_result, ProgramResult::Success));

    // Carry the post-state forward and replay.
    let (balance_pda, _) = derive_balance_pda(2, 2, &token_address);
    let (modification_pda, _) = derive_modification_pda(205);
    let post_balance = r1
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == balance_pda)
        .map(|(_, a)| a.clone())
        .expect("balance PDA missing");
    let post_modification = r1
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == modification_pda)
        .map(|(_, a)| a.clone())
        .expect("modification PDA missing");

    let r2 = run_modify_balance(
        &mollusk,
        &body,
        2,
        2,
        &token_address,
        205,
        None,
        Some(post_balance),
        Some(post_modification),
    );
    match r2.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::DuplicateModification as u32,
                "expected DuplicateModification on replay, got {code:?}"
            );
        }
        other => panic!("expected Failure(DuplicateModification), got {other:?}"),
    }
}

#[test]
fn modify_balance_rejects_wrong_balance_pda_bump() {
    // Caller supplies a non-canonical balance_pda_bump. The program recomputes
    // and rejects.
    let mollusk = mollusk();
    let token_address = [0x77u8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x06,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        3104,
        105,
        2,
        2,
        &token_address,
        1,
        Uint256::from_u128(1_000),
        &[0u8; 32],
    );
    // Override bump with a deliberately-wrong value. There's a small chance
    // this happens to equal the canonical bump for a given fixture; bias
    // toward an unlikely high byte and assert distinct below.
    let (_, canonical_bump) = derive_balance_pda(2, 2, &token_address);
    let wrong_bump = if canonical_bump == 0 { 1 } else { canonical_bump - 1 };
    assert_ne!(canonical_bump, wrong_bump);

    let r = run_modify_balance(
        &mollusk,
        &body,
        2,
        2,
        &token_address,
        105,
        Some(wrong_bump),
        None,
        None,
    );
    match r.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidPda as u32,
                "expected InvalidPda from canonical-bump check, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidPda), got {other:?}"),
    }
}

#[test]
fn modify_balance_two_sequences_share_balance_pda_with_distinct_logs() {
    // Two governance VAAs that touch the same (chain, token_chain, token_address)
    // triple must both succeed AND land at distinct ModificationLog PDAs — i.e.
    // the modification PDA seeds key on `sequence`, not on the balance PDA.
    // Mirrors CosmWasm `MODIFICATIONS.update` keyed on sequence at
    // `cosmwasm/packages/accountant/src/contract.rs:248`.
    let mollusk = mollusk();
    let token_address = [0xE7u8; 32];

    let add_body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x20,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        3104,
        300,
        2,
        2,
        &token_address,
        1, // Add
        Uint256::from_u128(100),
        &[0u8; 32],
    );
    let r1 = run_modify_balance(&mollusk, &add_body, 2, 2, &token_address, 300, None, None, None);
    assert!(
        matches!(r1.program_result, ProgramResult::Success),
        "first Add must succeed, got {:?}",
        r1.program_result
    );

    let (balance_pda, _) = derive_balance_pda(2, 2, &token_address);
    let (mod_pda_300, _) = derive_modification_pda(300);
    let (mod_pda_301, _) = derive_modification_pda(301);
    assert_ne!(
        mod_pda_300, mod_pda_301,
        "distinct sequences must derive to distinct modification PDAs"
    );

    let post_balance = r1
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == balance_pda)
        .map(|(_, a)| a.clone())
        .expect("balance PDA missing after first call");

    let sub_body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x21,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        3104,
        301,
        2,
        2,
        &token_address,
        2, // Subtract
        Uint256::from_u128(100),
        &[0u8; 32],
    );
    let r2 = run_modify_balance(
        &mollusk,
        &sub_body,
        2,
        2,
        &token_address,
        301,
        None,
        Some(post_balance),
        None,
    );
    assert!(
        matches!(r2.program_result, ProgramResult::Success),
        "second Sub at distinct sequence must succeed, got {:?}",
        r2.program_result
    );

    let final_balance = r2
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == balance_pda)
        .map(|(_, a)| a.clone())
        .expect("balance PDA missing after second call");
    let layout: &BalanceAccountLayout = bytemuck::from_bytes(&final_balance.data);
    assert_eq!(
        layout.balance,
        Uint256::from_u128(0),
        "Add 100 then Sub 100 leaves balance at zero"
    );

    let mod_300_post = r1
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == mod_pda_300)
        .expect("modification PDA seq=300 missing");
    assert_eq!(mod_300_post.1.owner, program_id());
    let mod_301_post = r2
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == mod_pda_301)
        .expect("modification PDA seq=301 missing");
    assert_eq!(mod_301_post.1.owner, program_id());
}
