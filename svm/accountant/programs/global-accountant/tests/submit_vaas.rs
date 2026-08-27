//! Mollusk integration tests for `submit_vaas`, with the real `solana_noreplay.so` and
//! `wormhole_verify_vaa_shim.so` (see `common::mollusk_fixtures`).

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        BalanceAccountLayout, ChainRegistrationLayout, GlobalAccountantError,
        Instruction as IxDiscriminator, NoReplayBitmapAccount, Uint256, ACCOUNT_SEED_PREFIX,
        CHAIN_REGISTRATION_SEED_PREFIX, CORE_BRIDGE_PROGRAM_ID, NOREPLAY_AUTHORITY_SEED_PREFIX,
        NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID, VERIFY_VAA_SHIM_PROGRAM_ID,
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
/// Guardian set index in the fixtures. `submit_vaas` logs `guardian_set_index = 0`.
const GUARDIAN_SET_INDEX: u32 = 4;

fn program_id() -> Pubkey {
    // Fixed program id; PDA derivation must match the program.
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

fn derive_account_pda(chain: u16, token_chain: u16, token_address: &[u8; 32]) -> (Pubkey, u8) {
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

fn derive_chain_registration_pda(chain: u16) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    Pubkey::find_program_address(&[CHAIN_REGISTRATION_SEED_PREFIX, &chain_be], &program_id())
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

fn chain_registration_account(chain: u16, emitter_address: &[u8; 32]) -> Account {
    let mut layout: ChainRegistrationLayout = bytemuck::Zeroable::zeroed();
    layout.tag = ChainRegistrationLayout::TAG;
    layout.chain = chain;
    layout.emitter_address = *emitter_address;
    Account {
        lamports: 1_000_000,
        data: bytemuck::bytes_of(&layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

/// Dedup digest `keccak256(keccak256(body))`.
fn double_keccak256_host(body: &[u8]) -> [u8; 32] {
    let inner = solana_keccak_hasher::hashv(&[body]).to_bytes();
    solana_keccak_hasher::hashv(&[&inner]).to_bytes()
}

fn build_attest_body(emitter_chain: u16, emitter_address: &[u8; 32], sequence: u64) -> Vec<u8> {
    let mut body = vec![0u8; 52];
    body[8..10].copy_from_slice(&emitter_chain.to_be_bytes());
    body[10..42].copy_from_slice(emitter_address);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body[51] = 0x02;
    body
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
    body[118] = 0xAB;
    body[149] = 0xCD;
    body[150..152].copy_from_slice(&recipient_chain.to_be_bytes());
    body
}

fn submit_vaas_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    // Wire: discriminator + guardian_set_bump + body_len(u16 LE) + body.
    let mut data = Vec::with_capacity(1 + 1 + 2 + body.len());
    data.push(IxDiscriminator::SubmitVaas as u8);
    data.push(guardian_set_bump);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
    data
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

/// Uninitialised NoReplay bucket.
fn noreplay_bucket_unmarked() -> Account {
    system_owned_account(0)
}

/// NoReplay bucket with the bit at `sequence % 1024` set.
fn noreplay_bucket_marked(sequence: u64) -> Account {
    let mut account: NoReplayBitmapAccount = bytemuck::Zeroable::zeroed();
    let bit_index = (sequence % NOREPLAY_BITS_PER_BUCKET) as usize;
    account.bitmap[bit_index / 8] |= 1u8 << (bit_index % 8);
    Account {
        lamports: 1_500_000_000,
        data: bytemuck::bytes_of(&account).to_vec(),
        owner: Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        executable: false,
        rent_epoch: 0,
    }
}

/// `GuardianSignatures` fixture in the Shim layout, signed by a quorum.
fn real_guardian_signatures_account(
    digest: &[u8; 32],
    refund_recipient: &Pubkey,
    guardians: &[Guardian],
) -> Account {
    let sigs: Vec<(u8, [u8; 65])> = (0..QUORUM)
        .map(|i| (i, sign_digest(&guardians[i as usize], digest)))
        .collect();
    guardian_signatures_account(
        GUARDIAN_SET_INDEX,
        refund_recipient,
        &sigs,
        &shim_program_id(),
    )
}

/// `GuardianSet` fixture: 19 addresses, never expires, Core-Bridge-owned.
fn real_guardian_set_account(guardians: &[Guardian]) -> Account {
    let keys: Vec<[u8; GUARDIAN_PUBKEY_LENGTH]> = guardians.iter().map(|g| g.eth_address).collect();
    guardian_set_account(GUARDIAN_SET_INDEX, &keys, 0, 0, &core_bridge_program_id())
}

#[derive(Clone)]
struct Scenario {
    chain: u16,
    emitter: [u8; 32],
    sequence: u64,
    body: Vec<u8>,
    digest: [u8; 32],
    guardian_set_bump: u8,
    submitter: Pubkey,
    guardian_set_pubkey: Pubkey,
    guardian_signatures_pubkey: Pubkey,
    noreplay_bucket_pubkey: Pubkey,
    noreplay_authority_pubkey: Pubkey,
    source_account_pubkey: Pubkey,
    dest_account_pubkey: Pubkey,
    chain_registration_pubkey: Pubkey,
    guardians: Vec<Guardian>,
}

impl Scenario {
    /// Default Attest scenario; `with_transfer_body` swaps in a Transfer.
    fn new(seed: u8) -> Self {
        let chain: u16 = 2;
        let mut emitter = [0u8; 32];
        emitter[31] = 0x77;
        emitter[0] = seed; // per-scenario emitter so PDA tests are isolated
        let sequence: u64 = 0x0000_0000_0000_0042;

        let body = build_attest_body(chain, &emitter, sequence);
        let digest = double_keccak256_host(&body);

        let submitter = Pubkey::new_from_array([0x11u8; 32]);
        let (noreplay_authority_pubkey, _) =
            Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
        let (chain_registration_pubkey, _) = derive_chain_registration_pda(chain);
        let noreplay_bucket_pubkey =
            derive_canonical_noreplay_bucket(&noreplay_authority_pubkey, chain, &emitter, sequence);
        let (guardian_set_pubkey, guardian_set_bump) =
            derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
        let guardians = make_guardians(GUARDIAN_COUNT, 0x42);

        Self {
            chain,
            emitter,
            sequence,
            body,
            digest,
            guardian_set_bump,
            submitter,
            guardian_set_pubkey,
            guardian_signatures_pubkey: Pubkey::new_from_array([0xC5u8; 32]),
            noreplay_bucket_pubkey,
            noreplay_authority_pubkey,
            // Attest: slots 7/8 unused.
            source_account_pubkey: noreplay_authority_pubkey,
            dest_account_pubkey: noreplay_authority_pubkey,
            chain_registration_pubkey,
            guardians,
        }
    }

    fn with_transfer_body(
        seed: u8,
        amount: u128,
        token_chain: u16,
        token_address: [u8; 32],
        recipient_chain: u16,
    ) -> Self {
        let mut base = Self::new(seed);
        base.body = build_transfer_body(
            base.chain,
            &base.emitter,
            base.sequence,
            amount,
            token_chain,
            &token_address,
            recipient_chain,
        );
        base.digest = double_keccak256_host(&base.body);
        let (src, _) = derive_account_pda(base.chain, token_chain, &token_address);
        let (dst, _) = derive_account_pda(recipient_chain, token_chain, &token_address);
        base.source_account_pubkey = src;
        base.dest_account_pubkey = dst;
        base
    }

    fn account_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.submitter, true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(self.guardian_set_pubkey, false),
            AccountMeta::new_readonly(self.guardian_signatures_pubkey, false),
            AccountMeta::new(self.noreplay_bucket_pubkey, false),
            AccountMeta::new_readonly(Pubkey::new_from_array(NOREPLAY_PROGRAM_ID), false),
            AccountMeta::new_readonly(self.noreplay_authority_pubkey, false),
            AccountMeta::new(self.source_account_pubkey, false),
            AccountMeta::new(self.dest_account_pubkey, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(self.chain_registration_pubkey, false),
        ]
    }

    fn initial_accounts(&self) -> Vec<(Pubkey, Account)> {
        let mut accounts = vec![
            (self.submitter, system_owned_account(50_000_000_000)),
            keyed_account_for_verify_vaa_shim_program(),
            (
                self.guardian_set_pubkey,
                real_guardian_set_account(&self.guardians),
            ),
            (
                self.guardian_signatures_pubkey,
                real_guardian_signatures_account(&self.digest, &self.submitter, &self.guardians),
            ),
            (self.noreplay_bucket_pubkey, noreplay_bucket_unmarked()),
            keyed_account_for_noreplay_program(),
            (self.noreplay_authority_pubkey, system_owned_account(0)),
        ];
        if self.source_account_pubkey != self.noreplay_authority_pubkey {
            accounts.push((self.source_account_pubkey, uninitialised_pda_account()));
        }
        if self.dest_account_pubkey != self.noreplay_authority_pubkey
            && self.dest_account_pubkey != self.source_account_pubkey
        {
            accounts.push((self.dest_account_pubkey, uninitialised_pda_account()));
        }
        accounts.push(keyed_account_for_system_program());
        accounts.push((
            self.chain_registration_pubkey,
            chain_registration_account(self.chain, &self.emitter),
        ));
        accounts
    }

    fn submit(
        &self,
        mollusk: &Mollusk,
        starting_accounts: Vec<(Pubkey, Account)>,
    ) -> mollusk_svm::result::InstructionResult {
        let ix = Instruction::new_with_bytes(
            program_id(),
            &submit_vaas_ix_data(self.guardian_set_bump, &self.body),
            self.account_metas(),
        );
        mollusk.process_instruction(&ix, &starting_accounts)
    }
}

fn find_account<'a>(accounts: &'a [(Pubkey, Account)], key: &Pubkey) -> &'a Account {
    &accounts
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("account {key} not in result list"))
        .1
}

/// Assert the bucket is NoReplay-owned, 129 bytes, bit at `sequence % 1024` set.
fn assert_bucket_marked(bucket: &Account, sequence: u64) {
    assert_eq!(
        bucket.owner,
        Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        "bucket owned by solana_noreplay after MarkUsed"
    );
    let account =
        NoReplayBitmapAccount::from_bytes(&bucket.data).expect("bucket sized to bitmap layout");
    assert!(account.is_marked(sequence), "bitmap bit set");
}

/// Assert the bucket is uninitialised.
fn assert_bucket_unmarked(bucket: &Account) {
    assert_eq!(
        bucket.owner,
        system_program_id(),
        "bucket still system-owned"
    );
    assert!(bucket.data.is_empty(), "bucket still uninitialised");
}

/// Ethereum-native USDC to Solana: both balance PDAs credit, NoReplay flips.
#[test]
fn submit_vaas_transfer_commits_balances_and_opens_digest() {
    let mollusk = mollusk();
    let token_address = [0x77u8; 32];
    let scenario = Scenario::with_transfer_body(
        0xA0,
        500_000u128,
        2, // token_chain = Ethereum (token-native)
        token_address,
        1, // recipient_chain = Solana (wrapped destination)
    );

    let initial = scenario.initial_accounts();
    for pda in [scenario.source_account_pubkey, scenario.dest_account_pubkey] {
        let pre = find_account(&initial, &pda);
        assert_eq!(
            pre.owner,
            system_program_id(),
            "Account PDA starts system-owned"
        );
        assert!(pre.data.is_empty(), "Account PDA starts with zero data");
    }

    let result = scenario.submit(&mollusk, initial);
    assert!(
        matches!(result.program_result, ProgramResult::Success),
        "expected Success, got {:?}",
        result.program_result
    );

    let bucket = find_account(&result.resulting_accounts, &scenario.noreplay_bucket_pubkey);
    assert_bucket_marked(bucket, scenario.sequence);

    // Mollusk hides program logs; the surfpool e2e suite checks the commit log.

    // Source (native): credit.
    let src = find_account(&result.resulting_accounts, &scenario.source_account_pubkey);
    assert_eq!(
        src.owner,
        program_id(),
        "source Account PDA owned by program"
    );
    let src_layout: &BalanceAccountLayout = bytemuck::from_bytes(&src.data);
    assert_eq!(src_layout.balance, Uint256::from_u128(500_000));

    // Dest (wrapped): credit.
    let dst = find_account(&result.resulting_accounts, &scenario.dest_account_pubkey);
    assert_eq!(dst.owner, program_id(), "dest Account PDA owned by program");
    let dst_layout: &BalanceAccountLayout = bytemuck::from_bytes(&dst.data);
    assert_eq!(dst_layout.balance, Uint256::from_u128(500_000));
}

/// Second `submit_vaas` for the same `(chain, emitter, sequence)`: `AlreadyAccounted`.
#[test]
fn submit_vaas_rejects_duplicate_after_noreplay_set() {
    let mollusk = mollusk();
    let token_address = [0x99u8; 32];
    let scenario = Scenario::with_transfer_body(0xA1, 100u128, 2, token_address, 1);

    let first = scenario.submit(&mollusk, scenario.initial_accounts());
    assert!(matches!(first.program_result, ProgramResult::Success));

    let second = scenario.submit(&mollusk, first.resulting_accounts.clone());
    match second.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::AlreadyAccounted as u32,
                "expected AlreadyAccounted, got {code:?}"
            );
        }
        other => panic!("expected Failure(AlreadyAccounted), got {other:?}"),
    }
}

/// Attest commits but touches neither balance PDA.
#[test]
fn submit_vaas_with_attest_payload_skips_balance_work_but_marks_replay() {
    let mollusk = mollusk();
    let scenario = Scenario::new(0xA2);

    let result = scenario.submit(&mollusk, scenario.initial_accounts());
    assert!(
        matches!(result.program_result, ProgramResult::Success),
        "attest submit must succeed, got {:?}",
        result.program_result
    );

    let bucket = find_account(&result.resulting_accounts, &scenario.noreplay_bucket_pubkey);
    assert_bucket_marked(bucket, scenario.sequence);

    assert_eq!(
        scenario.source_account_pubkey,
        scenario.noreplay_authority_pubkey
    );
    let sentinel = find_account(
        &result.resulting_accounts,
        &scenario.noreplay_authority_pubkey,
    );
    assert_eq!(
        sentinel.owner,
        system_program_id(),
        "sentinel slot stays system-owned across attest commit"
    );
    assert!(sentinel.data.is_empty());
}

/// Unknown payload action: `UnknownTokenBridgePayload`; replay slot stays free.
#[test]
fn submit_vaas_with_unknown_payload_rejects_and_preserves_replay_slot() {
    let mollusk = mollusk();
    let mut scenario = Scenario::new(0xA7);
    scenario.body[51] = 0x05; // unknown Token Bridge action byte
    scenario.digest = double_keccak256_host(&scenario.body);

    let result = scenario.submit(&mollusk, scenario.initial_accounts());
    match result.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::UnknownTokenBridgePayload as u32,
                "expected UnknownTokenBridgePayload, got code {code}"
            );
        }
        other => panic!("expected Failure(UnknownTokenBridgePayload), got {other:?}"),
    }

    let bucket = find_account(&result.resulting_accounts, &scenario.noreplay_bucket_pubkey);
    assert_bucket_unmarked(bucket);
}

/// Pre-marked NoReplay bucket: `AlreadyAccounted` before any state change.
#[test]
fn submit_vaas_with_pre_marked_noreplay_rejects_before_state_mutation() {
    let mollusk = mollusk();
    let scenario = Scenario::new(0xA3);

    let mut accounts = scenario.initial_accounts();
    for (key, acct) in accounts.iter_mut() {
        if *key == scenario.noreplay_bucket_pubkey {
            *acct = noreplay_bucket_marked(scenario.sequence);
        }
    }

    let result = scenario.submit(&mollusk, accounts);
    match result.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::AlreadyAccounted as u32,
                "expected AlreadyAccounted, got {code:?}"
            );
        }
        other => panic!("expected Failure(AlreadyAccounted), got {other:?}"),
    }
}

/// Wrapped-source debit above the balance: `BalanceUnderflow`, NoReplay unset.
#[test]
fn submit_vaas_with_balance_underflow_reverts() {
    let mollusk = mollusk();
    let token_address = [0x88u8; 32];
    // Solana sends Ethereum-native USDC back; wrapped source starts at zero.
    let mut scenario = Scenario::with_transfer_body(0xA5, 1_000u128, 2, token_address, 2);
    scenario.chain = 1;
    scenario.body = build_transfer_body(
        scenario.chain,
        &scenario.emitter,
        scenario.sequence,
        1_000,
        2,
        &token_address,
        2,
    );
    scenario.digest = double_keccak256_host(&scenario.body);
    let (src, _) = derive_account_pda(1, 2, &token_address);
    let (dst, _) = derive_account_pda(2, 2, &token_address);
    let (registration_pda, _) = derive_chain_registration_pda(scenario.chain);
    scenario.chain_registration_pubkey = registration_pda;
    scenario.noreplay_bucket_pubkey = derive_canonical_noreplay_bucket(
        &scenario.noreplay_authority_pubkey,
        scenario.chain,
        &scenario.emitter,
        scenario.sequence,
    );
    scenario.source_account_pubkey = src;
    scenario.dest_account_pubkey = dst;

    let result = scenario.submit(&mollusk, scenario.initial_accounts());
    match result.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::BalanceUnderflow as u32,
                "expected BalanceUnderflow, got {code:?}"
            );
        }
        other => panic!("expected Failure(BalanceUnderflow), got {other:?}"),
    }
    let bucket = find_account(&result.resulting_accounts, &scenario.noreplay_bucket_pubkey);
    assert_bucket_unmarked(bucket);
}

/// No registration PDA for `emitter_chain`: `MissingChainRegistration`.
#[test]
fn submit_vaas_rejects_unregistered_chain() {
    let mollusk = mollusk();
    let token_address = [0x99u8; 32];
    let scenario = Scenario::with_transfer_body(0xA7, 100u128, 2, token_address, 1);

    let mut accounts = scenario.initial_accounts();
    for entry in accounts.iter_mut() {
        if entry.0 == scenario.chain_registration_pubkey {
            entry.1 = uninitialised_pda_account();
            break;
        }
    }
    let result = scenario.submit(&mollusk, accounts);
    match result.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::MissingChainRegistration as u32,
                "expected MissingChainRegistration, got {code:?}"
            );
        }
        other => panic!("expected Failure(MissingChainRegistration), got {other:?}"),
    }
}

/// Registration holds another emitter: `UnregisteredEmitter`.
#[test]
fn submit_vaas_rejects_wrong_emitter_for_registered_chain() {
    let mollusk = mollusk();
    let token_address = [0x99u8; 32];
    let scenario = Scenario::with_transfer_body(0xA8, 100u128, 2, token_address, 1);

    let mut accounts = scenario.initial_accounts();
    for entry in accounts.iter_mut() {
        if entry.0 == scenario.chain_registration_pubkey {
            entry.1 = chain_registration_account(scenario.chain, &[0xCC; 32]);
            break;
        }
    }
    let result = scenario.submit(&mollusk, accounts);
    match result.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::UnregisteredEmitter as u32,
                "expected UnregisteredEmitter, got {code:?}"
            );
        }
        other => panic!("expected Failure(UnregisteredEmitter), got {other:?}"),
    }
}

/// Body of 51 bytes: `InvalidInstructionData` from the `body_len <= VaaBodyHeader::LEN` gate.
#[test]
fn submit_vaas_with_short_body_rejects() {
    let mollusk = mollusk();
    let scenario = Scenario::new(0xA6);

    let short_body = vec![0u8; 50];
    let mut wire = Vec::with_capacity(1 + 1 + 2 + short_body.len());
    wire.push(IxDiscriminator::SubmitVaas as u8);
    wire.push(scenario.guardian_set_bump);
    wire.extend_from_slice(&(short_body.len() as u16).to_le_bytes());
    wire.extend_from_slice(&short_body);
    let ix = Instruction::new_with_bytes(program_id(), &wire, scenario.account_metas());
    let result = mollusk.process_instruction(&ix, &scenario.initial_accounts());
    match result.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidInstructionData as u32,
                "expected InvalidInstructionData, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidInstructionData), got {other:?}"),
    }
}

/// `body_len` prefix larger than the bytes present: `InvalidInstructionData` from the
/// `data.len() != FIXED_LEN + body_len` gate.
#[test]
fn submit_vaas_with_declared_len_mismatch_rejects() {
    let mollusk = mollusk();
    let scenario = Scenario::new(0xA9);

    // Valid 52-byte attest body; length prefix overstates by one.
    let body = build_attest_body(scenario.chain, &scenario.emitter, scenario.sequence);
    let declared_len = (body.len() + 1) as u16;
    let mut wire = Vec::with_capacity(1 + 1 + 2 + body.len());
    wire.push(IxDiscriminator::SubmitVaas as u8);
    wire.push(scenario.guardian_set_bump);
    wire.extend_from_slice(&declared_len.to_le_bytes());
    wire.extend_from_slice(&body);
    let ix = Instruction::new_with_bytes(program_id(), &wire, scenario.account_metas());
    let result = mollusk.process_instruction(&ix, &scenario.initial_accounts());
    match result.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidInstructionData as u32,
                "expected InvalidInstructionData, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidInstructionData), got {other:?}"),
    }
}

/// Transfer payload shorter than 133 bytes passes the length gates, Shim, NoReplay,
/// and registration checks. The payload parser then fails with `InvalidInstructionData`.
#[test]
fn submit_vaas_with_truncated_transfer_payload_rejects() {
    let mollusk = mollusk();
    let scenario = Scenario::new(0xAA);

    let mut body = vec![0u8; 60];
    body[8..10].copy_from_slice(&scenario.chain.to_be_bytes());
    body[10..42].copy_from_slice(&scenario.emitter);
    body[42..50].copy_from_slice(&scenario.sequence.to_be_bytes());
    body[51] = 0x01; // transfer action; payload is truncated after this
    let digest = double_keccak256_host(&body);

    // Re-sign so the Shim CPI passes.
    let mut scenario = scenario;
    scenario.body = body;
    scenario.digest = digest;

    let result = scenario.submit(&mollusk, scenario.initial_accounts());
    match result.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidInstructionData as u32,
                "expected InvalidInstructionData from the truncated transfer payload, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidInstructionData), got {other:?}"),
    }

    // Rolled back: bucket uninitialised.
    let bucket = find_account(&result.resulting_accounts, &scenario.noreplay_bucket_pubkey);
    assert_bucket_unmarked(bucket);
}
