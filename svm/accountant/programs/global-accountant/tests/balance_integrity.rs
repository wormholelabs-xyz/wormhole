//! Balance-applicator integrity tests through `submit_vaas` and `modify_balance`, with the
//! real `solana_noreplay.so` and `wormhole_verify_vaa_shim.so`.
//!
//! Covers `transfer::apply_transfer` invariants: the same-PDA collapse (success and
//! transient underflow), rollback after a destination failure, and `modify_balance` Add
//! overflow. Each case asserts on-chain state. Each failure originates in the balance applicator.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        BalanceAccountLayout, GlobalAccountantError, Instruction as IxDiscriminator,
        NoReplayBitmapAccount, Uint256, ACCOUNTANT_GOVERNANCE_MODULE, ACCOUNT_SEED_PREFIX,
        CHAIN_REGISTRATION_SEED_PREFIX, CORE_BRIDGE_PROGRAM_ID, GOVERNANCE_EMITTER,
        MODIFICATION_SEED_PREFIX, MODIFY_BALANCE_ACTION, NOREPLAY_AUTHORITY_SEED_PREFIX,
        NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID, SOLANA_CHAIN_ID, VERIFY_VAA_SHIM_PROGRAM_ID,
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

fn derive_modification_pda(sequence: u64) -> (Pubkey, u8) {
    let seq_be = sequence.to_be_bytes();
    Pubkey::find_program_address(&[MODIFICATION_SEED_PREFIX, &seq_be], &program_id())
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
        &noreplay_program_id(),
    );
    pda
}

/// Dedup digest `keccak256(keccak256(body))`.
fn double_keccak256_host(body: &[u8]) -> [u8; 32] {
    let inner = solana_keccak_hasher::hashv(&[body]).to_bytes();
    solana_keccak_hasher::hashv(&[&inner]).to_bytes()
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

/// Program-owned `BalanceAccount` PDA with `balance`.
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

fn chain_registration_account(chain: u16, emitter_address: &[u8; 32]) -> Account {
    use global_accountant_definitions::ChainRegistrationLayout;
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

fn real_guardian_set_account(guardians: &[Guardian]) -> Account {
    let keys: Vec<[u8; GUARDIAN_PUBKEY_LENGTH]> = guardians.iter().map(|g| g.eth_address).collect();
    guardian_set_account(GUARDIAN_SET_INDEX, &keys, 0, 0, &core_bridge_program_id())
}

fn find_account<'a>(accounts: &'a [(Pubkey, Account)], key: &Pubkey) -> &'a Account {
    &accounts
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("account {key} not in result list"))
        .1
}

fn balance_of(accounts: &[(Pubkey, Account)], key: &Pubkey) -> Uint256 {
    let acct = find_account(accounts, key);
    let layout: &BalanceAccountLayout = bytemuck::from_bytes(&acct.data);
    layout.balance
}

/// Assert the NoReplay bucket is uninitialised (tx rolled back).
fn assert_bucket_unmarked(bucket: &Account) {
    assert_eq!(
        bucket.owner,
        system_program_id(),
        "bucket still system-owned"
    );
    assert!(bucket.data.is_empty(), "bucket still uninitialised");
}

fn assert_bucket_marked(bucket: &Account, sequence: u64) {
    assert_eq!(
        bucket.owner,
        noreplay_program_id(),
        "bucket owned by noreplay"
    );
    let account = NoReplayBitmapAccount::from_bytes(&bucket.data).expect("129-byte bitmap account");
    assert!(account.is_marked(sequence), "bitmap bit set");
}

fn extract_code(result: &ProgramResult) -> u32 {
    match result {
        ProgramResult::Failure(err) => u64::from(err.clone()) as u32,
        other => panic!("expected Failure, got {other:?}"),
    }
}

/// `submit_vaas` Transfer scenario. Shim, registration, and NoReplay checks pass, so
/// any failure comes from the balance applicator.
struct TransferCase {
    emitter_chain: u16,
    emitter: [u8; 32],
    sequence: u64,
    amount: u128,
    token_chain: u16,
    token_address: [u8; 32],
    recipient_chain: u16,
    /// Override for the dest PDA meta; injects a wrong dest PDA.
    dest_override: Option<Pubkey>,
    /// Pre-funded state for the source / dest balance PDAs.
    source_prefund: Option<Uint256>,
    dest_prefund: Option<Uint256>,
}

impl TransferCase {
    fn body(&self) -> Vec<u8> {
        build_transfer_body(
            self.emitter_chain,
            &self.emitter,
            self.sequence,
            self.amount,
            self.token_chain,
            &self.token_address,
            self.recipient_chain,
        )
    }

    fn source_pubkey(&self) -> Pubkey {
        derive_account_pda(self.emitter_chain, self.token_chain, &self.token_address).0
    }

    fn dest_pubkey(&self) -> Pubkey {
        self.dest_override.unwrap_or_else(|| {
            derive_account_pda(self.recipient_chain, self.token_chain, &self.token_address).0
        })
    }

    fn run(
        &self,
        mollusk: &Mollusk,
    ) -> (
        mollusk_svm::result::InstructionResult,
        Pubkey,
        Pubkey,
        Pubkey,
        Vec<AccountMeta>,
    ) {
        let body = self.body();
        let digest = double_keccak256_host(&body);

        let submitter = Pubkey::new_from_array([0x11u8; 32]);
        let (noreplay_authority, _) =
            Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
        let (chain_registration, _) = derive_chain_registration_pda(self.emitter_chain);
        let noreplay_bucket = derive_canonical_noreplay_bucket(
            &noreplay_authority,
            self.emitter_chain,
            &self.emitter,
            self.sequence,
        );
        let (guardian_set, guardian_set_bump) =
            derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
        let guardian_signatures = Pubkey::new_from_array([0xC5u8; 32]);
        let guardians = make_guardians(GUARDIAN_COUNT, 0x42);

        let source_pubkey = self.source_pubkey();
        let dest_pubkey = self.dest_pubkey();

        let metas = vec![
            AccountMeta::new(submitter, true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(guardian_set, false),
            AccountMeta::new_readonly(guardian_signatures, false),
            AccountMeta::new(noreplay_bucket, false),
            AccountMeta::new_readonly(noreplay_program_id(), false),
            AccountMeta::new_readonly(noreplay_authority, false),
            AccountMeta::new(source_pubkey, false),
            AccountMeta::new(dest_pubkey, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(chain_registration, false),
        ];

        let mut accounts = vec![
            (submitter, system_owned_account(50_000_000_000)),
            keyed_account_for_verify_vaa_shim_program(),
            (guardian_set, real_guardian_set_account(&guardians)),
            (
                guardian_signatures,
                real_guardian_signatures_account(&digest, &submitter, &guardians),
            ),
            (noreplay_bucket, system_owned_account(0)),
            keyed_account_for_noreplay_program(),
            (noreplay_authority, system_owned_account(0)),
        ];

        let source_state = match self.source_prefund {
            Some(bal) => balance_account(
                self.emitter_chain,
                self.token_chain,
                &self.token_address,
                bal,
            ),
            None => uninitialised_pda_account(),
        };
        accounts.push((source_pubkey, source_state));

        // Dest slot; omitted when it equals the source PDA.
        if dest_pubkey != source_pubkey {
            let dest_state = match self.dest_prefund {
                Some(bal) => balance_account(
                    self.recipient_chain,
                    self.token_chain,
                    &self.token_address,
                    bal,
                ),
                None => uninitialised_pda_account(),
            };
            accounts.push((dest_pubkey, dest_state));
        }

        accounts.push(keyed_account_for_system_program());
        accounts.push((
            chain_registration,
            chain_registration_account(self.emitter_chain, &self.emitter),
        ));

        let mut data = Vec::with_capacity(1 + 1 + 2 + body.len());
        data.push(IxDiscriminator::SubmitVaas as u8);
        data.push(guardian_set_bump);
        data.extend_from_slice(&(body.len() as u16).to_le_bytes());
        data.extend_from_slice(&body);

        let ix = Instruction::new_with_bytes(program_id(), &data, metas.clone());
        let result = mollusk.process_instruction(&ix, &accounts);
        (result, source_pubkey, dest_pubkey, noreplay_bucket, metas)
    }
}

/// Same-chain native self-transfer: source and dest are one PDA. `lock_or_burn` credits.
/// `unlock_or_mint` debits. The balance returns to its start value.
#[test]
fn self_transfer_native_collapse_nets_to_prefunded_balance() {
    let mollusk = mollusk();
    let token_address = [0x71u8; 32];
    let case = TransferCase {
        emitter_chain: 2,
        emitter: {
            let mut e = [0u8; 32];
            e[0] = 0xB0;
            e[31] = 0x77;
            e
        },
        sequence: 0x42,
        amount: 500_000,
        token_chain: 2, // native: chain == token_chain
        token_address,
        recipient_chain: 2, // same chain ⇒ same PDA
        dest_override: None,
        source_prefund: Some(Uint256::from_u128(1_000_000)),
        dest_prefund: None,
    };

    let (result, source, dest, bucket, metas) = case.run(&mollusk);
    assert!(
        matches!(result.program_result, ProgramResult::Success),
        "native self-transfer must succeed, got {:?}",
        result.program_result
    );

    assert_eq!(source, dest, "same-chain transfer collapses to one PDA");
    assert_eq!(
        metas[7].pubkey, metas[8].pubkey,
        "source and dest metas point at the same pubkey"
    );

    // +500_000 then -500_000 over 1_000_000.
    assert_eq!(
        balance_of(&result.resulting_accounts, &source),
        Uint256::from_u128(1_000_000),
        "collapse applied both ops to one layout: 1_000_000 + 500_000 - 500_000"
    );

    assert_bucket_marked(
        find_account(&result.resulting_accounts, &bucket),
        case.sequence,
    );
}

/// Same-chain wrapped self-transfer: `lock_or_burn` debits first. Balance below `amount`
/// fails with `BalanceUnderflow` and the NoReplay bit stays clear.
#[test]
fn self_transfer_wrapped_collapse_transient_underflow_rejects() {
    let mollusk = mollusk();
    let token_address = [0x72u8; 32];
    let case = TransferCase {
        emitter_chain: 2,
        emitter: {
            let mut e = [0u8; 32];
            e[0] = 0xB1;
            e[31] = 0x77;
            e
        },
        sequence: 0x43,
        amount: 1_000,
        token_chain: 3, // wrapped: chain (2) != token_chain (3)
        token_address,
        recipient_chain: 2, // same chain ⇒ same PDA
        dest_override: None,
        // Wrapped balance 100 < amount 1_000.
        source_prefund: Some(Uint256::from_u128(100)),
        dest_prefund: None,
    };

    let (result, source, dest, bucket, _) = case.run(&mollusk);
    assert_eq!(source, dest, "same-chain transfer collapses to one PDA");
    assert_eq!(
        extract_code(&result.program_result),
        GlobalAccountantError::BalanceUnderflow as u32,
        "transient burn must underflow on the wrapped collapse, got {:?}",
        result.program_result
    );

    assert_bucket_unmarked(find_account(&result.resulting_accounts, &bucket));
    assert_eq!(
        balance_of(&result.resulting_accounts, &source),
        Uint256::from_u128(100),
        "underflow rollback preserves the pre-funded balance"
    );
}

/// Destination overflow rolls back the source mutation. Native source credits; wrapped
/// dest at `MAX` overflows with `BalanceOverflow`. Source keeps its original balance.
#[test]
fn dest_overflow_rolls_back_source_mutation() {
    let mollusk = mollusk();
    let token_address = [0x73u8; 32];
    let source_initial = Uint256::from_u128(1_000);
    let case = TransferCase {
        emitter_chain: 2,
        emitter: {
            let mut e = [0u8; 32];
            e[0] = 0xB2;
            e[31] = 0x77;
            e
        },
        sequence: 0x44,
        amount: 500,
        token_chain: 2, // source native (chain 2 == token_chain 2): credit
        token_address,
        recipient_chain: 1, // dest wrapped (chain 1 != token_chain 2): credit
        dest_override: None,
        source_prefund: Some(source_initial),
        // Dest at MAX.
        dest_prefund: Some(Uint256::MAX),
    };

    let (result, source, dest, bucket, _) = case.run(&mollusk);
    assert_ne!(source, dest, "distinct source/dest PDAs");
    assert_eq!(
        extract_code(&result.program_result),
        GlobalAccountantError::BalanceOverflow as u32,
        "dest mint must overflow, got {:?}",
        result.program_result
    );

    // Source unchanged.
    assert_eq!(
        balance_of(&result.resulting_accounts, &source),
        source_initial,
        "source mutation rolled back on dest overflow"
    );
    assert_eq!(
        balance_of(&result.resulting_accounts, &dest),
        Uint256::MAX,
        "dest unchanged after overflow rollback"
    );
    assert_bucket_unmarked(find_account(&result.resulting_accounts, &bucket));
}

/// Wrong dest PDA (wrong `token_chain`) at slot 8 fails with `InvalidAccountPda` after
/// the source side ran. Source keeps its original balance.
#[test]
fn dest_invalid_account_pda_rolls_back_source() {
    let mollusk = mollusk();
    let token_address = [0x74u8; 32];
    let source_initial = Uint256::from_u128(2_000);
    // Expected dest is (recipient_chain=1, token_chain=2); pass token_chain 9.
    let wrong_dest = derive_account_pda(1, 9, &token_address).0;
    let case = TransferCase {
        emitter_chain: 2,
        emitter: {
            let mut e = [0u8; 32];
            e[0] = 0xB3;
            e[31] = 0x77;
            e
        },
        sequence: 0x45,
        amount: 500,
        token_chain: 2, // source native: credit succeeds
        token_address,
        recipient_chain: 1,
        dest_override: Some(wrong_dest),
        source_prefund: Some(source_initial),
        dest_prefund: None,
    };

    let (result, source, dest, bucket, _) = case.run(&mollusk);
    assert_ne!(source, dest, "wrong dest PDA differs from source");
    assert_eq!(
        extract_code(&result.program_result),
        GlobalAccountantError::InvalidAccountPda as u32,
        "wrong dest PDA must fail the canonical-address check, got {:?}",
        result.program_result
    );

    assert_eq!(
        balance_of(&result.resulting_accounts, &source),
        source_initial,
        "source unchanged after dest InvalidAccountPda"
    );
    assert_bucket_unmarked(find_account(&result.resulting_accounts, &bucket));
}

/// `modify_balance` Add overflow leaves the balance at `MAX - 1`.
#[test]
fn modify_balance_add_overflow_leaves_balance_unchanged() {
    let mollusk = mollusk();
    let token_address = [0x75u8; 32];
    let payload_sequence: u64 = 700;

    // MAX - 1 plus 2 overflows.
    let mut max_minus_one = [0xFFu8; 32];
    max_minus_one[31] = 0xFE;
    let pre_balance_value = Uint256(max_minus_one);

    let body = build_modify_balance_body(
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        0x30,
        &ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        SOLANA_CHAIN_ID,
        payload_sequence,
        2,
        2,
        &token_address,
        1, // Add
        Uint256::from_u128(2),
        &[0u8; 32],
    );
    let digest = double_keccak256_host(&body);

    let (balance_pda, _) = derive_account_pda(2, 2, &token_address);
    let (modification_pda, _) = derive_modification_pda(payload_sequence);
    let payer = Pubkey::new_from_array([0x11u8; 32]);
    let guardian_signatures = Pubkey::new_from_array([0xC3u8; 32]);
    let (guardian_set, guardian_set_bump) =
        derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
    let guardians = make_guardians(GUARDIAN_COUNT, 0x42);

    let accounts = vec![
        (payer, system_owned_account(50_000_000_000)),
        keyed_account_for_verify_vaa_shim_program(),
        (guardian_set, real_guardian_set_account(&guardians)),
        (
            guardian_signatures,
            real_guardian_signatures_account(&digest, &payer, &guardians),
        ),
        (
            balance_pda,
            balance_account(2, 2, &token_address, pre_balance_value),
        ),
        keyed_account_for_system_program(),
        (modification_pda, uninitialised_pda_account()),
    ];
    let metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new_readonly(shim_program_id(), false),
        AccountMeta::new_readonly(guardian_set, false),
        AccountMeta::new_readonly(guardian_signatures, false),
        AccountMeta::new(balance_pda, false),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new(modification_pda, false),
    ];

    let mut data = Vec::with_capacity(1 + 1 + 2 + body.len());
    data.push(IxDiscriminator::ModifyBalance as u8);
    data.push(guardian_set_bump);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(&body);

    let ix = Instruction::new_with_bytes(program_id(), &data, metas);
    let result = mollusk.process_instruction(&ix, &accounts);

    assert_eq!(
        extract_code(&result.program_result),
        GlobalAccountantError::ModifyBalanceOverflow as u32,
        "Add over MAX-1 must overflow, got {:?}",
        result.program_result
    );

    assert_eq!(
        balance_of(&result.resulting_accounts, &balance_pda),
        pre_balance_value,
        "overflow rollback leaves the original balance intact"
    );
}
