//! `register_chain` writes the `ChainRegistration` PDA. `submit_vaas` then reads that exact
//! account. This proves writer and reader agree on the layout.
//!
//! Mollusk with the real `solana_noreplay.so` and `wormhole_verify_vaa_shim.so`
//! (see `common::mollusk_fixtures`).

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        BalanceAccountLayout, ChainRegistrationLayout, GlobalAccountantError,
        Instruction as IxDiscriminator, Uint256, ACCOUNT_SEED_PREFIX, CHAIN_REGISTRATION_SEED_PREFIX,
        CORE_BRIDGE_PROGRAM_ID, GOVERNANCE_EMITTER, NOREPLAY_AUTHORITY_SEED_PREFIX,
        NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID, REGISTER_CHAIN_ACTION, SOLANA_CHAIN_ID,
        TOKEN_BRIDGE_GOVERNANCE_MODULE, VERIFY_VAA_SHIM_PROGRAM_ID,
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
fn noreplay_program_id() -> Pubkey {
    Pubkey::new_from_array(NOREPLAY_PROGRAM_ID)
}

fn double_keccak256_host(body: &[u8]) -> [u8; 32] {
    let inner = solana_keccak_hasher::hashv(&[body]).to_bytes();
    solana_keccak_hasher::hashv(&[&inner]).to_bytes()
}

fn derive_chain_registration_pda(chain: u16) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    Pubkey::find_program_address(&[CHAIN_REGISTRATION_SEED_PREFIX, &chain_be], &program_id())
}

fn derive_account_pda(chain: u16, token_chain: u16, token_address: &[u8; 32]) -> Pubkey {
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
    .0
}

fn derive_noreplay_authority() -> Pubkey {
    Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id()).0
}

fn derive_noreplay_bucket(authority: &Pubkey, chain: u16, emitter: &[u8; 32], sequence: u64) -> Pubkey {
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
        &noreplay_program_id(),
    )
    .0
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

fn find_account<'a>(accounts: &'a [(Pubkey, Account)], key: &Pubkey) -> &'a Account {
    &accounts
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("account {key} not in result list"))
        .1
}

fn build_register_chain_body(
    sequence: u64,
    chain_to_register: u16,
    emitter_to_register: &[u8; 32],
) -> Vec<u8> {
    let mut body = vec![0u8; 120];
    body[8..10].copy_from_slice(&SOLANA_CHAIN_ID.to_be_bytes());
    body[10..42].copy_from_slice(&GOVERNANCE_EMITTER);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body[51..83].copy_from_slice(&TOKEN_BRIDGE_GOVERNANCE_MODULE);
    body[83] = REGISTER_CHAIN_ACTION;
    body[84..86].copy_from_slice(&0u16.to_be_bytes()); // target_chain = Any
    body[86..88].copy_from_slice(&chain_to_register.to_be_bytes());
    body[88..120].copy_from_slice(emitter_to_register);
    body
}

fn register_chain_ix_data(guardian_set_bump: u8, registration_bump: u8, body: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 1 + 1 + 2 + body.len());
    data.push(IxDiscriminator::RegisterChain as u8);
    data.push(guardian_set_bump);
    data.push(registration_bump);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
    data
}

/// Run `register_chain`; return the written `ChainRegistration` account.
fn register_chain(
    mollusk: &Mollusk,
    chain_to_register: u16,
    emitter_to_register: &[u8; 32],
    sequence: u64,
    guardians: &[Guardian],
    payer: Pubkey,
) -> Account {
    let body = build_register_chain_body(sequence, chain_to_register, emitter_to_register);
    let digest = double_keccak256_host(&body);
    let (registration_pda, registration_bump) = derive_chain_registration_pda(chain_to_register);
    let (guardian_set, guardian_set_bump) =
        derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
    let guardian_signatures = Pubkey::new_from_array([0xC3u8; 32]);
    let noreplay_authority = derive_noreplay_authority();
    let noreplay_bucket =
        derive_noreplay_bucket(&noreplay_authority, SOLANA_CHAIN_ID, &GOVERNANCE_EMITTER, sequence);

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
        (registration_pda, uninitialised_pda_account()),
        (noreplay_bucket, system_owned_account(0)),
        keyed_account_for_noreplay_program(),
        (noreplay_authority, system_owned_account(0)),
        keyed_account_for_system_program(),
    ];
    let metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new_readonly(shim_program_id(), false),
        AccountMeta::new_readonly(guardian_set, false),
        AccountMeta::new_readonly(guardian_signatures, false),
        AccountMeta::new(registration_pda, false),
        AccountMeta::new(noreplay_bucket, false),
        AccountMeta::new_readonly(noreplay_program_id(), false),
        AccountMeta::new_readonly(noreplay_authority, false),
        AccountMeta::new_readonly(system_program_id(), false),
    ];

    let ix = Instruction::new_with_bytes(
        program_id(),
        &register_chain_ix_data(guardian_set_bump, registration_bump, &body),
        metas,
    );
    let r = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(r.program_result, ProgramResult::Success),
        "register_chain must succeed, got {:?}",
        r.program_result
    );
    find_account(&r.resulting_accounts, &registration_pda).clone()
}

fn build_transfer_body(
    emitter_chain: u16,
    emitter_address: &[u8; 32],
    sequence: u64,
    amount: u128,
    token_chain: u16,
    token_address: &[u8; 32],
    recipient_chain: u16,
) -> Vec<u8> {
    let mut body = vec![0u8; 51 + 133];
    body[8..10].copy_from_slice(&emitter_chain.to_be_bytes());
    body[10..42].copy_from_slice(emitter_address);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body[51] = 0x01;
    body[52 + 16..52 + 32].copy_from_slice(&amount.to_be_bytes());
    body[84..116].copy_from_slice(token_address);
    body[116..118].copy_from_slice(&token_chain.to_be_bytes());
    body[150..152].copy_from_slice(&recipient_chain.to_be_bytes());
    body
}

fn submit_vaas_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 1 + 2 + body.len());
    data.push(IxDiscriminator::SubmitVaas as u8);
    data.push(guardian_set_bump);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
    data
}

/// `submit_vaas` accepts the registration account that `register_chain` wrote.
#[test]
fn register_chain_then_submit_vaas_accepts_written_registration() {
    let mollusk = mollusk();
    let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
    let payer = Pubkey::new_from_array([0x11u8; 32]);

    let chain: u16 = 2;
    let mut emitter = [0u8; 32];
    emitter[31] = 0x77;

    // 1) `register_chain` writes the PDA.
    let written_registration = register_chain(&mollusk, chain, &emitter, 0x09, &guardians, payer);
    assert_eq!(
        written_registration.owner,
        program_id(),
        "register_chain output owned by the program"
    );
    // The written layout decodes to `(chain, emitter)`.
    let layout: &ChainRegistrationLayout = bytemuck::from_bytes(&written_registration.data);
    assert_eq!(layout.chain, chain);
    assert_eq!(layout.emitter_address, emitter);

    // 2) Transfer VAA from the same emitter and chain.
    let token_chain: u16 = 2;
    let token_address = [0x55u8; 32];
    let recipient_chain: u16 = 1;
    let amount: u128 = 4_321;
    let submit_sequence: u64 = 0x4242; // distinct from the register sequence
    let body = build_transfer_body(
        chain,
        &emitter,
        submit_sequence,
        amount,
        token_chain,
        &token_address,
        recipient_chain,
    );
    let digest = double_keccak256_host(&body);

    let (guardian_set, guardian_set_bump) =
        derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
    let guardian_signatures = Pubkey::new_from_array([0xC5u8; 32]);
    let noreplay_authority = derive_noreplay_authority();
    let noreplay_bucket =
        derive_noreplay_bucket(&noreplay_authority, chain, &emitter, submit_sequence);
    let (registration_pda, _) = derive_chain_registration_pda(chain);
    let source_account = derive_account_pda(chain, token_chain, &token_address);
    let dest_account = derive_account_pda(recipient_chain, token_chain, &token_address);

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
        (noreplay_bucket, system_owned_account(0)),
        keyed_account_for_noreplay_program(),
        (noreplay_authority, system_owned_account(0)),
        (source_account, uninitialised_pda_account()),
        (dest_account, uninitialised_pda_account()),
        keyed_account_for_system_program(),
        // Slot 10: the account `register_chain` wrote.
        (registration_pda, written_registration),
    ];
    let metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new_readonly(shim_program_id(), false),
        AccountMeta::new_readonly(guardian_set, false),
        AccountMeta::new_readonly(guardian_signatures, false),
        AccountMeta::new(noreplay_bucket, false),
        AccountMeta::new_readonly(noreplay_program_id(), false),
        AccountMeta::new_readonly(noreplay_authority, false),
        AccountMeta::new(source_account, false),
        AccountMeta::new(dest_account, false),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new_readonly(registration_pda, false),
    ];

    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_vaas_ix_data(guardian_set_bump, &body),
        metas,
    );
    let r = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(r.program_result, ProgramResult::Success),
        "submit_vaas must accept the register_chain-written registration, got {:?}",
        r.program_result
    );

    // Source (native) credited.
    let src = find_account(&r.resulting_accounts, &source_account);
    assert_eq!(src.owner, program_id(), "source Account PDA owned by program");
    let src_layout: &BalanceAccountLayout = bytemuck::from_bytes(&src.data);
    assert_eq!(src_layout.balance, Uint256::from_u128(amount));

    // Negative control: a registration for another emitter fails.
    let other_emitter = [0xEEu8; 32];
    let mismatched = register_chain(&mollusk, chain, &other_emitter, 0x0A, &guardians, payer);
    let mut bad_accounts = accounts_clone_for_replay(
        payer,
        guardian_set,
        &keys,
        guardian_signatures,
        &sigs,
        noreplay_bucket,
        noreplay_authority,
        source_account,
        dest_account,
        registration_pda,
        mismatched,
    );
    // Fresh bucket so the failure is the emitter mismatch.
    if let Some(e) = bad_accounts.iter_mut().find(|(k, _)| *k == noreplay_bucket) {
        e.1 = system_owned_account(0);
    }
    let ix2 = Instruction::new_with_bytes(
        program_id(),
        &submit_vaas_ix_data(guardian_set_bump, &body),
        vec![
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(guardian_set, false),
            AccountMeta::new_readonly(guardian_signatures, false),
            AccountMeta::new(noreplay_bucket, false),
            AccountMeta::new_readonly(noreplay_program_id(), false),
            AccountMeta::new_readonly(noreplay_authority, false),
            AccountMeta::new(source_account, false),
            AccountMeta::new(dest_account, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(registration_pda, false),
        ],
    );
    let r2 = mollusk.process_instruction(&ix2, &bad_accounts);
    match r2.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::UnregisteredEmitter as u32,
                "a registration for a different emitter must reject UnregisteredEmitter, got {code}"
            );
        }
        other => panic!("expected Failure(UnregisteredEmitter), got {other:?}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn accounts_clone_for_replay(
    payer: Pubkey,
    guardian_set: Pubkey,
    keys: &[[u8; GUARDIAN_PUBKEY_LENGTH]],
    guardian_signatures: Pubkey,
    sigs: &[(u8, [u8; 65])],
    noreplay_bucket: Pubkey,
    noreplay_authority: Pubkey,
    source_account: Pubkey,
    dest_account: Pubkey,
    registration_pda: Pubkey,
    registration_state: Account,
) -> Vec<(Pubkey, Account)> {
    vec![
        (payer, system_owned_account(50_000_000_000)),
        keyed_account_for_verify_vaa_shim_program(),
        (
            guardian_set,
            guardian_set_account(GUARDIAN_SET_INDEX, keys, 0, 0, &core_bridge_program_id()),
        ),
        (
            guardian_signatures,
            guardian_signatures_account(GUARDIAN_SET_INDEX, &payer, sigs, &shim_program_id()),
        ),
        (noreplay_bucket, system_owned_account(0)),
        keyed_account_for_noreplay_program(),
        (noreplay_authority, system_owned_account(0)),
        (source_account, uninitialised_pda_account()),
        (dest_account, uninitialised_pda_account()),
        keyed_account_for_system_program(),
        (registration_pda, registration_state),
    ]
}
