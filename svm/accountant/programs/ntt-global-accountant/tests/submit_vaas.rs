//! Integration tests for NTT `submit_vaas` — the Shim-verified signed-VAA
//! backfill path (digest verify via the real Verify VAA Shim CPI, NoReplay
//! pre-check/mark, commit-log emission, then `apply_ntt_transfer`).
//!
//! Driven against a Mollusk instance with the real `solana_noreplay.so` and
//! `wormhole_verify_vaa_shim.so` loaded at their canonical program IDs (see
//! `common::mollusk_fixtures`). Mirrors the WTT `submit_vaas` harness
//! mechanics (`programs/global-accountant/tests/submit_vaas.rs`), swapping in
//! the NTT account layout (relayer / hub / two peers / source+dest balance)
//! and an NTT `TransceiverMessage` body, matching the scenario builder in
//! `submit_observations.rs`.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        ntt_global_accountant::Instruction as IxDiscriminator, BalanceAccountLayout,
        GlobalAccountantError, TransceiverHubLayout, TransceiverPeerLayout, Uint256,
        ACCOUNT_SEED_PREFIX, CORE_BRIDGE_PROGRAM_ID, NATIVE_TOKEN_TRANSFER_PREFIX,
        NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID,
        TRANSCEIVER_HUB_SEED_PREFIX, TRANSCEIVER_MESSAGE_PREFIX, TRANSCEIVER_PEER_SEED_PREFIX,
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

// ============================================================================
// PDA derivation (identical to submit_observations.rs's helpers)
// ============================================================================

fn derive_balance_account_pda(
    chain: u16,
    token_chain: u16,
    token_address: &[u8; 32],
) -> (Pubkey, u8) {
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

fn derive_hub_pda(chain: u16, address: &[u8; 32]) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    Pubkey::find_program_address(
        &[TRANSCEIVER_HUB_SEED_PREFIX, &chain_be, address],
        &program_id(),
    )
}

fn derive_peer_pda(chain: u16, address: &[u8; 32], dest_chain: u16) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    let dest_chain_be = dest_chain.to_be_bytes();
    Pubkey::find_program_address(
        &[
            TRANSCEIVER_PEER_SEED_PREFIX,
            &chain_be,
            address,
            &dest_chain_be,
        ],
        &program_id(),
    )
}

fn derive_relayer_registration_pda(chain: u16) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    Pubkey::find_program_address(
        &[
            global_accountant_definitions::RELAYER_CHAIN_REGISTRATION_SEED_PREFIX,
            &chain_be,
        ],
        &program_id(),
    )
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

// ============================================================================
// Wire builders (identical shapes to submit_observations.rs's helpers)
// ============================================================================

fn build_ntt_message(decimals: u8, raw_amount: u64, to_chain: u16) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&TRANSCEIVER_MESSAGE_PREFIX);
    v.extend_from_slice(&[0xAA; 32]); // source_ntt_manager
    v.extend_from_slice(&[0xBB; 32]); // recipient_ntt_manager
    v.extend_from_slice(&145u16.to_be_bytes()); // ntt_manager_payload_len (informational)
    v.extend_from_slice(&[0xCC; 32]); // id
    v.extend_from_slice(&[0xDD; 32]); // sender (unused by accountant)
    v.extend_from_slice(&79u16.to_be_bytes()); // inner payload_len (informational)
    v.extend_from_slice(&NATIVE_TOKEN_TRANSFER_PREFIX);
    v.push(decimals);
    v.extend_from_slice(&raw_amount.to_be_bytes());
    v.extend_from_slice(&[0xEE; 32]); // source_token (ignored: hub identity used)
    v.extend_from_slice(&[0xFF; 32]); // to (ignored)
    v.extend_from_slice(&to_chain.to_be_bytes());
    v
}

fn build_vaa_body(
    emitter_chain: u16,
    emitter_address: &[u8; 32],
    sequence: u64,
    ntt_payload: &[u8],
) -> Vec<u8> {
    let mut body = vec![0u8; 51];
    body[8..10].copy_from_slice(&emitter_chain.to_be_bytes());
    body[10..42].copy_from_slice(emitter_address);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body.extend_from_slice(ntt_payload);
    body
}

fn submit_vaas_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    // Wire: discriminator + guardian_set_bump(1) + body_len(u16 LE) + body. No
    // digest or PDA bumps travel; the dedup digest and routing tuple are
    // derived on-chain from the body header [8..50].
    let mut data = Vec::with_capacity(1 + 1 + 2 + body.len());
    data.push(IxDiscriminator::SubmitVaas as u8);
    data.push(guardian_set_bump);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
    data
}

// ============================================================================
// Account fixtures
// ============================================================================

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

fn hub_account(chain: u16, address: &[u8; 32], hub_chain: u16, hub_address: &[u8; 32]) -> Account {
    let mut layout: TransceiverHubLayout = bytemuck::Zeroable::zeroed();
    layout.tag = TransceiverHubLayout::TAG;
    layout.chain = chain;
    layout.hub_chain = hub_chain;
    layout.address = *address;
    layout.hub_address = *hub_address;
    Account {
        lamports: 1_000_000,
        data: bytemuck::bytes_of(&layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn peer_account(
    chain: u16,
    address: &[u8; 32],
    dest_chain: u16,
    peer_address: &[u8; 32],
) -> Account {
    let mut layout: TransceiverPeerLayout = bytemuck::Zeroable::zeroed();
    layout.tag = TransceiverPeerLayout::TAG;
    layout.chain = chain;
    layout.dest_chain = dest_chain;
    layout.address = *address;
    layout.peer_address = *peer_address;
    Account {
        lamports: 1_000_000,
        data: bytemuck::bytes_of(&layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn guardian_keys(guardians: &[Guardian]) -> Vec<[u8; GUARDIAN_PUBKEY_LENGTH]> {
    guardians.iter().map(|g| g.eth_address).collect()
}

// ============================================================================
// Scenario
// ============================================================================

/// A non-relayer NTT transfer scenario, mirroring `submit_observations.rs`'s
/// `Scenario` but wired for the Shim-verified `submit_vaas` account layout
/// (real `GuardianSet`/`GuardianSignatures` accounts instead of a pending-PDA
/// quorum tracker).
struct Scenario {
    emitter_chain: u16,
    emitter: [u8; 32],
    recipient_chain: u16,
    hub_chain: u16,
    hub_address: [u8; 32],
    dest_peer: [u8; 32],
    normalized: Uint256,

    body: Vec<u8>,
    digest: [u8; 32],
    guardians: Vec<Guardian>,

    submitter: Pubkey,
    guardian_set_pubkey: Pubkey,
    guardian_set_bump: u8,
    guardian_signatures_pubkey: Pubkey,
    noreplay_bucket_pubkey: Pubkey,
    noreplay_authority_pubkey: Pubkey,
    relayer_registration_pubkey: Pubkey,
    hub_pubkey: Pubkey,
    peer_src_pubkey: Pubkey,
    peer_dst_pubkey: Pubkey,
    source_balance_pubkey: Pubkey,
    dest_balance_pubkey: Pubkey,
}

impl Scenario {
    /// Default scenario: hub native chain == emitter (source) chain.
    fn new(seed: u8) -> Self {
        Self::with_hub_chain(seed, 2)
    }

    fn with_hub_chain(seed: u8, hub_chain: u16) -> Self {
        let emitter_chain: u16 = 2;
        let mut emitter = [0u8; 32];
        emitter[0] = seed; // per-scenario isolation
        emitter[31] = 0x77;
        let sequence: u64 = 0x42;
        let recipient_chain: u16 = 10;
        let hub_address = [0x33u8; 32];
        let dest_peer = [0x88u8; 32];

        // 3 decimals -> 8: x10^5. 1000 -> 100_000_000.
        let decimals = 3u8;
        let raw_amount = 1000u64;
        let normalized = Uint256::from_u128(100_000_000);

        let ntt = build_ntt_message(decimals, raw_amount, recipient_chain);
        let body = build_vaa_body(emitter_chain, &emitter, sequence, &ntt);
        let digest = double_keccak256_host(&body);

        let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
        let submitter = Pubkey::new_from_array([0x11u8; 32]);
        let (guardian_set_pubkey, guardian_set_bump) =
            derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
        let (noreplay_authority_pubkey, _) =
            Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
        let noreplay_bucket_pubkey = derive_canonical_noreplay_bucket(
            &noreplay_authority_pubkey,
            emitter_chain,
            &emitter,
            sequence,
        );
        let (relayer_registration_pubkey, _) = derive_relayer_registration_pda(emitter_chain);
        let (hub_pubkey, _) = derive_hub_pda(emitter_chain, &emitter);
        let (peer_src_pubkey, _) = derive_peer_pda(emitter_chain, &emitter, recipient_chain);
        let (peer_dst_pubkey, _) = derive_peer_pda(recipient_chain, &dest_peer, emitter_chain);
        let (source_balance_pubkey, _) =
            derive_balance_account_pda(emitter_chain, hub_chain, &hub_address);
        let (dest_balance_pubkey, _) =
            derive_balance_account_pda(recipient_chain, hub_chain, &hub_address);

        Self {
            emitter_chain,
            emitter,
            recipient_chain,
            hub_chain,
            hub_address,
            dest_peer,
            normalized,
            body,
            digest,
            guardians,
            submitter,
            guardian_set_pubkey,
            guardian_set_bump,
            guardian_signatures_pubkey: Pubkey::new_from_array([0xC3u8; 32]),
            noreplay_bucket_pubkey,
            noreplay_authority_pubkey,
            relayer_registration_pubkey,
            hub_pubkey,
            peer_src_pubkey,
            peer_dst_pubkey,
            source_balance_pubkey,
            dest_balance_pubkey,
        }
    }

    /// Account slot order (see `submit_vaas::process`):
    ///   0. submitter (WRITE, SIGNER)
    ///   1. Verify VAA Shim program
    ///   2. Core Bridge GuardianSet
    ///   3. GuardianSignatures
    ///   4. NoReplay bitmap PDA (WRITE)
    ///   5. NoReplay program
    ///   6. NoReplay authority PDA
    ///   7. system program
    ///   8. relayer-registration PDA
    ///   9. TransceiverHub PDA
    ///  10. TransceiverPeer src PDA
    ///  11. TransceiverPeer dst PDA
    ///  12. source balance (WRITE)
    ///  13. dest balance (WRITE)
    fn account_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.submitter, true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(self.guardian_set_pubkey, false),
            AccountMeta::new_readonly(self.guardian_signatures_pubkey, false),
            AccountMeta::new(self.noreplay_bucket_pubkey, false),
            AccountMeta::new_readonly(Pubkey::new_from_array(NOREPLAY_PROGRAM_ID), false),
            AccountMeta::new_readonly(self.noreplay_authority_pubkey, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(self.relayer_registration_pubkey, false),
            AccountMeta::new_readonly(self.hub_pubkey, false),
            AccountMeta::new_readonly(self.peer_src_pubkey, false),
            AccountMeta::new_readonly(self.peer_dst_pubkey, false),
            AccountMeta::new(self.source_balance_pubkey, false),
            AccountMeta::new(self.dest_balance_pubkey, false),
        ]
    }

    /// `GuardianSignatures` fixture signed over `self.digest` by a full quorum.
    fn guardian_signatures_for(&self, digest: &[u8; 32]) -> Account {
        let sigs: Vec<(u8, [u8; 65])> = (0..QUORUM)
            .map(|i| (i, sign_digest(&self.guardians[i as usize], digest)))
            .collect();
        guardian_signatures_account(
            GUARDIAN_SET_INDEX,
            &self.submitter,
            &sigs,
            &shim_program_id(),
        )
    }

    /// Initial account list. Hub + both peers pre-seeded for a valid topology;
    /// negative tests override entries before calling `submit`.
    fn initial_accounts(&self) -> Vec<(Pubkey, Account)> {
        vec![
            (self.submitter, system_owned_account(50_000_000_000)),
            keyed_account_for_verify_vaa_shim_program(),
            (
                self.guardian_set_pubkey,
                guardian_set_account(
                    GUARDIAN_SET_INDEX,
                    &guardian_keys(&self.guardians),
                    0,
                    0,
                    &core_bridge_program_id(),
                ),
            ),
            (
                self.guardian_signatures_pubkey,
                self.guardian_signatures_for(&self.digest),
            ),
            (self.noreplay_bucket_pubkey, system_owned_account(0)),
            keyed_account_for_noreplay_program(),
            (self.noreplay_authority_pubkey, system_owned_account(0)),
            keyed_account_for_system_program(),
            (
                self.relayer_registration_pubkey,
                uninitialised_pda_account(),
            ),
            (
                self.hub_pubkey,
                hub_account(
                    self.emitter_chain,
                    &self.emitter,
                    self.hub_chain,
                    &self.hub_address,
                ),
            ),
            (
                self.peer_src_pubkey,
                peer_account(
                    self.emitter_chain,
                    &self.emitter,
                    self.recipient_chain,
                    &self.dest_peer,
                ),
            ),
            (
                self.peer_dst_pubkey,
                peer_account(
                    self.recipient_chain,
                    &self.dest_peer,
                    self.emitter_chain,
                    &self.emitter,
                ),
            ),
            (self.source_balance_pubkey, uninitialised_pda_account()),
            (self.dest_balance_pubkey, uninitialised_pda_account()),
        ]
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

fn expect_failure(r: &mollusk_svm::result::InstructionResult, expected: GlobalAccountantError) {
    match &r.program_result {
        ProgramResult::Failure(e) => {
            let code = u64::from(e.clone()) as u32;
            assert_eq!(
                code, expected as u32,
                "expected {expected:?}, got code {code}"
            );
        }
        other => panic!("expected Failure({expected:?}), got {other:?}"),
    }
}

// ============================================================================
// Tests
// ============================================================================

/// Happy path: the Shim verifies the digest against a genuine quorum of
/// signatures, NoReplay flips, and `apply_ntt_transfer` credits both the
/// source and destination balances at the HUB token identity.
#[test]
fn submit_vaas_happy_path_applies_transfer_and_marks_replay() {
    let mollusk = mollusk();
    let scenario = Scenario::new(0xB0);

    let result = scenario.submit(&mollusk, scenario.initial_accounts());
    assert!(
        matches!(result.program_result, ProgramResult::Success),
        "expected Success, got {:?}",
        result.program_result
    );

    let bucket = find_account(
        &result.resulting_accounts,
        &scenario.noreplay_bucket_pubkey,
    );
    assert_eq!(
        bucket.owner,
        Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        "bucket owned by solana_noreplay after MarkUsed"
    );

    let source = find_account(&result.resulting_accounts, &scenario.source_balance_pubkey);
    let source_layout: &BalanceAccountLayout = bytemuck::from_bytes(&source.data);
    assert_eq!(source_layout.token_chain, scenario.hub_chain);
    assert_eq!(source_layout.token_address, scenario.hub_address);
    assert_eq!(source_layout.balance, scenario.normalized);

    let dest = find_account(&result.resulting_accounts, &scenario.dest_balance_pubkey);
    let dest_layout: &BalanceAccountLayout = bytemuck::from_bytes(&dest.data);
    assert_eq!(dest_layout.token_chain, scenario.hub_chain);
    assert_eq!(dest_layout.balance, scenario.normalized);
}

/// A `GuardianSignatures` account whose signatures were produced over a
/// DIFFERENT digest than the one the Shim recomputes from the supplied VAA
/// body: the recovered pubkeys don't match the guardian set, so the Shim CPI
/// itself fails. Since the Shim — not this program — is the sole authenticity
/// anchor on this path (see `shim::verify_vaa`'s doc comment), the failure
/// propagates as whatever error the Shim raises, not an accountant custom code.
#[test]
fn submit_vaas_digest_mismatch_rejects() {
    let mollusk = mollusk();
    let scenario = Scenario::new(0xB1);

    let mut accounts = scenario.initial_accounts();
    // Sign a DIFFERENT (unrelated) digest so the guardian signatures don't
    // correspond to `scenario.digest` / `scenario.body`.
    let wrong_digest = double_keccak256_host(b"an entirely different vaa body");
    for entry in accounts.iter_mut() {
        if entry.0 == scenario.guardian_signatures_pubkey {
            entry.1 = scenario.guardian_signatures_for(&wrong_digest);
        }
    }

    let result = scenario.submit(&mollusk, accounts);
    match result.program_result {
        // Error code is Shim-defined, not an accountant custom code.
        ProgramResult::Failure(_) => {}
        other => panic!("expected a Failure from the Shim CPI, got {other:?}"),
    }
    let bucket = find_account(
        &result.resulting_accounts,
        &scenario.noreplay_bucket_pubkey,
    );
    assert_eq!(
        bucket.owner,
        system_program_id(),
        "NoReplay must stay unmarked when the Shim rejects the digest"
    );
}

/// A body shorter than the 51-byte header + at-least-one-payload-byte bound
/// (`body_len <= VaaBodyHeader::LEN`) rejects with `InvalidInstructionData`
/// before the Shim is even invoked.
#[test]
fn submit_vaas_with_short_body_rejects() {
    let mollusk = mollusk();
    let scenario = Scenario::new(0xB2);

    let short_body = vec![0u8; 51]; // == VaaBodyHeader::LEN, not > it
    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_vaas_ix_data(scenario.guardian_set_bump, &short_body),
        scenario.account_metas(),
    );
    let result = mollusk.process_instruction(&ix, &scenario.initial_accounts());
    expect_failure(&result, GlobalAccountantError::InvalidInstructionData);
}

/// The declared `body_len` prefix overstating the bytes that actually follow
/// on the wire rejects via the `data.len() != FIXED_LEN + body_len` gate —
/// distinct from the short-body gate above.
#[test]
fn submit_vaas_with_declared_len_mismatch_rejects() {
    let mollusk = mollusk();
    let scenario = Scenario::new(0xB3);

    let declared_len = (scenario.body.len() + 1) as u16;
    let mut wire = Vec::with_capacity(1 + 1 + 2 + scenario.body.len());
    wire.push(IxDiscriminator::SubmitVaas as u8);
    wire.push(scenario.guardian_set_bump);
    wire.extend_from_slice(&declared_len.to_le_bytes());
    wire.extend_from_slice(&scenario.body);
    let ix = Instruction::new_with_bytes(program_id(), &wire, scenario.account_metas());
    let result = mollusk.process_instruction(&ix, &scenario.initial_accounts());
    expect_failure(&result, GlobalAccountantError::InvalidInstructionData);
}

/// A second `submit_vaas` for the same `(chain, emitter, sequence)` rejects
/// with `AlreadyAccounted` via the NoReplay pre-check — the replay-protection
/// resubmission case.
#[test]
fn submit_vaas_rejects_resubmission_after_noreplay_set() {
    let mollusk = mollusk();
    let scenario = Scenario::new(0xB4);

    let first = scenario.submit(&mollusk, scenario.initial_accounts());
    assert!(
        matches!(first.program_result, ProgramResult::Success),
        "first submission must succeed, got {:?}",
        first.program_result
    );

    let second = scenario.submit(&mollusk, first.resulting_accounts.clone());
    expect_failure(&second, GlobalAccountantError::AlreadyAccounted);
}
