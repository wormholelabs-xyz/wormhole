//! Surfpool E2E against the Solana mainnet fork: drives the DigestAccount
//! close path through the real Verify VAA Shim CPI. The Shim, Core Bridge, and
//! active `GuardianSet` PDA lazy-fetch from the mainnet datasource; the test
//! deploys our `.so` and posts the historical VAA's signatures.
//!
//! # Run
//!
//! ```sh
//! just test-e2e-mainnet-fork
//! ```
//!
//! Spawns a surfpool subprocess and lazy-fetches mainnet, so both tests are
//! `#[ignore]`. Datasource defaults to `https://api.mainnet-beta.solana.com`;
//! override with `GA_E2E_DATASOURCE_RPC=<url>` (the labsapis proxy needs an
//! `Origin` header surfpool v1.2.1 cannot inject).
//!
//! The production-shape `.so` has `mock-vaa`/`test-only-open-digest` off (paired
//! feature fence in `src/lib.rs`), so `open_digest` is undispatched here; the
//! DigestAccount PDA is materialised via `surfnet_setAccount` with the exact
//! byte image `open_digest` would have produced.
//!
//! # Fixture
//!
//! `tests/fixtures/mainnet_solana_token_bridge_seq2211.vaa` (1132 bytes).
//!
//! | field                 | value                                                              |
//! |-----------------------|--------------------------------------------------------------------|
//! | emitter_chain         | 1 (Solana)                                                         |
//! | emitter_address       | `4385cebf45845f3a162f42c96a3dfe696b7eb8368f1af1e7613f870af36f1fc6` |
//! | emitter_native        | `5Yaf3N7MAEThp5FBBjUri8rv9mWxFEiJBjTKYYeKEi37`                     |
//! | sequence              | 2211                                                               |
//! | guardian_set_index    | 6 (currently active)                                               |
//! | signatures            | 13 (quorum exactly)                                                |
//! | body digest (keccak²) | `e4cac284656ac74ad4ef1b0ec7c2be76289705458071c7ddbef805499a054116` |
//!
//! Chosen because guardian set 6 is live, so its `GuardianSet` PDA
//! (`qHpgKQfi2166hrKgLPBJxdJwTzwq4D14g3D4i4eU5TK`, bump 254) is unexpired and
//! lazy-fetches cleanly.

use std::time::{Duration, Instant};

use global_accountant_definitions::{
    DigestAccountLayout, Instruction as IxDiscriminator, DIGEST_SEED_PREFIX,
    VERIFY_VAA_SHIM_PROGRAM_ID,
};
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_system_interface::program as system_program;
use solana_transaction::Transaction;

mod common;
use common::{await_confirmed, deploy_program, so_path, start_surfpool, SurfpoolOptions};

/// Wormhole Core Bridge program ID on Solana mainnet
/// (`worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth`), pinned as raw bytes.
const CORE_BRIDGE_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
    0x0e, 0x0a, 0x58, 0x9a, 0x41, 0xa5, 0x5f, 0xbd, 0x66, 0xc5, 0x2a, 0x47, 0x5f, 0x2d, 0x92, 0xa6,
    0xd3, 0xdc, 0x9b, 0x47, 0x47, 0x11, 0x4c, 0xb9, 0xaf, 0x82, 0x5a, 0x98, 0xb5, 0x45, 0xd3, 0xce,
]);

/// Anchor discriminator for the Shim's `post_signatures` instruction
/// (`sha256("global:post_signatures")[..8]`).
const POST_SIGNATURES_SELECTOR: [u8; 8] = [0x8a, 0x02, 0x35, 0xa6, 0x2d, 0x4d, 0x89, 0x33];

/// Compute Budget program ID (`ComputeBudget111111111111111111111111111111`).
const COMPUTE_BUDGET_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
    0x03, 0x06, 0x46, 0x6f, 0xe5, 0x21, 0x17, 0x32, 0xff, 0xec, 0xad, 0xba, 0x72, 0xc3, 0x9b, 0xe7,
    0xbc, 0x8c, 0xe5, 0xbb, 0xc5, 0xf7, 0x12, 0x6b, 0x2c, 0x43, 0x9b, 0x3a, 0x40, 0x00, 0x00, 0x00,
]);

/// CU ceiling for `close_digest`. The Shim's 13-sig `VerifyHash` burns
/// ~198_787 CU; 400_000 leaves headroom without exceeding the per-block cap.
const CLOSE_DIGEST_CU_LIMIT: u32 = 400_000;

/// One guardian signature record: 1-byte index + 64-byte r||s + 1-byte rec id.
const GUARDIAN_SIGNATURE_LENGTH: usize = 66;

/// Embedded VAA bytes (inline to keep the test single-binary).
const VAA_BYTES: &[u8] = include_bytes!("fixtures/mainnet_solana_token_bridge_seq2211.vaa");

/// Pre-decoded VAA metadata (hardcoded so fixture corruption surfaces early).
const EMITTER_CHAIN: u16 = 1;
const SEQUENCE: u64 = 2211;
const GUARDIAN_SET_INDEX: u32 = 6;
const NUM_SIGNATURES: usize = 13;
const EMITTER_ADDRESS: [u8; 32] = [
    0x43, 0x85, 0xce, 0xbf, 0x45, 0x84, 0x5f, 0x3a, 0x16, 0x2f, 0x42, 0xc9, 0x6a, 0x3d, 0xfe, 0x69,
    0x6b, 0x7e, 0xb8, 0x36, 0x8f, 0x1a, 0xf1, 0xe7, 0x61, 0x3f, 0x87, 0x0a, 0xf3, 0x6f, 0x1f, 0xc6,
];
const EXPECTED_DIGEST: [u8; 32] = [
    0xe4, 0xca, 0xc2, 0x84, 0x65, 0x6a, 0xc7, 0x4a, 0xd4, 0xef, 0x1b, 0x0e, 0xc7, 0xc2, 0xbe, 0x76,
    0x28, 0x97, 0x05, 0x45, 0x80, 0x71, 0xc7, 0xdd, 0xbe, 0xf8, 0x05, 0x49, 0x9a, 0x05, 0x41, 0x16,
];

/// Active mainnet `GuardianSet` PDA for index 6
/// (`qHpgKQfi2166hrKgLPBJxdJwTzwq4D14g3D4i4eU5TK`, bump 254).
const GUARDIAN_SET_PDA: Pubkey = Pubkey::new_from_array([
    0x0c, 0x5e, 0xe6, 0x4a, 0x1d, 0x73, 0x0f, 0xc8, 0xfc, 0x2b, 0xe7, 0x1f, 0xea, 0xa6, 0x34, 0x78,
    0xee, 0xc5, 0x1d, 0xf3, 0x82, 0x08, 0x20, 0x6e, 0x6c, 0x96, 0xa1, 0xc3, 0xcf, 0xef, 0x94, 0xae,
]);
const GUARDIAN_SET_BUMP: u8 = 254;

/// Datasource URL for surfpool's mainnet fork; `GA_E2E_DATASOURCE_RPC` overrides.
fn datasource_rpc_url() -> String {
    std::env::var("GA_E2E_DATASOURCE_RPC")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string())
}

fn derive_digest_pda(
    program_id: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    let sequence_be = sequence.to_be_bytes();
    Pubkey::find_program_address(
        &[DIGEST_SEED_PREFIX, &chain_be, emitter, &sequence_be],
        program_id,
    )
}

/// `SetComputeUnitLimit` instruction. Wire format: `[0x02, u32_le_units]`.
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

fn close_digest_ix_data(digest: &[u8; 32], guardian_set_bump: u8) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 32 + 1);
    data.push(IxDiscriminator::CloseDigest as u8);
    data.extend_from_slice(digest);
    data.push(guardian_set_bump);
    data
}

/// `post_signatures` instruction data. Wire format after the 8-byte selector:
///
/// | offset | size  | field                            |
/// |--------|-------|----------------------------------|
/// | 0      | 4     | guardian_set_index (LE)          |
/// | 4      | 1     | total_signatures (u8)            |
/// | 5      | 4     | guardian_signatures_len (LE)     |
/// | 9..    | 66*N  | guardian_signatures (contiguous) |
fn post_signatures_ix_data(
    guardian_set_index: u32,
    total_signatures: u8,
    guardian_signatures: &[u8],
) -> Vec<u8> {
    assert!(
        guardian_signatures.len() % GUARDIAN_SIGNATURE_LENGTH == 0,
        "guardian_signatures length must be a multiple of {GUARDIAN_SIGNATURE_LENGTH}"
    );
    let count = guardian_signatures.len() / GUARDIAN_SIGNATURE_LENGTH;
    let mut data = Vec::with_capacity(8 + 9 + guardian_signatures.len());
    data.extend_from_slice(&POST_SIGNATURES_SELECTOR);
    data.extend_from_slice(&guardian_set_index.to_le_bytes());
    data.push(total_signatures);
    data.extend_from_slice(&(count as u32).to_le_bytes());
    data.extend_from_slice(guardian_signatures);
    data
}

/// Post all 13 signatures to a fresh `GuardianSignatures` PDA in one tx
/// (13 × 66 = 858 bytes fits the 1232-byte envelope).
fn post_signatures(
    rpc: &solana_client::rpc_client::RpcClient,
    payer: &Keypair,
    guardian_signatures_kp: &Keypair,
    guardian_signatures_slice: &[u8],
) {
    let shim_program_id = Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID);
    let ix_data = post_signatures_ix_data(
        GUARDIAN_SET_INDEX,
        NUM_SIGNATURES as u8,
        guardian_signatures_slice,
    );
    let ix = Instruction {
        program_id: shim_program_id,
        accounts: vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(guardian_signatures_kp.pubkey(), true),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: ix_data,
    };

    let blockhash = rpc.get_latest_blockhash().expect("blockhash post_sigs");
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[payer, guardian_signatures_kp],
        blockhash,
    );
    let sig = rpc
        .send_and_confirm_transaction(&tx)
        .expect("PostSignatures send_and_confirm");
    eprintln!("[e2e] PostSignatures tx={sig}");
}

/// Rent-exempt balance for a 120-byte `DigestAccountLayout`
/// (`Rent::default().minimum_balance(120)`).
const DIGEST_PDA_RENT_LAMPORTS: u64 = 1_572_960;

/// Live state after `boot_and_seed`: surfpool booted, program deployed, payer
/// funded, signatures posted, DigestAccount PDA seeded.
struct Fixture {
    _guard: common::SurfpoolGuard,
    rpc: solana_client::rpc_client::RpcClient,
    rpc_url: String,
    payer: Keypair,
    program_id: Pubkey,
    digest_pda: Pubkey,
    guardian_signatures_pda: Pubkey,
}

/// Inject a `DigestAccountLayout` PDA via `surfnet_setAccount`, replacing the
/// undispatched `open_digest`. Cheatcode shape (surfpool v1.2.1):
/// `[<pubkey_b58>, {lamports, owner, executable, rent_epoch, data}]` where
/// `data` is a bare hex string (not the `getAccountInfo` base64 tuple).
fn write_digest_pda(
    rpc_url: &str,
    program_id: &Pubkey,
    digest_pda: &Pubkey,
    digest: [u8; 32],
    payer: Pubkey,
) {
    // Zeroable + field assignment since `_padding` is `pub(crate)`. Matches
    // open_digest's output (bar `quorum_at_slot`, which close_digest ignores).
    let mut layout: DigestAccountLayout = bytemuck::Zeroable::zeroed();
    layout.emitter = EMITTER_ADDRESS;
    layout.digest = digest;
    layout.payer = payer.to_bytes();
    layout.sequence = SEQUENCE;
    layout.quorum_at_slot = 0;
    layout.guardian_set_index = GUARDIAN_SET_INDEX;
    layout.chain = EMITTER_CHAIN;
    let bytes = bytemuck::bytes_of(&layout);
    assert_eq!(bytes.len(), DigestAccountLayout::LEN);

    let resp = common::rpc_call(
        rpc_url,
        "surfnet_setAccount",
        serde_json::json!([
            digest_pda.to_string(),
            {
                "lamports": DIGEST_PDA_RENT_LAMPORTS,
                "owner": program_id.to_string(),
                "executable": false,
                "rent_epoch": 0u64,
                "data": common::hex_encode(bytes),
            }
        ]),
    );
    assert!(
        resp.get("error").is_none(),
        "surfnet_setAccount failed: {resp}"
    );
    eprintln!("[e2e] surfnet_setAccount OK for {digest_pda}");
}

fn boot_and_seed(stored_digest: [u8; 32]) -> Fixture {
    let so = so_path("global_accountant");
    let so_bytes = std::fs::read(&so).unwrap_or_else(|e| {
        panic!(
            "could not read {}: {e}. Run `just build-prod` first (this test \
             deploys the production-shape `.so` with the real Verify VAA Shim CPI).",
            so.display()
        )
    });
    eprintln!(
        "[e2e] loaded {} bytes from {}",
        so_bytes.len(),
        so.display()
    );

    let guard = start_surfpool(SurfpoolOptions::mainnet_fork(
        "ga-surfpool-e2e-mfork",
        datasource_rpc_url(),
    ));
    let rpc_url = guard.rpc_url();
    let rpc = guard.rpc_client();

    let program_kp = Keypair::new();
    let program_id = program_kp.pubkey();
    let payer = Keypair::new();
    let guardian_signatures_kp = Keypair::new();
    eprintln!(
        "[e2e] program_id={program_id} payer={} guardian_sigs={}",
        payer.pubkey(),
        guardian_signatures_kp.pubkey(),
    );

    // Fund payer.
    let airdrop_sig = rpc
        .request_airdrop(&payer.pubkey(), 10_000_000_000)
        .expect("request_airdrop");
    await_confirmed("airdrop", Duration::from_secs(10), || {
        rpc.confirm_transaction(&airdrop_sig)
    });
    assert!(
        rpc.get_balance(&payer.pubkey())
            .expect("payer balance after airdrop")
            >= 10_000_000_000,
        "payer funded"
    );

    // Deploy our production-shape program.
    deploy_program(&rpc_url, &program_id, &so_bytes);
    let acct = rpc.get_account(&program_id).expect("program account");
    assert!(acct.executable, "deployed program is executable");

    // Lazy-fetch the Shim, Core Bridge, and GuardianSet PDA up front.
    let shim_program_id = Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID);
    let shim_acct = rpc
        .get_account(&shim_program_id)
        .expect("Verify VAA Shim account lazy-fetch");
    assert!(shim_acct.executable, "Shim is executable");
    let core_acct = rpc
        .get_account(&CORE_BRIDGE_PROGRAM_ID)
        .expect("Core Bridge account lazy-fetch");
    assert!(core_acct.executable, "Core Bridge is executable");
    let gset_acct = rpc
        .get_account(&GUARDIAN_SET_PDA)
        .expect("GuardianSet PDA lazy-fetch");
    eprintln!(
        "[e2e] GuardianSet idx={} data_len={} owner={}",
        GUARDIAN_SET_INDEX,
        gset_acct.data.len(),
        gset_acct.owner
    );
    assert_eq!(
        gset_acct.owner, CORE_BRIDGE_PROGRAM_ID,
        "GuardianSet owner == Core Bridge"
    );

    // Post the 13 signatures into a fresh GuardianSignatures PDA.
    let sigs_slice = &VAA_BYTES[6..6 + NUM_SIGNATURES * GUARDIAN_SIGNATURE_LENGTH];
    let post_start = Instant::now();
    post_signatures(&rpc, &payer, &guardian_signatures_kp, sigs_slice);
    eprintln!("[e2e] PostSignatures elapsed: {:?}", post_start.elapsed());

    let gs_acct = rpc
        .get_account_with_commitment(
            &guardian_signatures_kp.pubkey(),
            CommitmentConfig::confirmed(),
        )
        .expect("GuardianSignatures lookup")
        .value
        .expect("GuardianSignatures account materialised");
    assert_eq!(gs_acct.owner, shim_program_id, "GS owned by Shim");
    eprintln!(
        "[e2e] GuardianSignatures lamports={} data_len={}",
        gs_acct.lamports,
        gs_acct.data.len()
    );

    // Seed the DigestAccount PDA via cheatcode (open_digest is undispatched).
    let (digest_pda, _bump) =
        derive_digest_pda(&program_id, EMITTER_CHAIN, &EMITTER_ADDRESS, SEQUENCE);
    write_digest_pda(
        &rpc_url,
        &program_id,
        &digest_pda,
        stored_digest,
        payer.pubkey(),
    );

    // Spot-check the injected layout.
    let pda_acct = rpc.get_account(&digest_pda).expect("digest pda");
    assert_eq!(pda_acct.owner, program_id, "PDA owner == program_id");
    assert_eq!(
        pda_acct.data.len(),
        DigestAccountLayout::LEN,
        "PDA len == DigestAccountLayout::LEN"
    );
    let stored: &DigestAccountLayout = bytemuck::from_bytes(&pda_acct.data);
    assert_eq!(stored.digest, stored_digest);
    assert_eq!(stored.payer, payer.pubkey().to_bytes());
    assert_eq!(stored.chain, EMITTER_CHAIN);
    assert_eq!(stored.sequence, SEQUENCE);
    assert_eq!(stored.emitter, EMITTER_ADDRESS);

    Fixture {
        _guard: guard,
        rpc,
        rpc_url,
        payer,
        program_id,
        digest_pda,
        guardian_signatures_pda: guardian_signatures_kp.pubkey(),
    }
}

/// Build the `close_digest` Instruction; caller supplies the digest in the data.
fn build_close_ix(
    program_id: Pubkey,
    payer: Pubkey,
    digest_pda: Pubkey,
    guardian_signatures_pda: Pubkey,
    digest_in_data: [u8; 32],
) -> Instruction {
    let shim_program_id = Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID);
    Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new_readonly(payer, true), // closer
            AccountMeta::new(digest_pda, false),
            AccountMeta::new(payer, false), // rent recipient
            AccountMeta::new_readonly(guardian_signatures_pda, false), // GS PDA
            AccountMeta::new_readonly(GUARDIAN_SET_PDA, false), // GuardianSet PDA
            AccountMeta::new_readonly(shim_program_id, false), // CPI target
        ],
        data: close_digest_ix_data(&digest_in_data, GUARDIAN_SET_BUMP),
    }
}

/// Happy path: close_digest's real CPI verifies against the live guardian set.
#[test]
#[ignore = "spawns surfpool subprocess + lazy-fetches mainnet; run via \
            `just test-e2e-mainnet-fork` or `cargo test -- --ignored`"]
fn close_digest_with_real_cpi_against_mainnet_fork_succeeds() {
    let fx = boot_and_seed(EXPECTED_DIGEST);

    let payer_after_open = fx
        .rpc
        .get_balance(&fx.payer.pubkey())
        .expect("payer balance after open");

    let close_ix = build_close_ix(
        fx.program_id,
        fx.payer.pubkey(),
        fx.digest_pda,
        fx.guardian_signatures_pda,
        EXPECTED_DIGEST,
    );
    let blockhash = fx.rpc.get_latest_blockhash().expect("blockhash close");
    let close_tx = Transaction::new_signed_with_payer(
        // CU limit first; VerifyHash exceeds the 200k default.
        &[set_compute_unit_limit_ix(CLOSE_DIGEST_CU_LIMIT), close_ix],
        Some(&fx.payer.pubkey()),
        &[&fx.payer],
        blockhash,
    );

    let close_sig = fx
        .rpc
        .send_and_confirm_transaction(&close_tx)
        .expect("close_digest with real CPI must succeed");
    eprintln!("[e2e] close_digest tx={close_sig}");

    // Raw RPC for the tx so logs + CU land in test output without a
    // transaction-status dev-dep.
    let resp = common::rpc_call(
        &fx.rpc_url,
        "getTransaction",
        serde_json::json!([
            close_sig.to_string(),
            { "encoding": "json", "commitment": "confirmed", "maxSupportedTransactionVersion": 0 }
        ]),
    );
    if let Some(meta) = resp.pointer("/result/meta") {
        if let Some(logs) = meta.pointer("/logMessages").and_then(|v| v.as_array()) {
            eprintln!("[e2e] close_digest program logs ({} lines):", logs.len());
            for line in logs {
                if let Some(s) = line.as_str() {
                    eprintln!("  {s}");
                }
            }
        }
        if let Some(cu) = meta.pointer("/computeUnitsConsumed") {
            eprintln!("[e2e] close_digest computeUnitsConsumed: {cu}");
        }
    }

    // PDA gone or zeroed.
    match fx
        .rpc
        .get_account_with_commitment(&fx.digest_pda, CommitmentConfig::confirmed())
    {
        Ok(resp) => match resp.value {
            None => eprintln!("[e2e] DigestAccount fully closed (account does not exist)"),
            Some(acct) => {
                assert_eq!(acct.lamports, 0, "PDA lamports drained");
                assert_eq!(acct.owner, system_program::ID, "PDA reassigned to system");
                assert!(acct.data.is_empty(), "PDA data dropped");
            }
        },
        Err(e) => panic!("get_account after close: {e}"),
    }

    let payer_after_close = fx
        .rpc
        .get_balance(&fx.payer.pubkey())
        .expect("payer balance after close");
    assert!(
        payer_after_close > payer_after_open,
        "rent flowed back to payer: before={payer_after_open} after={payer_after_close}"
    );
    eprintln!("[e2e] payer balance: after_open={payer_after_open} after_close={payer_after_close}",);
}

/// Negative path: a tampered stored digest passes close_digest's equality check
/// but the Shim CPI rejects it at signature recovery; PDA is left intact.
#[test]
#[ignore = "spawns surfpool subprocess + lazy-fetches mainnet; run via \
            `just test-e2e-mainnet-fork` or `cargo test -- --ignored`"]
fn close_digest_with_tampered_digest_fails_at_cpi() {
    let mut tampered = EXPECTED_DIGEST;
    tampered[31] ^= 0x01;

    let fx = boot_and_seed(tampered);
    let close_ix = build_close_ix(
        fx.program_id,
        fx.payer.pubkey(),
        fx.digest_pda,
        fx.guardian_signatures_pda,
        tampered,
    );
    let blockhash = fx.rpc.get_latest_blockhash().expect("blockhash close-bad");
    let close_tx = Transaction::new_signed_with_payer(
        // Same CU bump so the rejection is the signature mismatch, not a budget error.
        &[set_compute_unit_limit_ix(CLOSE_DIGEST_CU_LIMIT), close_ix],
        Some(&fx.payer.pubkey()),
        &[&fx.payer],
        blockhash,
    );

    let err = fx
        .rpc
        .send_and_confirm_transaction(&close_tx)
        .expect_err("close_digest with tampered digest must be rejected by the Shim CPI");
    eprintln!("[e2e] close_digest rejected as expected: {err}");

    // PDA still exists — failed tx leaves state untouched.
    let pda_acct = fx
        .rpc
        .get_account_with_commitment(&fx.digest_pda, CommitmentConfig::confirmed())
        .expect("get_account after failed close")
        .value
        .expect("DigestAccount preserved after failed close");
    assert_eq!(pda_acct.owner, fx.program_id, "PDA still program-owned");
    assert_eq!(
        pda_acct.data.len(),
        DigestAccountLayout::LEN,
        "PDA size preserved"
    );

    // Keep the field live to silence unused-field warnings.
    let _ = &fx.rpc_url;
}
