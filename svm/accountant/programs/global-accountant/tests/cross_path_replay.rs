//! Cross-path replay invariant: a `(chain, emitter, sequence)` accounted via one
//! submission path must be rejected as `AlreadyAccounted` on the other.
//!
//! Both `submit_observations` (quorum tracker) and `submit_vaas` (signed-VAA
//! backfill) share a single NoReplay bucket PDA, derived identically from
//! `(noreplay_authority, chain, emitter, sequence)` under the same program id.
//! Once either path marks that slot, the other must refuse the same triple. The
//! two pre-existing replay tests are same-path only; this file pins the
//! double-spend invariant across the path boundary in both directions.
//!
//! Driven against a Mollusk with the real `solana_noreplay.so` and
//! `wormhole_verify_vaa_shim.so` loaded (see `common::mollusk_fixtures`).

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        ChainRegistrationLayout, GlobalAccountantError, Instruction as IxDiscriminator,
        ACCOUNT_SEED_PREFIX, CHAIN_REGISTRATION_SEED_PREFIX, CORE_BRIDGE_PROGRAM_ID,
        NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID,
        PENDING_OBSERVATIONS_SEED_PREFIX, PendingObservationsLayout, VERIFY_VAA_SHIM_PROGRAM_ID,
    },
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk},
    solana_account::Account,
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

mod common;
use common::guardian_fixtures::{
    derive_guardian_set_pda, guardian_set_account as core_bridge_guardian_set_account,
    guardian_signatures_account, make_guardians, sign_digest, Guardian, GUARDIAN_PUBKEY_LENGTH,
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

// ----------------------------------------------------------------------------
// Shared derivations and fixtures
// ----------------------------------------------------------------------------

fn double_keccak256_host(body: &[u8]) -> [u8; 32] {
    let inner = solana_keccak_hasher::hashv(&[body]).to_bytes();
    solana_keccak_hasher::hashv(&[&inner]).to_bytes()
}

fn derive_chain_registration_pda(chain: u16) -> Pubkey {
    let chain_be = chain.to_be_bytes();
    Pubkey::find_program_address(&[CHAIN_REGISTRATION_SEED_PREFIX, &chain_be], &program_id()).0
}

fn derive_pending_pda(chain: u16, emitter: &[u8; 32], sequence: u64, digest: &[u8; 32]) -> Pubkey {
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
    .0
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

/// Transfer VAA body (Token Bridge action 0x01), shared shape across paths.
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

fn find_account<'a>(accounts: &'a [(Pubkey, Account)], key: &Pubkey) -> &'a Account {
    &accounts
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("account {key} not in result list"))
        .1
}

/// Replace `key`'s entry in `accounts` with `state` (or push it if absent).
fn upsert(accounts: &mut Vec<(Pubkey, Account)>, key: Pubkey, state: Account) {
    if let Some(entry) = accounts.iter_mut().find(|(k, _)| *k == key) {
        entry.1 = state;
    } else {
        accounts.push((key, state));
    }
}

// ----------------------------------------------------------------------------
// submit_observations driver (inline-secp guardian set)
// ----------------------------------------------------------------------------

/// Core-Bridge `GuardianSet` account for the inline secp path: raw 20-byte keys,
/// never-expiring, owned by the Core Bridge.
fn obs_guardian_set_account(guardians: &[Guardian]) -> Account {
    let keys: Vec<[u8; GUARDIAN_PUBKEY_LENGTH]> = guardians.iter().map(|g| g.eth_address).collect();
    core_bridge_guardian_set_account(GUARDIAN_SET_INDEX, &keys, 0, 0, &core_bridge_program_id())
}

/// Build `submit_observations` instruction data in the TARGET derived-digest
/// wire format: discriminator + 4-byte gsi (LE) + 1-byte guardian index +
/// 65-byte signature + 2-byte body len (LE) + body. The on-chain digest is
/// re-derived from the body; no digest travels in the instruction data.
fn submit_observations_ix_data(
    guardian_index: u8,
    signature: &[u8; 65],
    body: &[u8],
) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 4 + 1 + 65 + 2 + body.len());
    data.push(IxDiscriminator::SubmitObservations as u8);
    data.extend_from_slice(&GUARDIAN_SET_INDEX.to_le_bytes());
    data.push(guardian_index);
    data.extend_from_slice(signature);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
    data
}

struct ObsCtx {
    chain: u16,
    emitter: [u8; 32],
    #[allow(dead_code)] // retained for readability; derivations consume it in `new`.
    sequence: u64,
    body: Vec<u8>,
    digest: [u8; 32],
    guardians: Vec<Guardian>,
    submitter: Pubkey,
    pending_pda: Pubkey,
    guardian_set_pubkey: Pubkey,
    noreplay_bucket: Pubkey,
    noreplay_authority: Pubkey,
    source_account: Pubkey,
    dest_account: Pubkey,
    chain_registration: Pubkey,
}

impl ObsCtx {
    fn new(chain: u16, emitter: [u8; 32], sequence: u64, body: Vec<u8>, token_chain: u16, token_address: &[u8; 32], recipient_chain: u16) -> Self {
        let digest = double_keccak256_host(&body);
        let noreplay_authority = derive_noreplay_authority();
        // Canonical GuardianSet PDA (Core Bridge): the observation path pins the
        // address, not just the owner, so a placeholder pubkey is rejected.
        let (guardian_set_pubkey, _) =
            derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
        Self {
            chain,
            emitter,
            sequence,
            body,
            digest,
            guardians: make_guardians(GUARDIAN_COUNT, 0x42),
            submitter: Pubkey::new_from_array([0x11u8; 32]),
            pending_pda: derive_pending_pda(chain, &emitter, sequence, &digest),
            guardian_set_pubkey,
            noreplay_bucket: derive_noreplay_bucket(&noreplay_authority, chain, &emitter, sequence),
            noreplay_authority,
            source_account: derive_account_pda(chain, token_chain, token_address),
            dest_account: derive_account_pda(recipient_chain, token_chain, token_address),
            chain_registration: derive_chain_registration_pda(chain),
        }
    }

    fn metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.submitter, true),
            AccountMeta::new(self.pending_pda, false),
            AccountMeta::new_readonly(self.guardian_set_pubkey, false),
            AccountMeta::new(self.noreplay_bucket, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(noreplay_program_id(), false),
            AccountMeta::new_readonly(self.noreplay_authority, false),
            AccountMeta::new(self.source_account, false),
            AccountMeta::new(self.dest_account, false),
            AccountMeta::new(self.submitter, false),
            AccountMeta::new_readonly(self.chain_registration, false),
        ]
    }

    fn initial_accounts(&self) -> Vec<(Pubkey, Account)> {
        let mut accounts = vec![
            (self.submitter, system_owned_account(50_000_000_000)),
            (self.pending_pda, uninitialised_pda_account()),
            (self.guardian_set_pubkey, obs_guardian_set_account(&self.guardians)),
            (self.noreplay_bucket, system_owned_account(0)),
            keyed_account_for_system_program(),
            keyed_account_for_noreplay_program(),
            (self.noreplay_authority, system_owned_account(0)),
            (self.source_account, uninitialised_pda_account()),
        ];
        if self.dest_account != self.source_account {
            accounts.push((self.dest_account, uninitialised_pda_account()));
        }
        accounts.push((self.chain_registration, chain_registration_account(self.chain, &self.emitter)));
        accounts
    }

    fn submit_once(
        &self,
        mollusk: &Mollusk,
        accounts: Vec<(Pubkey, Account)>,
        guardian_index: u8,
    ) -> mollusk_svm::result::InstructionResult {
        let signature = sign_digest(&self.guardians[guardian_index as usize], &self.digest);
        let ix = Instruction::new_with_bytes(
            program_id(),
            &submit_observations_ix_data(guardian_index, &signature, &self.body),
            self.metas(),
        );
        mollusk.process_instruction(&ix, &accounts)
    }

    /// Drive to quorum, returning the final resulting accounts (bucket marked).
    fn drive_to_quorum(&self, mollusk: &Mollusk) -> Vec<(Pubkey, Account)> {
        let mut accounts = self.initial_accounts();
        for i in 0..QUORUM {
            let r = self.submit_once(mollusk, accounts.clone(), i);
            assert!(
                matches!(r.program_result, ProgramResult::Success),
                "obs submit #{i} expected success, got {:?}",
                r.program_result
            );
            accounts = r.resulting_accounts;
        }
        accounts
    }
}

// ----------------------------------------------------------------------------
// submit_vaas driver (Shim CPI + posted GuardianSignatures)
// ----------------------------------------------------------------------------

fn submit_vaas_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 1 + 2 + body.len());
    data.push(IxDiscriminator::SubmitVaas as u8);
    data.push(guardian_set_bump);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
    data
}

struct VaasCtx {
    chain: u16,
    emitter: [u8; 32],
    #[allow(dead_code)] // retained for readability; derivations consume it in `new`.
    sequence: u64,
    body: Vec<u8>,
    digest: [u8; 32],
    guardians: Vec<Guardian>,
    guardian_set_bump: u8,
    submitter: Pubkey,
    guardian_set_pubkey: Pubkey,
    guardian_signatures_pubkey: Pubkey,
    noreplay_bucket: Pubkey,
    noreplay_authority: Pubkey,
    source_account: Pubkey,
    dest_account: Pubkey,
    chain_registration: Pubkey,
}

impl VaasCtx {
    fn new(chain: u16, emitter: [u8; 32], sequence: u64, body: Vec<u8>, token_chain: u16, token_address: &[u8; 32], recipient_chain: u16) -> Self {
        let digest = double_keccak256_host(&body);
        let noreplay_authority = derive_noreplay_authority();
        let (guardian_set_pubkey, guardian_set_bump) =
            derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
        Self {
            chain,
            emitter,
            sequence,
            body,
            digest,
            guardians: make_guardians(GUARDIAN_COUNT, 0x42),
            guardian_set_bump,
            submitter: Pubkey::new_from_array([0x11u8; 32]),
            guardian_set_pubkey,
            guardian_signatures_pubkey: Pubkey::new_from_array([0xC5u8; 32]),
            noreplay_bucket: derive_noreplay_bucket(&noreplay_authority, chain, &emitter, sequence),
            noreplay_authority,
            source_account: derive_account_pda(chain, token_chain, token_address),
            dest_account: derive_account_pda(recipient_chain, token_chain, token_address),
            chain_registration: derive_chain_registration_pda(chain),
        }
    }

    fn metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.submitter, true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(self.guardian_set_pubkey, false),
            AccountMeta::new_readonly(self.guardian_signatures_pubkey, false),
            AccountMeta::new(self.noreplay_bucket, false),
            AccountMeta::new_readonly(noreplay_program_id(), false),
            AccountMeta::new_readonly(self.noreplay_authority, false),
            AccountMeta::new(self.source_account, false),
            AccountMeta::new(self.dest_account, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(self.chain_registration, false),
        ]
    }

    fn signatures_account(&self) -> Account {
        let sigs: Vec<(u8, [u8; 65])> = (0..QUORUM)
            .map(|i| (i, sign_digest(&self.guardians[i as usize], &self.digest)))
            .collect();
        guardian_signatures_account(GUARDIAN_SET_INDEX, &self.submitter, &sigs, &shim_program_id())
    }

    fn initial_accounts(&self) -> Vec<(Pubkey, Account)> {
        let keys: Vec<[u8; GUARDIAN_PUBKEY_LENGTH]> =
            self.guardians.iter().map(|g| g.eth_address).collect();
        let mut accounts = vec![
            (self.submitter, system_owned_account(50_000_000_000)),
            keyed_account_for_verify_vaa_shim_program(),
            (
                self.guardian_set_pubkey,
                core_bridge_guardian_set_account(GUARDIAN_SET_INDEX, &keys, 0, 0, &core_bridge_program_id()),
            ),
            (self.guardian_signatures_pubkey, self.signatures_account()),
            (self.noreplay_bucket, system_owned_account(0)),
            keyed_account_for_noreplay_program(),
            (self.noreplay_authority, system_owned_account(0)),
            (self.source_account, uninitialised_pda_account()),
        ];
        if self.dest_account != self.source_account {
            accounts.push((self.dest_account, uninitialised_pda_account()));
        }
        accounts.push(keyed_account_for_system_program());
        accounts.push((self.chain_registration, chain_registration_account(self.chain, &self.emitter)));
        accounts
    }

    fn submit(
        &self,
        mollusk: &Mollusk,
        accounts: Vec<(Pubkey, Account)>,
    ) -> mollusk_svm::result::InstructionResult {
        let ix = Instruction::new_with_bytes(
            program_id(),
            &submit_vaas_ix_data(self.guardian_set_bump, &self.body),
            self.metas(),
        );
        mollusk.process_instruction(&ix, &accounts)
    }
}

fn assert_already_accounted(result: &mollusk_svm::result::InstructionResult) {
    match &result.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err.clone()) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::AlreadyAccounted as u32,
                "expected AlreadyAccounted, got code {code}"
            );
        }
        other => panic!("expected Failure(AlreadyAccounted), got {other:?}"),
    }
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

/// submit_vaas marks the slot; a later submit_observations quorum for the same
/// `(chain, emitter, sequence)` is rejected `AlreadyAccounted`.
#[test]
fn vaas_then_observations_same_triple_rejects_already_accounted() {
    let mollusk = mollusk();
    let chain: u16 = 2;
    let mut emitter = [0u8; 32];
    emitter[31] = 0x77;
    let sequence: u64 = 0x0000_0000_0000_0042;
    let token_chain: u16 = 2;
    let token_address = [0x55u8; 32];
    let recipient_chain: u16 = 1;
    let body = build_transfer_body(chain, &emitter, sequence, 1_000, token_chain, &token_address, recipient_chain);

    // 1) Account the triple via submit_vaas (single-call mark).
    let vaas = VaasCtx::new(chain, emitter, sequence, body.clone(), token_chain, &token_address, recipient_chain);
    let first = vaas.submit(&mollusk, vaas.initial_accounts());
    assert!(
        matches!(first.program_result, ProgramResult::Success),
        "submit_vaas must mark the slot, got {:?}",
        first.program_result
    );
    let marked_bucket = find_account(&first.resulting_accounts, &vaas.noreplay_bucket).clone();

    // 2) Carry the now-marked bucket into a fresh submit_observations quorum
    //    attempt for the same triple. The first observation must already trip
    //    the NoReplay pre-check.
    let obs = ObsCtx::new(chain, emitter, sequence, body, token_chain, &token_address, recipient_chain);
    assert_eq!(
        obs.noreplay_bucket, vaas.noreplay_bucket,
        "both paths must derive the same bucket PDA for the triple"
    );
    let mut accounts = obs.initial_accounts();
    upsert(&mut accounts, obs.noreplay_bucket, marked_bucket);

    let replay = obs.submit_once(&mollusk, accounts, 0);
    assert_already_accounted(&replay);
}

/// submit_observations reaches quorum and marks the slot; a later submit_vaas
/// for the same `(chain, emitter, sequence)` is rejected `AlreadyAccounted`.
#[test]
fn observations_then_vaas_same_triple_rejects_already_accounted() {
    let mollusk = mollusk();
    let chain: u16 = 2;
    let mut emitter = [0u8; 32];
    emitter[31] = 0x77;
    let sequence: u64 = 0x0000_0000_0000_0042;
    let token_chain: u16 = 2;
    let token_address = [0x66u8; 32];
    let recipient_chain: u16 = 1;
    let body = build_transfer_body(chain, &emitter, sequence, 2_000, token_chain, &token_address, recipient_chain);

    // 1) Account the triple via the quorum path.
    let obs = ObsCtx::new(chain, emitter, sequence, body.clone(), token_chain, &token_address, recipient_chain);
    let after_quorum = obs.drive_to_quorum(&mollusk);
    let marked_bucket = find_account(&after_quorum, &obs.noreplay_bucket).clone();
    assert_eq!(
        marked_bucket.owner,
        noreplay_program_id(),
        "quorum must flip the bucket to noreplay ownership"
    );

    // 2) Carry the marked bucket into a submit_vaas call for the same triple.
    let vaas = VaasCtx::new(chain, emitter, sequence, body, token_chain, &token_address, recipient_chain);
    assert_eq!(vaas.noreplay_bucket, obs.noreplay_bucket);
    let mut accounts = vaas.initial_accounts();
    upsert(&mut accounts, vaas.noreplay_bucket, marked_bucket);

    let replay = vaas.submit(&mollusk, accounts);
    assert_already_accounted(&replay);
}

// Tie the QUORUM local to the derived threshold for this set size so a change
// to the formula fails to compile here rather than silently desyncing.
const _: () = assert!(QUORUM as u32 == PendingObservationsLayout::quorum_for(GUARDIAN_COUNT as u32));
