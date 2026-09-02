//! Integration tests for the NTT `modify_balance` governance handler.
//!
//! Driven against a Mollusk instance with the real `wormhole_verify_vaa_shim.so`
//! loaded at the canonical Shim program ID (see `common::mollusk_fixtures`).
//! Mirrors the WTT `modify_balance` suite; the only behavioural difference is
//! the governance module string (`NTT_ACCOUNTANT_GOVERNANCE_MODULE`).

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        BalanceAccountLayout, GlobalAccountantError, ModifyBalanceLayout, Uint256,
        ACCOUNT_SEED_PREFIX, CORE_BRIDGE_PROGRAM_ID, GOVERNANCE_EMITTER, MODIFY_BALANCE_SEED_PREFIX,
        MODIFY_BALANCE_ACTION, NTT_ACCOUNTANT_GOVERNANCE_MODULE, SOLANA_CHAIN_ID,
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
    sign_digest, Guardian, GUARDIAN_PUBKEY_LENGTH,
};
use common::mollusk_fixtures::{keyed_account_for_verify_vaa_shim_program, mollusk_with_fixtures};

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

fn derive_balance_pda(chain: u16, token_chain: u16, token_address: &[u8; 32]) -> (Pubkey, u8) {
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

fn derive_modification_pda(sequence: u64) -> (Pubkey, u8) {
    let seq_be = sequence.to_be_bytes();
    Pubkey::find_program_address(&[MODIFY_BALANCE_SEED_PREFIX, &seq_be], &program_id())
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

fn balance_account(chain: u16, token_chain: u16, token_address: &[u8; 32], balance: Uint256) -> Account {
    let mut layout: BalanceAccountLayout = bytemuck::Zeroable::zeroed();
    layout.tag = BalanceAccountLayout::TAG;
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

/// Build an NTT `ModifyBalance` governance VAA body (195 bytes). Same layout as
/// the WTT handler; only the module string differs.
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

fn modify_balance_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 1 + 2 + body.len());
    data.push(IxDiscriminator::ModifyBalance as u8);
    data.push(guardian_set_bump);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
    data
}

/// Account slot order:
///   0. payer (SIGNER, WRITE)
///   1. Verify VAA Shim program
///   2. Core Bridge GuardianSet
///   3. GuardianSignatures
///   4. BalanceAccount PDA (WRITE)
///   5. system program
///   6. Modification PDA (WRITE)
fn build_metas(
    payer: Pubkey,
    guardian_set: Pubkey,
    guardian_signatures: Pubkey,
    balance_pda: Pubkey,
    modification_pda: Pubkey,
) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(payer, true),
        AccountMeta::new_readonly(shim_program_id(), false),
        AccountMeta::new_readonly(guardian_set, false),
        AccountMeta::new_readonly(guardian_signatures, false),
        AccountMeta::new(balance_pda, false),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new(modification_pda, false),
    ]
}

fn build_initial_accounts(
    payer: Pubkey,
    digest: &[u8; 32],
    guardian_set_pubkey: Pubkey,
    guardian_signatures_pubkey: Pubkey,
    guardians: &[Guardian],
    balance_pda: Pubkey,
    balance_pda_state: Account,
    modification_pda: Pubkey,
    modification_pda_state: Account,
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
        (balance_pda, balance_pda_state),
        keyed_account_for_system_program(),
        (modification_pda, modification_pda_state),
    ]
}

#[allow(clippy::too_many_arguments)]
fn run_modify_balance(
    mollusk: &Mollusk,
    body: &[u8],
    chain_id: u16,
    token_chain: u16,
    token_address: &[u8; 32],
    payload_sequence: u64,
    balance_initial: Option<Account>,
    modification_initial: Option<Account>,
) -> mollusk_svm::result::InstructionResult {
    let (balance_pda, _) = derive_balance_pda(chain_id, token_chain, token_address);
    let (modification_pda, _) = derive_modification_pda(payload_sequence);

    let payer = Pubkey::new_from_array([0x11u8; 32]);
    let guardian_signatures = Pubkey::new_from_array([0xC3u8; 32]);
    let (guardian_set, guardian_set_bump) =
        derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
    let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
    let digest = double_keccak256_host(body);

    let accounts = build_initial_accounts(
        payer,
        &digest,
        guardian_set,
        guardian_signatures,
        &guardians,
        balance_pda,
        balance_initial.unwrap_or_else(uninitialised_pda_account),
        modification_pda,
        modification_initial.unwrap_or_else(uninitialised_pda_account),
    );
    let metas = build_metas(
        payer,
        guardian_set,
        guardian_signatures,
        balance_pda,
        modification_pda,
    );

    let ix = Instruction::new_with_bytes(
        program_id(),
        &modify_balance_ix_data(guardian_set_bump, body),
        metas,
    );
    mollusk.process_instruction(&ix, &accounts)
}

/// Add on a fresh triple lazy-inits the BalanceAccount and creates the
/// Modification under the NTT governance module.
#[test]
fn modify_balance_add_on_uninit_pda_initialises_and_credits() {
    let mollusk = mollusk();
    let token_address = [0x77u8; 32];
    let reason = *b"audit-log: post-incident credit ";
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x10,
        &NTT_ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        SOLANA_CHAIN_ID,
        200,
        2,
        2,
        &token_address,
        1, // Add
        Uint256::from_u128(1_000_000),
        &reason,
    );
    let r = run_modify_balance(&mollusk, &body, 2, 2, &token_address, 200, None, None);
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
    assert_eq!(
        post_balance.1.owner,
        program_id(),
        "balance PDA owned by program"
    );
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
    assert_eq!(
        post_log.1.owner,
        program_id(),
        "modification PDA owned by program"
    );
    let log: &ModifyBalanceLayout = bytemuck::from_bytes(&post_log.1.data);
    assert_eq!(log.sequence, 200);
    assert_eq!(log.chain_id, 2);
    assert_eq!(log.kind, 1);
    assert_eq!(log.amount, Uint256::from_u128(1_000_000));
    assert_eq!(
        log.reason, reason,
        "reason persisted to the modification log"
    );
}

/// A VAA carrying the WTT `GlobalAccountant` module (not the NTT module) is
/// rejected with `InvalidGovernanceModule` — the module string scopes the VAA
/// to the NTT program.
#[test]
fn modify_balance_wrong_governance_module_rejects() {
    let mollusk = mollusk();
    let token_address = [0x88u8; 32];
    // Any module that is not NTT_ACCOUNTANT_GOVERNANCE_MODULE (here the WTT one).
    let wrong_module = global_accountant_definitions::ACCOUNTANT_GOVERNANCE_MODULE;
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x11,
        &wrong_module,
        MODIFY_BALANCE_ACTION,
        SOLANA_CHAIN_ID,
        210,
        2,
        2,
        &token_address,
        1,
        Uint256::from_u128(1_000),
        &[0u8; 32],
    );
    let r = run_modify_balance(&mollusk, &body, 2, 2, &token_address, 210, None, None);
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
/// `InvalidGovernanceEmitter` — governance VAAs must originate from the
/// canonical Solana governance emitter.
#[test]
fn modify_balance_wrong_emitter_chain_rejects() {
    let mollusk = mollusk();
    let token_address = [0x89u8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID + 1, // wrong emitter chain
        &GOVERNANCE_EMITTER,
        0x12,
        &NTT_ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        SOLANA_CHAIN_ID,
        211,
        2,
        2,
        &token_address,
        1,
        Uint256::from_u128(1_000),
        &[0u8; 32],
    );
    let r = run_modify_balance(&mollusk, &body, 2, 2, &token_address, 211, None, None);
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
fn modify_balance_wrong_emitter_address_rejects() {
    let mollusk = mollusk();
    let token_address = [0x8Au8; 32];
    let wrong_emitter = [0x01u8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &wrong_emitter,
        0x13,
        &NTT_ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        SOLANA_CHAIN_ID,
        212,
        2,
        2,
        &token_address,
        1,
        Uint256::from_u128(1_000),
        &[0u8; 32],
    );
    let r = run_modify_balance(&mollusk, &body, 2, 2, &token_address, 212, None, None);
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

/// A VAA carrying the correct NTT governance module but a non-`0x01` action
/// byte is rejected with `InvalidGovernanceAction`.
#[test]
fn modify_balance_wrong_governance_action_rejects() {
    let mollusk = mollusk();
    let token_address = [0x8Bu8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x14,
        &NTT_ACCOUNTANT_GOVERNANCE_MODULE,
        0xFF, // not MODIFY_BALANCE_ACTION
        SOLANA_CHAIN_ID,
        213,
        2,
        2,
        &token_address,
        1,
        Uint256::from_u128(1_000),
        &[0u8; 32],
    );
    let r = run_modify_balance(&mollusk, &body, 2, 2, &token_address, 213, None, None);
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

/// A VAA whose `target_chain` is neither Solana nor `Any` (unlike Token Bridge
/// governance, `modify_balance` does not accept `Any`) is rejected with
/// `GovernanceChainMismatch`.
#[test]
fn modify_balance_wrong_target_chain_rejects() {
    let mollusk = mollusk();
    let token_address = [0x8Cu8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x16,
        &NTT_ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        0, // Any — not accepted for modify_balance, unlike register_chain
        214,
        2,
        2,
        &token_address,
        1,
        Uint256::from_u128(1_000),
        &[0u8; 32],
    );
    let r = run_modify_balance(&mollusk, &body, 2, 2, &token_address, 214, None, None);
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

/// A second VAA with the same payload sequence collides on the Modification PDA
/// and rejects with `DuplicateModifyBalance`.
#[test]
fn modify_balance_rejects_duplicate_modification_sequence() {
    let mollusk = mollusk();
    let token_address = [0xCCu8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x15,
        &NTT_ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        SOLANA_CHAIN_ID,
        205,
        2,
        2,
        &token_address,
        1,
        Uint256::from_u128(100),
        &[0u8; 32],
    );

    let r1 = run_modify_balance(&mollusk, &body, 2, 2, &token_address, 205, None, None);
    assert!(matches!(r1.program_result, ProgramResult::Success));

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
        Some(post_balance),
        Some(post_modification),
    );
    match r2.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::DuplicateModifyBalance as u32,
                "expected DuplicateModifyBalance on replay, got {code:?}"
            );
        }
        other => panic!("expected Failure(DuplicateModifyBalance), got {other:?}"),
    }
}

/// A `kind` byte that is neither `1` (Add) nor `2` (Subtract) rejects with
/// `InvalidModificationKind` before any account is touched.
#[test]
fn modify_balance_invalid_modification_kind_rejects() {
    let mollusk = mollusk();
    let token_address = [0xDDu8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x20,
        &NTT_ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        SOLANA_CHAIN_ID,
        220,
        2,
        2,
        &token_address,
        3, // neither Add (1) nor Subtract (2)
        Uint256::from_u128(100),
        &[0u8; 32],
    );
    let r = run_modify_balance(&mollusk, &body, 2, 2, &token_address, 220, None, None);
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

    // Balance PDA stays uninitialised: the kind check runs before allocation.
    let (balance_pda, _) = derive_balance_pda(2, 2, &token_address);
    let post_balance = r
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == balance_pda)
        .expect("balance PDA missing from result");
    assert_eq!(post_balance.1.owner, system_program_id());
    assert!(post_balance.1.data.is_empty());
}

/// `Subtract` against an uninitialised `BalanceAccount` underflows from zero
/// and is rejected before allocation (mirrors the WTT sibling's coverage of
/// the same rejection).
#[test]
fn modify_balance_sub_on_uninit_pda_rejects_underflow() {
    let mollusk = mollusk();
    let token_address = [0xEEu8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x21,
        &NTT_ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        SOLANA_CHAIN_ID,
        221,
        2,
        2,
        &token_address,
        2, // Subtract
        Uint256::from_u128(1),
        &[0u8; 32],
    );
    let r = run_modify_balance(&mollusk, &body, 2, 2, &token_address, 221, None, None);
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

    let (balance_pda, _) = derive_balance_pda(2, 2, &token_address);
    let post_balance = r
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == balance_pda)
        .expect("balance PDA missing from result");
    assert_eq!(post_balance.1.owner, system_program_id());
    assert!(post_balance.1.data.is_empty());
}

/// `Add` against an existing (non-zero) balance near `Uint256::MAX` overflows
/// and rejects with `ModifyBalanceOverflow`, leaving the balance unchanged.
#[test]
fn modify_balance_add_overflow_against_existing_balance_rejects() {
    let mollusk = mollusk();
    let token_address = [0xF1u8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x22,
        &NTT_ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        SOLANA_CHAIN_ID,
        222,
        2,
        2,
        &token_address,
        1, // Add
        Uint256::from_u128(2),
        &[0u8; 32],
    );
    let mut max_minus_one_bytes = [0xFFu8; 32];
    max_minus_one_bytes[31] = 0xFE;
    let pre_balance = balance_account(2, 2, &token_address, Uint256(max_minus_one_bytes));
    let r = run_modify_balance(
        &mollusk,
        &body,
        2,
        2,
        &token_address,
        222,
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

    let (balance_pda, _) = derive_balance_pda(2, 2, &token_address);
    let post_balance = r
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == balance_pda)
        .expect("balance PDA missing from result");
    let layout: &BalanceAccountLayout = bytemuck::from_bytes(&post_balance.1.data);
    assert_eq!(
        layout.balance,
        Uint256(max_minus_one_bytes),
        "balance unchanged on overflow rejection"
    );
}

/// `Subtract` against an existing (non-zero) balance smaller than the
/// modification amount underflows and rejects with `ModifyBalanceUnderflow`,
/// leaving the balance unchanged.
#[test]
fn modify_balance_sub_underflow_against_existing_balance_rejects() {
    let mollusk = mollusk();
    let token_address = [0xF2u8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x23,
        &NTT_ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        SOLANA_CHAIN_ID,
        223,
        2,
        2,
        &token_address,
        2, // Subtract
        Uint256::from_u128(1_000),
        &[0u8; 32],
    );
    let pre_balance = balance_account(2, 2, &token_address, Uint256::from_u128(999));
    let r = run_modify_balance(
        &mollusk,
        &body,
        2,
        2,
        &token_address,
        223,
        Some(pre_balance),
        None,
    );
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

    let (balance_pda, _) = derive_balance_pda(2, 2, &token_address);
    let post_balance = r
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == balance_pda)
        .expect("balance PDA missing from result");
    let layout: &BalanceAccountLayout = bytemuck::from_bytes(&post_balance.1.data);
    assert_eq!(
        layout.balance,
        Uint256::from_u128(999),
        "balance unchanged on underflow rejection"
    );
}
