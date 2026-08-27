//! Mollusk integration tests for `modify_balance`, with the real `wormhole_verify_vaa_shim.so`
//! (see `common::mollusk_fixtures`).

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        BalanceAccountLayout, GlobalAccountantError, Instruction as IxDiscriminator,
        ModifyBalanceLayout, Uint256, ACCOUNTANT_GOVERNANCE_MODULE, ACCOUNT_SEED_PREFIX,
        CORE_BRIDGE_PROGRAM_ID, GOVERNANCE_EMITTER, MODIFY_BALANCE_ACTION,
        MODIFY_BALANCE_SEED_PREFIX, SOLANA_CHAIN_ID, VERIFY_VAA_SHIM_PROGRAM_ID,
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
use common::mollusk_fixtures::{keyed_account_for_verify_vaa_shim_program, mollusk_with_fixtures};

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

fn derive_modify_balance_pda(sequence: u64) -> (Pubkey, u8) {
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

/// `BalanceAccount` PDA fixture with `balance`.
fn balance_account(
    chain: u16,
    token_chain: u16,
    token_address: &[u8; 32],
    balance: Uint256,
) -> Account {
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

/// `ModifyBalance` governance VAA body, after the 51-byte header:
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

fn modify_balance_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    // Wire: discriminator + guardian_set_bump + body_len(u16 LE) + body.
    let mut data = Vec::with_capacity(1 + 1 + 2 + body.len());
    data.push(IxDiscriminator::ModifyBalance as u8);
    data.push(guardian_set_bump);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
    data
}

/// Account fixtures and metas for `modify_balance`:
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
    modify_balance_pda: Pubkey,
) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(payer, true),
        AccountMeta::new_readonly(shim_program_id(), false),
        AccountMeta::new_readonly(guardian_set, false),
        AccountMeta::new_readonly(guardian_signatures, false),
        AccountMeta::new(balance_pda, false),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new(modify_balance_pda, false),
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
    modify_balance_pda: Pubkey,
    modify_balance_pda_state: Account,
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
        (modify_balance_pda, modify_balance_pda_state),
    ]
}

/// Single driver over the standard fixtures.
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
    let (modify_balance_pda, _) = derive_modify_balance_pda(payload_sequence);

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
        modify_balance_pda,
        modification_initial.unwrap_or_else(uninitialised_pda_account),
    );
    let metas = build_metas(
        payer,
        guardian_set,
        guardian_signatures,
        balance_pda,
        modify_balance_pda,
    );

    let ix = Instruction::new_with_bytes(
        program_id(),
        &modify_balance_ix_data(guardian_set_bump, body),
        metas,
    );
    mollusk.process_instruction(&ix, &accounts)
}

/// Each case mutates one body field. Signatures are re-signed over the mutated body, so
/// the Shim passes and the rejection comes from the governance-header validation.
#[test]
fn modify_balance_body_header_violations_reject() {
    struct Case {
        label: &'static str,
        emitter: [u8; 32],
        module: [u8; 32],
        action: u8,
        target_chain: u16,
        kind: u8,
        sequence: u64,
        payload_seq: u64,
        expected: u32,
    }
    let canonical = Case {
        label: "(canonical baseline — not run)",
        emitter: GOVERNANCE_EMITTER,
        module: ACCOUNTANT_GOVERNANCE_MODULE,
        action: MODIFY_BALANCE_ACTION,
        target_chain: SOLANA_CHAIN_ID,
        kind: 1,
        sequence: 0,
        payload_seq: 0,
        expected: 0,
    };
    let cases = [
        Case {
            label: "non-governance emitter",
            emitter: [0xDEu8; 32],
            sequence: 0x01,
            payload_seq: 100,
            expected: GlobalAccountantError::InvalidGovernanceEmitter as u32,
            ..canonical
        },
        Case {
            label: "wrong governance module",
            module: [0xAAu8; 32],
            sequence: 0x02,
            payload_seq: 101,
            expected: GlobalAccountantError::InvalidGovernanceModule as u32,
            ..canonical
        },
        Case {
            label: "wrong action byte",
            action: 0x02,
            sequence: 0x03,
            payload_seq: 102,
            expected: GlobalAccountantError::InvalidGovernanceAction as u32,
            ..canonical
        },
        Case {
            label: "target_chain != Solana",
            target_chain: 0, // Any — modify_balance refuses
            sequence: 0x04,
            payload_seq: 103,
            expected: GlobalAccountantError::GovernanceChainMismatch as u32,
            ..canonical
        },
        Case {
            label: "invalid modification kind",
            kind: 0, // Unknown(0)
            sequence: 0x05,
            payload_seq: 104,
            expected: GlobalAccountantError::InvalidModificationKind as u32,
            ..canonical
        },
    ];

    let mollusk = mollusk();
    let token_address = [0x77u8; 32];
    for case in cases {
        let body = build_modify_balance_body(
            SOLANA_CHAIN_ID,
            &case.emitter,
            case.sequence,
            &case.module,
            case.action,
            case.target_chain,
            case.payload_seq,
            2,
            2,
            &token_address,
            case.kind,
            Uint256::from_u128(1_000),
            &[0u8; 32],
        );
        let r = run_modify_balance(
            &mollusk,
            &body,
            2,
            2,
            &token_address,
            case.payload_seq,
            None,
            None,
        );
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

/// Add on a fresh triple creates the `BalanceAccount` and the `Modification`.
#[test]
fn modify_balance_add_on_uninit_pda_initialises_and_credits() {
    let mollusk = mollusk();
    let token_address = [0x77u8; 32];
    let reason = *b"audit-log: post-incident credit ";
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x10,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        SOLANA_CHAIN_ID,
        200,
        2,
        2,
        &token_address,
        1,
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

    let (modify_balance_pda, _) = derive_modify_balance_pda(200);
    let post_log = r
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == modify_balance_pda)
        .expect("modification PDA missing from result");
    assert_eq!(
        post_log.1.owner,
        program_id(),
        "modification PDA owned by program"
    );
    let log: &ModifyBalanceLayout = bytemuck::from_bytes(&post_log.1.data);
    assert_eq!(log.sequence, 200);
    assert_eq!(log.chain_id, 2);
    assert_eq!(log.token_chain, 2);
    assert_eq!(log.kind, 1); // Add
    assert_eq!(log.amount, Uint256::from_u128(1_000_000));
    assert_eq!(
        log.reason, reason,
        "reason persisted to the modification log"
    );
}

/// Subtract from 5000 by 1500 gives 3500.
#[test]
fn modify_balance_sub_on_existing_pda_debits() {
    let mollusk = mollusk();
    let token_address = [0x88u8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x11,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        SOLANA_CHAIN_ID,
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
    assert_eq!(
        layout.balance,
        Uint256::from_u128(3_500),
        "5000 - 1500 = 3500"
    );
}

/// Add to 100 by 50 gives 150.
#[test]
fn modify_balance_add_on_existing_pda_credits() {
    let mollusk = mollusk();
    let token_address = [0x99u8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x12,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        SOLANA_CHAIN_ID,
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

/// Subtract on an absent `BalanceAccount`: `ModifyBalanceUnderflow` before allocation.
#[test]
fn modify_balance_sub_on_uninit_pda_rejects_underflow() {
    let mollusk = mollusk();
    let token_address = [0xAAu8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x13,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        SOLANA_CHAIN_ID,
        203,
        2,
        2,
        &token_address,
        2, // Subtract
        Uint256::from_u128(1),
        &[0u8; 32],
    );
    let r = run_modify_balance(&mollusk, &body, 2, 2, &token_address, 203, None, None);
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

/// Add near `MAX`: `ModifyBalanceOverflow`.
#[test]
fn modify_balance_add_overflow_rejects() {
    let mollusk = mollusk();
    let token_address = [0xBBu8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x14,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        SOLANA_CHAIN_ID,
        204,
        2,
        2,
        &token_address,
        1, // Add
        Uint256::from_u128(2),
        &[0u8; 32],
    );
    // `MAX - 1` plus 2 overflows.
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

/// Second VAA with the same payload sequence: `DuplicateModifyBalance`.
#[test]
fn modify_balance_rejects_duplicate_modification_sequence() {
    let mollusk = mollusk();
    let token_address = [0xCCu8; 32];
    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x15,
        &ACCOUNTANT_GOVERNANCE_MODULE,
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
    let (modify_balance_pda, _) = derive_modify_balance_pda(205);
    let post_balance = r1
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == balance_pda)
        .map(|(_, a)| a.clone())
        .expect("balance PDA missing");
    let post_modification = r1
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == modify_balance_pda)
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

/// Two VAAs on one balance triple succeed with distinct `ModifyBalance` PDAs.
#[test]
fn modify_balance_two_sequences_share_balance_pda_with_distinct_logs() {
    let mollusk = mollusk();
    let token_address = [0xE7u8; 32];

    let add_body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x20,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        SOLANA_CHAIN_ID,
        300,
        2,
        2,
        &token_address,
        1, // Add
        Uint256::from_u128(100),
        &[0u8; 32],
    );
    let r1 = run_modify_balance(&mollusk, &add_body, 2, 2, &token_address, 300, None, None);
    assert!(
        matches!(r1.program_result, ProgramResult::Success),
        "first Add must succeed, got {:?}",
        r1.program_result
    );

    let (balance_pda, _) = derive_balance_pda(2, 2, &token_address);
    let (mod_pda_300, _) = derive_modify_balance_pda(300);
    let (mod_pda_301, _) = derive_modify_balance_pda(301);
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
        SOLANA_CHAIN_ID,
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
