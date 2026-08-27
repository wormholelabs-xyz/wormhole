//! Real `solana_noreplay` CPI wire format. Drives `submit_observations` to quorum. Asserts
//! the bucket is 129 bytes, NoReplay-owned, with bit `sequence % 1024` set.
//!
//! `SEQUENCE = 9` puts the bit outside byte 0.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        ChainRegistrationLayout, Instruction as IxDiscriminator, NoReplayBitmapAccount,
        CHAIN_REGISTRATION_SEED_PREFIX, NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITS_PER_BUCKET,
        NOREPLAY_PROGRAM_ID, PENDING_OBSERVATIONS_SEED_PREFIX, SUBMIT_OBSERVATION_PREFIX,
    },
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk},
    solana_account::Account,
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

mod common;
use common::guardian_fixtures::{
    guardian_set_account, make_guardians, sign_digest, Guardian, GUARDIAN_PUBKEY_LENGTH,
};
use common::mollusk_fixtures::{keyed_account_for_noreplay_program, mollusk_with_fixtures};

const PROGRAM_NAME: &str = "global_accountant";
/// Bit lands outside byte 0.
const SEQUENCE: u64 = 9;
const CHAIN: u16 = 2;
const GUARDIAN_SET_INDEX: u32 = 4;
const GUARDIAN_COUNT: usize = 19;
const QUORUM: u8 = 13;

fn program_id() -> Pubkey {
    Pubkey::new_from_array([7u8; 32])
}

fn system_program_id() -> Pubkey {
    keyed_account_for_system_program().0
}

fn core_bridge_program_id() -> Pubkey {
    Pubkey::new_from_array(global_accountant_definitions::CORE_BRIDGE_PROGRAM_ID)
}

fn derive_pending_pda(
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    digest: &[u8; 32],
) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    let sequence_be = sequence.to_be_bytes();
    Pubkey::find_program_address(
        &[
            PENDING_OBSERVATIONS_SEED_PREFIX,
            &chain_be,
            emitter,
            &sequence_be,
            digest,
        ],
        &program_id(),
    )
}

fn derive_chain_registration_pda(chain: u16) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    Pubkey::find_program_address(&[CHAIN_REGISTRATION_SEED_PREFIX, &chain_be], &program_id())
}

fn derive_noreplay_bucket_pda(
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

fn double_keccak256_host(body: &[u8]) -> [u8; 32] {
    let inner = solana_keccak_hasher::hashv(&[body]).to_bytes();
    solana_keccak_hasher::hashv(&[&inner]).to_bytes()
}

/// Fixed source-chain transaction id for the signing digest.
const TX_HASH: [u8; 32] = [0xA9_u8; 32];

/// Host mirror of `observation_signing_digest`.
fn signing_digest_for(body: &[u8]) -> [u8; 32] {
    solana_keccak_hasher::hashv(&[SUBMIT_OBSERVATION_PREFIX, &TX_HASH, body]).to_bytes()
}

/// 52-byte Attest body; slots 7/8 stay unused.
fn build_attest_body(chain: u16, emitter: &[u8; 32], sequence: u64) -> Vec<u8> {
    let mut body = vec![0u8; 52];
    body[8..10].copy_from_slice(&chain.to_be_bytes());
    body[10..42].copy_from_slice(emitter);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body[51] = 0x02;
    body
}

fn submit_ix_data(
    digest: &[u8; 32],
    guardian_set_index: u32,
    guardian_index: u8,
    signature: &[u8; 65],
    body: &[u8],
) -> Vec<u8> {
    // `tx_hash` follows the fixed prefix; no bump bytes on the wire.
    let mut data = Vec::with_capacity(1 + 102 + 32 + 2 + body.len());
    data.push(IxDiscriminator::SubmitObservations as u8);
    data.extend_from_slice(digest);
    data.extend_from_slice(&guardian_set_index.to_le_bytes());
    data.push(guardian_index);
    data.extend_from_slice(signature);
    data.extend_from_slice(&TX_HASH);
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

fn uninit_pda_account() -> Account {
    system_owned_account(0)
}

fn chain_registration_account(chain: u16, emitter: &[u8; 32]) -> Account {
    let mut layout: ChainRegistrationLayout = bytemuck::Zeroable::zeroed();
    layout.tag = ChainRegistrationLayout::TAG;
    layout.chain = chain;
    layout.emitter_address = *emitter;
    Account {
        lamports: 1_000_000,
        data: bytemuck::bytes_of(&layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

struct Scenario {
    emitter: [u8; 32],
    body: Vec<u8>,
    digest: [u8; 32],
    signing_digest: [u8; 32],
    pending_pda: Pubkey,
    noreplay_authority: Pubkey,
    noreplay_bucket: Pubkey,
    guardian_set_pubkey: Pubkey,
    chain_registration_pubkey: Pubkey,
    submitter: Pubkey,
    guardians: Vec<Guardian>,
}

impl Scenario {
    fn build() -> Self {
        let mut emitter = [0u8; 32];
        emitter[31] = 0x77;
        let body = build_attest_body(CHAIN, &emitter, SEQUENCE);
        let digest = double_keccak256_host(&body);
        let signing_digest = signing_digest_for(&body);
        let (pending_pda, _) = derive_pending_pda(CHAIN, &emitter, SEQUENCE, &digest);
        let (noreplay_authority, _) =
            Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
        let noreplay_bucket =
            derive_noreplay_bucket_pda(&noreplay_authority, CHAIN, &emitter, SEQUENCE);
        let (chain_registration_pubkey, _) = derive_chain_registration_pda(CHAIN);
        // The inline recovery path reads only guardian keys from this account.
        let guardian_set_pubkey = Pubkey::new_from_array([0xC1u8; 32]);
        let submitter = Pubkey::new_from_array([0x11u8; 32]);
        let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
        Self {
            emitter,
            body,
            digest,
            signing_digest,
            pending_pda,
            noreplay_authority,
            noreplay_bucket,
            guardian_set_pubkey,
            chain_registration_pubkey,
            submitter,
            guardians,
        }
    }

    fn account_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.submitter, true),
            AccountMeta::new(self.pending_pda, false),
            AccountMeta::new_readonly(self.guardian_set_pubkey, false),
            AccountMeta::new(self.noreplay_bucket, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(Pubkey::new_from_array(NOREPLAY_PROGRAM_ID), false),
            AccountMeta::new_readonly(self.noreplay_authority, false),
            AccountMeta::new(self.noreplay_authority, false), // source sentinel (attest)
            AccountMeta::new(self.noreplay_authority, false), // dest sentinel (attest)
            AccountMeta::new(self.submitter, false),          // rent recipient
            AccountMeta::new_readonly(self.chain_registration_pubkey, false),
        ]
    }

    fn initial_accounts(&self) -> Vec<(Pubkey, Account)> {
        let guardian_keys: Vec<[u8; GUARDIAN_PUBKEY_LENGTH]> =
            self.guardians.iter().map(|g| g.eth_address).collect();
        vec![
            (self.submitter, system_owned_account(50_000_000_000)),
            (self.pending_pda, uninit_pda_account()),
            (
                self.guardian_set_pubkey,
                guardian_set_account(
                    GUARDIAN_SET_INDEX,
                    &guardian_keys,
                    0,
                    0,
                    &core_bridge_program_id(),
                ),
            ),
            // The CPI allocates the bucket on `MarkUsed`.
            (self.noreplay_bucket, uninit_pda_account()),
            keyed_account_for_system_program(),
            // Slot 5 needs a Loader-V3 executable entry.
            keyed_account_for_noreplay_program(),
            (self.noreplay_authority, system_owned_account(0)),
            // Slots 7/8 unused for Attest.
            (
                self.chain_registration_pubkey,
                chain_registration_account(CHAIN, &self.emitter),
            ),
        ]
    }

    fn submit_once(
        &self,
        mollusk: &Mollusk,
        accounts: Vec<(Pubkey, Account)>,
        guardian_index: u8,
    ) -> mollusk_svm::result::InstructionResult {
        let guardian = &self.guardians[guardian_index as usize];
        let signature = sign_digest(guardian, &self.signing_digest);
        let ix = Instruction::new_with_bytes(
            program_id(),
            &submit_ix_data(
                &self.digest,
                GUARDIAN_SET_INDEX,
                guardian_index,
                &signature,
                &self.body,
            ),
            self.account_metas(),
        );
        mollusk.process_instruction(&ix, &accounts)
    }
}

/// Quorum sets the real NoReplay bitmap bit.
#[test]
fn submit_observations_quorum_marks_real_noreplay_bitmap() {
    let mollusk = mollusk_with_fixtures(&program_id(), PROGRAM_NAME);
    let scenario = Scenario::build();

    let mut accounts = scenario.initial_accounts();
    for i in 0..QUORUM {
        let result = scenario.submit_once(&mollusk, accounts.clone(), i);
        assert!(
            matches!(result.program_result, ProgramResult::Success),
            "submit #{i} expected success, got {:?}",
            result.program_result
        );
        accounts = result.resulting_accounts;
    }

    let bucket = accounts
        .iter()
        .find(|(k, _)| *k == scenario.noreplay_bucket)
        .map(|(_, a)| a.clone())
        .expect("noreplay bucket in resulting accounts");

    assert_eq!(
        bucket.owner,
        Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        "noreplay bucket must be owned by solana_noreplay after MarkUsed"
    );
    let account = NoReplayBitmapAccount::from_bytes(&bucket.data)
        .expect("noreplay bucket must be 129 bytes ([bump: u8][bitmap: 128 B])");
    let bit_index = (SEQUENCE % NOREPLAY_BITS_PER_BUCKET) as usize;
    assert!(
        account.is_marked(SEQUENCE),
        "bit {bit_index} must be set after MarkUsed"
    );
}
