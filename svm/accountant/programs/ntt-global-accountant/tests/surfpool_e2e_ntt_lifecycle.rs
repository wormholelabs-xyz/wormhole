//! Surfpool E2E — full NTT global-accountant lifecycle against a live
//! validator: deploy the program, then run
//! `register_hub` -> `register_peer` (adopt) -> `register_peer` (match) ->
//! `register_relayer_chain` -> `submit_vaas` (relayer-unwrap transfer) ->
//! `modify_balance` -> a real on-chain negative path (replaying the
//! `modify_balance` VAA rejects `DuplicateModifyBalance`), asserting on-chain
//! state after each step.
//!
//! Modelled on `programs/global-accountant/tests/surfpool_e2e_submit_vaas.rs`
//! (see that file's doc comment for the surfpool subprocess / cheatcode
//! pattern), with one deliberate divergence: the WTT e2e test replays a real
//! historical mainnet Token Bridge transfer VAA against the REAL, currently
//! active guardian set (fetched via a mainnet fork). No such real, signed VAA
//! exists for the NTT-specific messages this test drives (`register_hub`,
//! `register_peer`, `register_relayer_chain`, and a synthetic NTT transfer) —
//! they are internal to this program and were never emitted by the real
//! guardians. This test therefore runs fully `--offline` and instead:
//!
//!   - Deploys the real `wormhole_verify_vaa_shim.so` / `solana_noreplay.so`
//!     fixtures plus this crate's own freshly built `.so` (same real binaries
//!     the mollusk suite CPIs into, executed here by a live SBF loader).
//!   - Cheat-writes a `GuardianSet` account at the canonical Core-Bridge PDA
//!     address with a SYNTHETIC set of guardian keys (the same deterministic
//!     `libsecp256k1` keys the mollusk suite uses). This is sound because the
//!     Shim's `VerifyHash` only checks the guardian-set account's ADDRESS
//!     against `[GUARDIAN_SET_SEED, index, bump]` under the Core Bridge
//!     program ID — it never asserts that account's *owner* — so a
//!     cheat-written account at that exact address is indistinguishable, from
//!     the Shim's perspective, from a genuine Core-Bridge-owned one. This is
//!     the same account shape `common::guardian_fixtures::guardian_set_account`
//!     builds for the mollusk suite, which already exercises this exact real
//!     `.so` successfully (see `register_hub.rs` et al.).
//!   - Signs each synthetic NTT VAA body with that same guardian set and
//!     posts the signatures via the Shim's real `PostSignatures` instruction.
//!
//! # Run
//!
//! ```sh
//! cd svm/accountant && just build && cargo test -p ntt-global-accountant \
//!     --test surfpool_e2e_ntt_lifecycle -- --ignored
//! ```
//!
//! # Note on `CreateAccountAllowPrefund` (SIMD-0312)
//!
//! `accountant-operational-core::instructions::pda_init::init_or_upgrade_pda`
//! — the PDA-creation helper used by every instruction in both this program
//! and the sibling WTT `global-accountant` program — allocates via the System
//! Program's `CreateAccountAllowPrefund` instruction (discriminant 13,
//! SIMD-0312: <https://github.com/solana-foundation/solana-improvement-documents/blob/main/proposals/0312-create-account-allow-prefund.md>).
//! Requires `surfpool` >= 1.5.0: earlier builds bundle a System Program
//! predating this feature's activation in their simulated feature set, so
//! PDA creation fails with "invalid instruction data".

#![allow(clippy::too_many_arguments)]

use std::time::Duration;

use global_accountant_definitions::{
    BalanceAccountLayout, GlobalAccountantError, ModifyBalanceLayout,
    RelayerChainRegistrationLayout, TransceiverHubLayout, TransceiverPeerLayout, Uint256,
    ACCOUNT_SEED_PREFIX, CORE_BRIDGE_PROGRAM_ID, GOVERNANCE_EMITTER, MODIFY_BALANCE_SEED_PREFIX,
    MODIFY_BALANCE_ACTION, NATIVE_TOKEN_TRANSFER_PREFIX, NOREPLAY_AUTHORITY_SEED_PREFIX,
    NTT_ACCOUNTANT_GOVERNANCE_MODULE, REGISTER_CHAIN_ACTION, RELAYER_CHAIN_REGISTRATION_SEED_PREFIX,
    RELAYER_GOVERNANCE_MODULE, SOLANA_CHAIN_ID, TRANSCEIVER_HUB_SEED_PREFIX,
    TRANSCEIVER_INFO_PREFIX, TRANSCEIVER_MESSAGE_PREFIX, TRANSCEIVER_PEER_INFO_PREFIX,
    TRANSCEIVER_PEER_SEED_PREFIX, VaaBodyHeader, VERIFY_VAA_SHIM_PROGRAM_ID,
};
use global_accountant_definitions::ntt_global_accountant::Instruction as IxDiscriminator;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_system_interface::program as system_program;
use solana_transaction::Transaction;

mod common;
use common::{
    await_confirmed, deploy_program, hex_encode, noreplay_so_path, set_account, so_path,
    start_surfpool, verify_vaa_shim_so_path, SurfpoolOptions, NOREPLAY_PROGRAM_ID,
};
use common::guardian_fixtures::{make_guardians, sign_digest, Guardian, GUARDIAN_PUBKEY_LENGTH};

const GUARDIAN_COUNT: usize = 19;
const QUORUM: u8 = 13;
const GUARDIAN_SET_INDEX: u32 = 1;
const GUARDIAN_SET_SEED: &[u8] = b"GuardianSet";

/// Anchor discriminator for the Shim's `post_signatures`
/// (`sha256("global:post_signatures")[..8]`).
const POST_SIGNATURES_SELECTOR: [u8; 8] = [0x8a, 0x02, 0x35, 0xa6, 0x2d, 0x4d, 0x89, 0x33];

/// Compute Budget program ID.
const COMPUTE_BUDGET_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
    0x03, 0x06, 0x46, 0x6f, 0xe5, 0x21, 0x17, 0x32, 0xff, 0xec, 0xad, 0xba, 0x72, 0xc3, 0x9b, 0xe7,
    0xbc, 0x8c, 0xe5, 0xbb, 0xc5, 0xf7, 0x12, 0x6b, 0x2c, 0x43, 0x9b, 0x3a, 0x40, 0x00, 0x00, 0x00,
]);

/// CU ceiling generous enough for the Shim's `VerifyHash` (~200k CU) plus this
/// program's own PDA lazy-inits and CPIs.
const CU_LIMIT: u32 = 400_000;

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
// PDA derivation
// ============================================================================

fn derive_guardian_set_pda(index: u32) -> (Pubkey, u8) {
    let idx_be = index.to_be_bytes();
    Pubkey::find_program_address(&[GUARDIAN_SET_SEED, &idx_be], &core_bridge_program_id())
}

fn derive_hub_pda(program_id: &Pubkey, chain: u16, address: &[u8; 32]) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    Pubkey::find_program_address(
        &[TRANSCEIVER_HUB_SEED_PREFIX, &chain_be, address],
        program_id,
    )
}

fn derive_peer_pda(
    program_id: &Pubkey,
    chain: u16,
    address: &[u8; 32],
    dest_chain: u16,
) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    let dest_chain_be = dest_chain.to_be_bytes();
    Pubkey::find_program_address(
        &[
            TRANSCEIVER_PEER_SEED_PREFIX,
            &chain_be,
            address,
            &dest_chain_be,
        ],
        program_id,
    )
}

fn derive_relayer_registration_pda(program_id: &Pubkey, chain: u16) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    Pubkey::find_program_address(
        &[RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, &chain_be],
        program_id,
    )
}

fn derive_balance_pda(
    program_id: &Pubkey,
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
        program_id,
    )
}

fn derive_noreplay_authority_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], program_id)
}

fn derive_modification_pda(program_id: &Pubkey, sequence: u64) -> (Pubkey, u8) {
    let seq_be = sequence.to_be_bytes();
    Pubkey::find_program_address(&[MODIFY_BALANCE_SEED_PREFIX, &seq_be], program_id)
}

fn build_namespace(chain: u16, emitter: &[u8; 32]) -> [u8; 34] {
    let mut ns = [0u8; 34];
    ns[..2].copy_from_slice(&chain.to_be_bytes());
    ns[2..].copy_from_slice(emitter);
    ns
}

fn derive_noreplay_bucket(program_id: &Pubkey, chain: u16, emitter: &[u8; 32], sequence: u64) -> Pubkey {
    let (authority, _) = derive_noreplay_authority_pda(program_id);
    common::derive_noreplay_bitmap_pda(&authority, &build_namespace(chain, emitter), sequence).0
}

// ============================================================================
// Wire / body builders
// ============================================================================

/// `WormholeTransceiverInfo` (hub-registration, `INFO_PREFIX`) payload.
fn build_hub_body(emitter_chain: u16, emitter_address: &[u8; 32], sequence: u64) -> Vec<u8> {
    let mut body = vec![0u8; VaaBodyHeader::LEN];
    body[8..10].copy_from_slice(&emitter_chain.to_be_bytes());
    body[10..42].copy_from_slice(emitter_address);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body.extend_from_slice(&TRANSCEIVER_INFO_PREFIX);
    body.extend_from_slice(&[0x11u8; 32]); // manager_address
    body.push(0); // mode = Locking
    body.extend_from_slice(&[0x22u8; 32]); // token_address
    body.push(8); // token_decimals
    body
}

/// `WormholeTransceiverRegistration` (peer-registration, `PEER_INFO_PREFIX`).
fn build_peer_body(
    emitter_chain: u16,
    emitter_address: &[u8; 32],
    sequence: u64,
    dest_chain: u16,
    peer_address: &[u8; 32],
) -> Vec<u8> {
    let mut body = vec![0u8; VaaBodyHeader::LEN];
    body[8..10].copy_from_slice(&emitter_chain.to_be_bytes());
    body[10..42].copy_from_slice(emitter_address);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body.extend_from_slice(&TRANSCEIVER_PEER_INFO_PREFIX);
    body.extend_from_slice(&dest_chain.to_be_bytes());
    body.extend_from_slice(peer_address);
    body
}

/// `WormholeRelayer` `RegisterChain` governance VAA body (120 bytes).
fn build_register_relayer_chain_body(
    sequence: u64,
    target_chain: u16,
    chain_to_register: u16,
    emitter_to_register: &[u8; 32],
) -> Vec<u8> {
    let mut body = vec![0u8; 120];
    body[8..10].copy_from_slice(&SOLANA_CHAIN_ID.to_be_bytes());
    body[10..42].copy_from_slice(&GOVERNANCE_EMITTER);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body[51..83].copy_from_slice(&RELAYER_GOVERNANCE_MODULE);
    body[83] = REGISTER_CHAIN_ACTION;
    body[84..86].copy_from_slice(&target_chain.to_be_bytes());
    body[86..88].copy_from_slice(&chain_to_register.to_be_bytes());
    body[88..120].copy_from_slice(emitter_to_register);
    body
}

/// NTT `ModifyBalance` governance VAA body (195 bytes).
fn build_modify_balance_body(
    sequence: u64,
    payload_sequence: u64,
    chain_id: u16,
    token_chain: u16,
    token_address: &[u8; 32],
    kind: u8,
    amount: Uint256,
    reason: &[u8; 32],
) -> Vec<u8> {
    let mut body = vec![0u8; 195];
    body[8..10].copy_from_slice(&SOLANA_CHAIN_ID.to_be_bytes());
    body[10..42].copy_from_slice(&GOVERNANCE_EMITTER);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body[51..83].copy_from_slice(&NTT_ACCOUNTANT_GOVERNANCE_MODULE);
    body[83] = MODIFY_BALANCE_ACTION;
    body[84..86].copy_from_slice(&SOLANA_CHAIN_ID.to_be_bytes());
    body[86..94].copy_from_slice(&payload_sequence.to_be_bytes());
    body[94..96].copy_from_slice(&chain_id.to_be_bytes());
    body[96..98].copy_from_slice(&token_chain.to_be_bytes());
    body[98..130].copy_from_slice(token_address);
    body[130] = kind;
    body[131..163].copy_from_slice(&amount.0);
    body[163..195].copy_from_slice(reason);
    body
}

/// NTT `TransceiverMessage` wrapping one `NativeTokenTransfer`.
fn build_ntt_message(decimals: u8, raw_amount: u64, to_chain: u16) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&TRANSCEIVER_MESSAGE_PREFIX);
    v.extend_from_slice(&[0xAA; 32]);
    v.extend_from_slice(&[0xBB; 32]);
    v.extend_from_slice(&145u16.to_be_bytes());
    v.extend_from_slice(&[0xCC; 32]);
    v.extend_from_slice(&[0xDD; 32]);
    v.extend_from_slice(&79u16.to_be_bytes());
    v.extend_from_slice(&NATIVE_TOKEN_TRANSFER_PREFIX);
    v.push(decimals);
    v.extend_from_slice(&raw_amount.to_be_bytes());
    v.extend_from_slice(&[0xEE; 32]);
    v.extend_from_slice(&[0xFF; 32]);
    v.extend_from_slice(&to_chain.to_be_bytes());
    v
}

/// Standard-relayer `DeliveryInstruction` wrapping `inner`, recovering
/// `sender` on unwrap. See `definitions::ntt::tests::build_delivery`.
fn build_delivery_instruction(sender: [u8; 32], inner: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.push(1); // DELIVERY_INSTRUCTION_PAYLOAD_ID
    v.extend_from_slice(&7u16.to_be_bytes());
    v.extend_from_slice(&[0x01; 32]);
    v.extend_from_slice(&(inner.len() as u32).to_be_bytes());
    v.extend_from_slice(inner);
    v.extend_from_slice(&[0x02; 32]);
    v.extend_from_slice(&[0x03; 32]);
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(&9u16.to_be_bytes());
    v.extend_from_slice(&[0x04; 32]);
    v.extend_from_slice(&[0x05; 32]);
    v.extend_from_slice(&[0x06; 32]);
    v.extend_from_slice(&sender);
    v.push(0); // num_messages
    v
}

fn build_transfer_body(
    emitter_chain: u16,
    emitter_address: &[u8; 32],
    sequence: u64,
    payload: &[u8],
) -> Vec<u8> {
    let mut body = vec![0u8; VaaBodyHeader::LEN];
    body[8..10].copy_from_slice(&emitter_chain.to_be_bytes());
    body[10..42].copy_from_slice(emitter_address);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body.extend_from_slice(payload);
    body
}

fn set_compute_unit_limit_ix(units: u32) -> Instruction {
    let mut data = Vec::with_capacity(5);
    data.push(0x02);
    data.extend_from_slice(&units.to_le_bytes());
    Instruction {
        program_id: COMPUTE_BUDGET_PROGRAM_ID,
        accounts: vec![],
        data,
    }
}

fn post_signatures_ix_data(guardian_set_index: u32, total_signatures: u8, sigs: &[u8]) -> Vec<u8> {
    assert_eq!(sigs.len() % 66, 0, "sig block must be a multiple of 66");
    let count = sigs.len() / 66;
    let mut data = Vec::with_capacity(8 + 9 + sigs.len());
    data.extend_from_slice(&POST_SIGNATURES_SELECTOR);
    data.extend_from_slice(&guardian_set_index.to_le_bytes());
    data.push(total_signatures);
    data.extend_from_slice(&(count as u32).to_le_bytes());
    data.extend_from_slice(sigs);
    data
}

/// Guardian-set bytes matching the Shim's `zero_copy::GuardianSet` layout:
/// `[index:u32 LE][keys_len:u32 LE][keys:20*N][creation_time:u32 LE][expiration_time:u32 LE]`.
/// Mirrors `common::guardian_fixtures::guardian_set_account`'s data shape.
fn guardian_set_bytes(index: u32, keys: &[[u8; GUARDIAN_PUBKEY_LENGTH]]) -> Vec<u8> {
    let mut data = Vec::with_capacity(8 + keys.len() * GUARDIAN_PUBKEY_LENGTH + 8);
    data.extend_from_slice(&index.to_le_bytes());
    data.extend_from_slice(&(keys.len() as u32).to_le_bytes());
    for key in keys {
        data.extend_from_slice(key);
    }
    data.extend_from_slice(&0u32.to_le_bytes()); // creation_time
    data.extend_from_slice(&0u32.to_le_bytes()); // expiration_time = never expires
    data
}

// ============================================================================
// Test harness: a live guardian set + a helper to sign-and-post a body's
// digest through the real Shim, then run one accountant instruction.
// ============================================================================

struct LiveHarness<'a> {
    rpc: &'a solana_client::rpc_client::RpcClient,
    payer: Keypair,
    guardians: Vec<Guardian>,
    guardian_set_bump: u8,
}

impl<'a> LiveHarness<'a> {
    /// Sign `body`'s digest with a quorum of the synthetic guardian set, post
    /// the signatures via the real Shim, and return the fresh
    /// `guardian_signatures` account pubkey to pass into the accountant ix.
    fn post_signatures_for(&self, body: &[u8]) -> Pubkey {
        let digest = double_keccak256_host(body);
        let sigs_kp = Keypair::new();
        let mut sig_block = Vec::with_capacity(QUORUM as usize * 66);
        for i in 0..QUORUM {
            let sig = sign_digest(&self.guardians[i as usize], &digest);
            sig_block.push(i);
            sig_block.extend_from_slice(&sig);
        }
        let ix = Instruction {
            program_id: shim_program_id(),
            accounts: vec![
                AccountMeta::new(self.payer.pubkey(), true),
                AccountMeta::new(sigs_kp.pubkey(), true),
                AccountMeta::new_readonly(system_program::ID, false),
            ],
            data: post_signatures_ix_data(GUARDIAN_SET_INDEX, QUORUM, &sig_block),
        };
        let blockhash = self.rpc.get_latest_blockhash().expect("blockhash");
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.payer.pubkey()),
            &[&self.payer, &sigs_kp],
            blockhash,
        );
        let sig = self
            .rpc
            .send_and_confirm_transaction(&tx)
            .expect("PostSignatures send_and_confirm");
        eprintln!("[lifecycle-e2e] PostSignatures tx={sig} for digest={}", hex_encode(&digest));
        sigs_kp.pubkey()
    }

    /// Send a single accountant instruction (already fully built) and return
    /// the transaction signature.
    fn send(&self, ix: Instruction) -> String {
        let blockhash = self.rpc.get_latest_blockhash().expect("blockhash");
        let tx = Transaction::new_signed_with_payer(
            &[set_compute_unit_limit_ix(CU_LIMIT), ix],
            Some(&self.payer.pubkey()),
            &[&self.payer],
            blockhash,
        );
        self.rpc
            .send_and_confirm_transaction(&tx)
            .unwrap_or_else(|e| panic!("send_and_confirm failed: {e}"))
            .to_string()
    }

    /// Send `ix` expecting the live validator to reject it, and return the
    /// program's custom error code (`ProgramError::Custom(u32)`). Panics if the
    /// transaction unexpectedly succeeds, or fails for a reason other than a
    /// program-raised custom error.
    fn send_expect_custom_error(&self, ix: Instruction) -> u32 {
        let blockhash = self.rpc.get_latest_blockhash().expect("blockhash");
        let tx = Transaction::new_signed_with_payer(
            &[set_compute_unit_limit_ix(CU_LIMIT), ix],
            Some(&self.payer.pubkey()),
            &[&self.payer],
            blockhash,
        );
        let err = match self.rpc.send_and_confirm_transaction(&tx) {
            Ok(sig) => panic!("expected rejection, but transaction {sig} succeeded"),
            Err(e) => e,
        };
        match err.get_transaction_error() {
            Some(solana_transaction::TransactionError::InstructionError(
                _,
                solana_instruction::error::InstructionError::Custom(code),
            )) => code,
            other => panic!("expected InstructionError::Custom(_), got {other:?} (raw: {err})"),
        }
    }
}

/// Full NTT lifecycle: register_hub -> register_peer (adopt) -> register_peer
/// (match) -> register_relayer_chain -> submit_vaas (relayer-unwrapped
/// transfer, hub-substituted crediting) -> modify_balance -> a negative path
/// (replaying the just-submitted modify_balance VAA), each a real transaction
/// against a live surfpool validator running the real `.so` binaries.
#[test]
#[ignore = "spawns surfpool subprocess; run via `cargo test -p ntt-global-accountant --test surfpool_e2e_ntt_lifecycle -- --ignored`"]
fn surfpool_ntt_lifecycle() {
    let ntt_so = so_path("ntt_global_accountant");
    let ntt_bytes = std::fs::read(&ntt_so).unwrap_or_else(|e| {
        panic!(
            "could not read {}: {e}. Run `just build` first.",
            ntt_so.display()
        )
    });
    let noreplay_bytes = std::fs::read(noreplay_so_path()).expect("read solana_noreplay.so");
    let shim_bytes = std::fs::read(verify_vaa_shim_so_path()).expect("read wormhole_verify_vaa_shim.so");

    let guard = start_surfpool(SurfpoolOptions::offline("ntt-lifecycle-e2e"));
    let rpc_url = guard.rpc_url();
    let rpc = guard.rpc_client();

    // Anchor's `declare_id!` pins the program to a single fixed address,
    // checked on every entry (`DeclaredProgramIdMismatch`). The pre-migration
    // pinocchio program had no such check and so deployed at a fresh random
    // keypair per run; deploy at the declared ID instead so the cheat-written
    // `.so` (which has the address baked in) passes Anchor's own check.
    let program_id = Pubkey::new_from_array(ntt_global_accountant::ID.to_bytes());
    let payer = Keypair::new();
    eprintln!("[lifecycle-e2e] program_id={program_id} payer={}", payer.pubkey());

    let airdrop_sig = rpc
        .request_airdrop(&payer.pubkey(), 50_000_000_000)
        .expect("airdrop payer");
    await_confirmed("airdrop", Duration::from_secs(10), || {
        rpc.confirm_transaction(&airdrop_sig)
    });

    deploy_program(&rpc_url, &program_id, &ntt_bytes);
    deploy_program(&rpc_url, &NOREPLAY_PROGRAM_ID, &noreplay_bytes);
    deploy_program(&rpc_url, &shim_program_id(), &shim_bytes);

    // Cheat-write the GuardianSet PDA with a synthetic guardian set — sound
    // because the Shim only checks this account's ADDRESS, never its owner
    // (see the module doc comment).
    let guardians = make_guardians(GUARDIAN_COUNT, 0x77);
    let keys: Vec<[u8; GUARDIAN_PUBKEY_LENGTH]> = guardians.iter().map(|g| g.eth_address).collect();
    let (guardian_set_pda, guardian_set_bump) = derive_guardian_set_pda(GUARDIAN_SET_INDEX);
    set_account(
        &rpc_url,
        &guardian_set_pda,
        &core_bridge_program_id(),
        1_000_000_000,
        &guardian_set_bytes(GUARDIAN_SET_INDEX, &keys),
    );
    eprintln!(
        "[lifecycle-e2e] guardian_set_pda={guardian_set_pda} bump={guardian_set_bump} \
         ({GUARDIAN_COUNT} synthetic guardians, quorum={QUORUM})"
    );

    let harness = LiveHarness {
        rpc: &rpc,
        payer,
        guardians,
        guardian_set_bump,
    };

    let (noreplay_authority, _) = derive_noreplay_authority_pda(&program_id);

    // ---- (1) register_hub: T1 registers itself as a Locking hub on chain A ----
    let chain_a: u16 = 2;
    let mut t1 = [0u8; 32];
    t1[31] = 0x01;
    let hub_body = build_hub_body(chain_a, &t1, 1);
    let hub_sigs_pubkey = harness.post_signatures_for(&hub_body);
    let (hub_t1_pda, hub_t1_bump) = derive_hub_pda(&program_id, chain_a, &t1);
    let hub_bucket = derive_noreplay_bucket(&program_id, chain_a, &t1, 1);

    let mut hub_data = Vec::with_capacity(1 + 1 + 1 + 2 + hub_body.len());
    hub_data.push(IxDiscriminator::RegisterHub as u8);
    hub_data.push(harness.guardian_set_bump);
    hub_data.push(hub_t1_bump);
    hub_data.extend_from_slice(&(hub_body.len() as u16).to_le_bytes());
    hub_data.extend_from_slice(&hub_body);
    let ix = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(harness.payer.pubkey(), true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(guardian_set_pda, false),
            AccountMeta::new_readonly(hub_sigs_pubkey, false),
            AccountMeta::new(hub_t1_pda, false),
            AccountMeta::new(hub_bucket, false),
            AccountMeta::new_readonly(NOREPLAY_PROGRAM_ID, false),
            AccountMeta::new_readonly(noreplay_authority, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: hub_data,
    };
    let sig = harness.send(ix);
    eprintln!("[lifecycle-e2e] register_hub tx={sig}");

    let hub_account = rpc.get_account(&hub_t1_pda).expect("hub PDA exists");
    assert_eq!(hub_account.owner, program_id, "hub PDA owned by program");
    let hub_layout: &TransceiverHubLayout = bytemuck::from_bytes(&hub_account.data);
    assert_eq!(hub_layout.chain, chain_a);
    assert_eq!(hub_layout.hub_chain, chain_a, "T1 is its own hub");
    assert_eq!(hub_layout.hub_address, t1, "T1 is its own hub");

    // ---- (2) register_peer: P (chain C) adopts T1's hub by registering T1 as
    //          its peer on chain A ----
    let chain_c: u16 = 10;
    let mut p = [0u8; 32];
    p[31] = 0x02;
    let peer_body_adopt = build_peer_body(chain_c, &p, 1, chain_a, &t1);
    let adopt_sigs_pubkey = harness.post_signatures_for(&peer_body_adopt);
    let (peer_hub_for_adopt_pda, _) = derive_hub_pda(&program_id, chain_a, &t1); // T1's hub (peer's hub target)
    let (own_hub_p_pda, own_hub_p_bump) = derive_hub_pda(&program_id, chain_c, &p);
    let (peer_c_p_a_pda, peer_c_p_a_bump) = derive_peer_pda(&program_id, chain_c, &p, chain_a);
    let adopt_bucket = derive_noreplay_bucket(&program_id, chain_c, &p, 1);

    let mut adopt_data = Vec::with_capacity(1 + 1 + 1 + 1 + 2 + peer_body_adopt.len());
    adopt_data.push(IxDiscriminator::RegisterPeer as u8);
    adopt_data.push(harness.guardian_set_bump);
    adopt_data.push(own_hub_p_bump);
    adopt_data.push(peer_c_p_a_bump);
    adopt_data.extend_from_slice(&(peer_body_adopt.len() as u16).to_le_bytes());
    adopt_data.extend_from_slice(&peer_body_adopt);
    let ix = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(harness.payer.pubkey(), true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(guardian_set_pda, false),
            AccountMeta::new_readonly(adopt_sigs_pubkey, false),
            AccountMeta::new_readonly(peer_hub_for_adopt_pda, false),
            AccountMeta::new(own_hub_p_pda, false),
            AccountMeta::new(peer_c_p_a_pda, false),
            AccountMeta::new(adopt_bucket, false),
            AccountMeta::new_readonly(NOREPLAY_PROGRAM_ID, false),
            AccountMeta::new_readonly(noreplay_authority, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: adopt_data,
    };
    let sig = harness.send(ix);
    eprintln!("[lifecycle-e2e] register_peer (adopt) tx={sig}");

    let own_hub_p_account = rpc.get_account(&own_hub_p_pda).expect("P's hub PDA exists");
    let own_hub_p_layout: &TransceiverHubLayout = bytemuck::from_bytes(&own_hub_p_account.data);
    assert_eq!(own_hub_p_layout.hub_chain, chain_a, "P adopted T1's hub chain");
    assert_eq!(own_hub_p_layout.hub_address, t1, "P adopted T1's hub address");

    // ---- (3) register_peer: T1 (chain A) registers P as its peer on chain C
    //          — MATCH branch (T1's hub already equals P's hub) ----
    let peer_body_match = build_peer_body(chain_a, &t1, 2, chain_c, &p);
    let match_sigs_pubkey = harness.post_signatures_for(&peer_body_match);
    let (peer_hub_for_match_pda, _) = derive_hub_pda(&program_id, chain_c, &p); // P's hub (peer's hub)
    let (own_hub_t1_pda, own_hub_t1_bump) = derive_hub_pda(&program_id, chain_a, &t1);
    let (peer_a_t1_c_pda, peer_a_t1_c_bump) = derive_peer_pda(&program_id, chain_a, &t1, chain_c);
    let match_bucket = derive_noreplay_bucket(&program_id, chain_a, &t1, 2);

    let mut match_data = Vec::with_capacity(1 + 1 + 1 + 1 + 2 + peer_body_match.len());
    match_data.push(IxDiscriminator::RegisterPeer as u8);
    match_data.push(harness.guardian_set_bump);
    match_data.push(own_hub_t1_bump);
    match_data.push(peer_a_t1_c_bump);
    match_data.extend_from_slice(&(peer_body_match.len() as u16).to_le_bytes());
    match_data.extend_from_slice(&peer_body_match);
    let ix = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(harness.payer.pubkey(), true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(guardian_set_pda, false),
            AccountMeta::new_readonly(match_sigs_pubkey, false),
            AccountMeta::new_readonly(peer_hub_for_match_pda, false),
            AccountMeta::new(own_hub_t1_pda, false),
            AccountMeta::new(peer_a_t1_c_pda, false),
            AccountMeta::new(match_bucket, false),
            AccountMeta::new_readonly(NOREPLAY_PROGRAM_ID, false),
            AccountMeta::new_readonly(noreplay_authority, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: match_data,
    };
    let sig = harness.send(ix);
    eprintln!("[lifecycle-e2e] register_peer (match) tx={sig}");

    let peer_account = rpc.get_account(&peer_a_t1_c_pda).expect("T1's peer PDA exists");
    let peer_layout: &TransceiverPeerLayout = bytemuck::from_bytes(&peer_account.data);
    assert_eq!(peer_layout.peer_address, p, "T1's peer on chain C is P");

    // ---- (4) register_relayer_chain: register relayer R for chain A ----
    let mut r = [0u8; 32];
    r[31] = 0x03;
    let relayer_body = build_register_relayer_chain_body(3, 0, chain_a, &r);
    let relayer_sigs_pubkey = harness.post_signatures_for(&relayer_body);
    let (relayer_pda, relayer_bump) = derive_relayer_registration_pda(&program_id, chain_a);
    let relayer_bucket = derive_noreplay_bucket(&program_id, SOLANA_CHAIN_ID, &GOVERNANCE_EMITTER, 3);

    let mut relayer_data = Vec::with_capacity(1 + 1 + 1 + 2 + relayer_body.len());
    relayer_data.push(IxDiscriminator::RegisterRelayerChain as u8);
    relayer_data.push(harness.guardian_set_bump);
    relayer_data.push(relayer_bump);
    relayer_data.extend_from_slice(&(relayer_body.len() as u16).to_le_bytes());
    relayer_data.extend_from_slice(&relayer_body);
    let ix = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(harness.payer.pubkey(), true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(guardian_set_pda, false),
            AccountMeta::new_readonly(relayer_sigs_pubkey, false),
            AccountMeta::new(relayer_pda, false),
            AccountMeta::new(relayer_bucket, false),
            AccountMeta::new_readonly(NOREPLAY_PROGRAM_ID, false),
            AccountMeta::new_readonly(noreplay_authority, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: relayer_data,
    };
    let sig = harness.send(ix);
    eprintln!("[lifecycle-e2e] register_relayer_chain tx={sig}");

    let relayer_account = rpc.get_account(&relayer_pda).expect("relayer registration PDA exists");
    let relayer_layout: &RelayerChainRegistrationLayout = bytemuck::from_bytes(&relayer_account.data);
    assert_eq!(relayer_layout.chain, chain_a);
    assert_eq!(relayer_layout.emitter_address, r);

    // ---- (5) submit_vaas: a relayer-wrapped transfer from T1 (chain A) to
    //          chain C, hub-substituted onto T1's own hub identity
    //          (native lock on the source, wrapped mint on the dest — both
    //          credit, so no pre-funding is required) ----
    let ntt_payload = build_ntt_message(3, 1000, chain_c); // 1000 @ 3 decimals -> 1e8 normalized
    let delivery = build_delivery_instruction(t1, &ntt_payload);
    let transfer_body = build_transfer_body(chain_a, &r, 4, &delivery); // emitter = relayer R
    let transfer_sigs_pubkey = harness.post_signatures_for(&transfer_body);
    let transfer_bucket = derive_noreplay_bucket(&program_id, chain_a, &r, 4);
    let (source_balance_pda, _) = derive_balance_pda(&program_id, chain_a, chain_a, &t1);
    let (dest_balance_pda, _) = derive_balance_pda(&program_id, chain_c, chain_a, &t1);

    // Reverse-peer PDA `(chain_c, p, chain_a)` — registered in step (2) as a
    // side effect of P's adopt call (`register_peer` always writes the peer
    // PDA for the `(emitter_chain, emitter_address, dest_chain)` it was
    // called with; P's adopt call was keyed on `dest_chain = chain_a`).
    let (peer_c_p_a_reverse_pda, _) = derive_peer_pda(&program_id, chain_c, &p, chain_a);

    let mut submit_data = Vec::with_capacity(1 + 1 + 2 + transfer_body.len());
    submit_data.push(IxDiscriminator::SubmitVaas as u8);
    submit_data.push(harness.guardian_set_bump);
    submit_data.extend_from_slice(&(transfer_body.len() as u16).to_le_bytes());
    submit_data.extend_from_slice(&transfer_body);
    let ix = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(harness.payer.pubkey(), true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(guardian_set_pda, false),
            AccountMeta::new_readonly(transfer_sigs_pubkey, false),
            AccountMeta::new(transfer_bucket, false),
            AccountMeta::new_readonly(NOREPLAY_PROGRAM_ID, false),
            AccountMeta::new_readonly(noreplay_authority, false),
            AccountMeta::new_readonly(system_program::ID, false),
            AccountMeta::new_readonly(relayer_pda, false),
            AccountMeta::new_readonly(hub_t1_pda, false),
            AccountMeta::new_readonly(peer_a_t1_c_pda, false),
            AccountMeta::new_readonly(peer_c_p_a_reverse_pda, false),
            AccountMeta::new(source_balance_pda, false),
            AccountMeta::new(dest_balance_pda, false),
        ],
        data: submit_data,
    };
    let sig = harness.send(ix);
    eprintln!("[lifecycle-e2e] submit_vaas tx={sig}");

    let source_account = rpc.get_account(&source_balance_pda).expect("source balance PDA exists");
    assert_eq!(source_account.owner, program_id);
    let source_layout: &BalanceAccountLayout = bytemuck::from_bytes(&source_account.data);
    assert_eq!(
        source_layout.balance,
        Uint256::from_u128(100_000_000),
        "source credited the normalized amount (native lock, hub_chain == emitter_chain)"
    );

    let dest_account = rpc.get_account(&dest_balance_pda).expect("dest balance PDA exists");
    let dest_layout: &BalanceAccountLayout = bytemuck::from_bytes(&dest_account.data);
    assert_eq!(
        dest_layout.balance,
        Uint256::from_u128(100_000_000),
        "dest credited the normalized amount (wrapped mint)"
    );

    // ---- (6) modify_balance: governance correction on the dest balance ----
    let delta = Uint256::from_u128(500);
    let reason = *b"e2e-lifecycle post-incident add ";
    let modify_body = build_modify_balance_body(5, 900, chain_c, chain_a, &t1, 1, delta, &reason);
    let modify_sigs_pubkey = harness.post_signatures_for(&modify_body);
    let (modification_pda, _) = derive_modification_pda(&program_id, 900);

    let mut modify_data = Vec::with_capacity(1 + 1 + 2 + modify_body.len());
    modify_data.push(IxDiscriminator::ModifyBalance as u8);
    modify_data.push(harness.guardian_set_bump);
    modify_data.extend_from_slice(&(modify_body.len() as u16).to_le_bytes());
    modify_data.extend_from_slice(&modify_body);
    let ix = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(harness.payer.pubkey(), true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(guardian_set_pda, false),
            AccountMeta::new_readonly(modify_sigs_pubkey, false),
            AccountMeta::new(dest_balance_pda, false),
            AccountMeta::new_readonly(system_program::ID, false),
            AccountMeta::new(modification_pda, false),
        ],
        data: modify_data,
    };
    let sig = harness.send(ix);
    eprintln!("[lifecycle-e2e] modify_balance tx={sig}");

    let dest_account_after = rpc
        .get_account_with_commitment(&dest_balance_pda, CommitmentConfig::confirmed())
        .expect("dest balance PDA fetch")
        .value
        .expect("dest balance PDA exists after modify_balance");
    let dest_layout_after: &BalanceAccountLayout = bytemuck::from_bytes(&dest_account_after.data);
    assert_eq!(
        dest_layout_after.balance,
        Uint256::from_u128(100_000_500),
        "dest balance = 1e8 (transfer) + 500 (governance Add)"
    );

    let modification_account = rpc.get_account(&modification_pda).expect("modification PDA exists");
    assert_eq!(modification_account.owner, program_id);
    let modification_layout: &ModifyBalanceLayout = bytemuck::from_bytes(&modification_account.data);
    assert_eq!(modification_layout.sequence, 900);
    assert_eq!(modification_layout.amount, delta);

    // ---- (7) Negative path, live against the validator: replaying the exact
    //          same modify_balance VAA collides on the now-existing
    //          Modification PDA (keyed on the payload's sequence, 900) and is
    //          rejected `DuplicateModifyBalance` — a real on-chain rejection,
    //          not a mollusk-only assertion. The GuardianSignatures account
    //          posted in step (6) is read-only to this instruction and was
    //          never closed, so it remains valid for a second verify. ----
    let mut replay_data = Vec::with_capacity(1 + 1 + 2 + modify_body.len());
    replay_data.push(IxDiscriminator::ModifyBalance as u8);
    replay_data.push(harness.guardian_set_bump);
    replay_data.extend_from_slice(&(modify_body.len() as u16).to_le_bytes());
    replay_data.extend_from_slice(&modify_body);
    let replay_ix = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(harness.payer.pubkey(), true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(guardian_set_pda, false),
            AccountMeta::new_readonly(modify_sigs_pubkey, false),
            AccountMeta::new(dest_balance_pda, false),
            AccountMeta::new_readonly(system_program::ID, false),
            AccountMeta::new(modification_pda, false),
        ],
        data: replay_data,
    };
    let code = harness.send_expect_custom_error(replay_ix);
    assert_eq!(
        code,
        GlobalAccountantError::DuplicateModifyBalance as u32,
        "replaying the modify_balance VAA must reject DuplicateModifyBalance, got code {code}"
    );
    eprintln!("[lifecycle-e2e] modify_balance replay correctly rejected: DuplicateModifyBalance");

    // Dest balance must be unchanged by the rejected replay.
    let dest_account_final = rpc
        .get_account_with_commitment(&dest_balance_pda, CommitmentConfig::confirmed())
        .expect("dest balance PDA fetch")
        .value
        .expect("dest balance PDA exists after rejected replay");
    let dest_layout_final: &BalanceAccountLayout = bytemuck::from_bytes(&dest_account_final.data);
    assert_eq!(
        dest_layout_final.balance,
        Uint256::from_u128(100_000_500),
        "dest balance unchanged by the rejected replay"
    );

    eprintln!("[lifecycle-e2e] full lifecycle green: register_hub -> register_peer (x2) -> register_relayer_chain -> submit_vaas -> modify_balance -> rejected replay (DuplicateModifyBalance)");
}
