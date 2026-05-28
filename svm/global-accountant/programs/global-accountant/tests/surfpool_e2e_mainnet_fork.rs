//! Surfpool E2E test against the Solana mainnet fork.
//!
//! Drives the full DigestAccount lifecycle through the real Verify VAA Shim
//! CPI. surfpool boots with a mainnet datasource so the Shim program, the
//! Core Bridge, and the active `GuardianSet` PDA are lazy-fetched from
//! upstream; the test only has to (a) deploy our `.so` and (b) build a
//! `GuardianSignatures` PDA via a `PostSignatures` tx for the historical VAA.
//!
//! # Run
//!
//! ```sh
//! # From svm/global-accountant/
//! make test-e2e-mainnet-fork
//! # Or manually:
//! cargo build-sbf --features bpf-entrypoint                  # prod-shape .so
//! cargo build-sbf --features bpf-entrypoint,mock-vaa,test-only-open-digest
//! SBF_OUT_DIR=$(pwd)/target/deploy \
//!   cargo test --features mock-vaa,test-only-open-digest \
//!   --test surfpool_e2e_mainnet_fork -- --ignored --nocapture
//! ```
//!
//! ## Datasource selection
//!
//! Default is `https://api.mainnet-beta.solana.com` (the canonical entry from
//! `w7-registry/chains/mainnet/solana.yaml`). The labsapis proxy
//! (`https://rpc.labsapis.com/mainnet/solana`) requires an `Origin:
//! https://portalbridge.com` header that surfpool v1.2.1 cannot inject on
//! datasource requests — there is no header flag on `surfpool start`, and the
//! `SURFPOOL_DATASOURCE_RPC_URL` env var carries only the URL. Override the
//! datasource by exporting `GA_E2E_DATASOURCE_RPC=<url>` before running, e.g.
//! a private endpoint that does not enforce CORS.
//!
//! # Why we use `surfnet_setAccount` instead of `open_digest`
//!
//! The program's paired-feature fence (`src/lib.rs`) makes `mock-vaa` and
//! `test-only-open-digest` mutually inclusive: a build either has both (mock
//! VAA + dispatchable `open_digest`) or neither (real CPI + no `open_digest`).
//! This test exercises the **real CPI** path, so the deployed `.so`
//! has both features off — which means `open_digest` is not in the program's
//! dispatch table and we cannot use it to materialise the DigestAccount PDA.
//!
//! Instead we use surfpool's `surfnet_setAccount` cheatcode to write the PDA
//! bytes (a `DigestAccountLayout`) directly into the simnet's account DB. The
//! production close path treats the result identically: same owner, same
//! `data_len`, same rent-exempt lamport balance, and (in the happy path) the
//! same digest the historical guardians signed.
//!
//! # What this test proves end-to-end
//!
//! 1. surfpool boots with mainnet fork; the Verify VAA Shim, the Core Bridge,
//!    and the active `GuardianSet` PDA all lazy-fetch cleanly.
//! 2. A `PostSignatures` tx writes the historical VAA's signatures into a
//!    fresh `GuardianSignatures` PDA owned by the Shim.
//! 3. `surfnet_setAccount` materialises a `DigestAccount` PDA seeded with the
//!    historical VAA's digest, payer pubkey, and metadata.
//! 4. `close_digest`'s real CPI (no `mock-vaa` feature) reaches the Shim's
//!    `VerifyHash`, the Shim recovers 13 guardian pubkeys against the digest,
//!    the recovered keys match the on-chain `GuardianSet`, and the close
//!    succeeds — rent flowing to the recorded payer.
//! 5. A tampered-digest variant: `close_digest` fails at the CPI step because
//!    the recovered pubkeys do not match the guardian set.
//!
//! # VAA fixture
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
//! | Wormholescan URL      | https://wormholescan.io/#/tx/1/.../2211 (per emitter+seq)          |
//!
//! Source: pulled from `https://api.wormholescan.io/api/v1/vaas?page=0&pageSize=3`
//! on 2026-05-19. Chosen because guardian set 6 is the live set today, so the
//! Core Bridge `GuardianSet` PDA at index 6
//! (`qHpgKQfi2166hrKgLPBJxdJwTzwq4D14g3D4i4eU5TK`, bump 254) is unexpired and
//! lazy-fetches cleanly from mainnet.

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
use common::{
    await_confirmed, deploy_program, so_path, start_surfpool, SurfpoolOptions,
};

/// Wormhole Core Bridge program ID on Solana mainnet
/// (`worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth`). Source: w7-registry
/// `deployments/mainnet/wormhole/core-bridge.yaml`. Vendored as a `Pubkey`
/// rather than a base58 string so the test does not pull in a base58 dev-dep.
const CORE_BRIDGE_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
    0x0e, 0x0a, 0x58, 0x9a, 0x41, 0xa5, 0x5f, 0xbd, 0x66, 0xc5, 0x2a, 0x47, 0x5f, 0x2d, 0x92, 0xa6,
    0xd3, 0xdc, 0x9b, 0x47, 0x47, 0x11, 0x4c, 0xb9, 0xaf, 0x82, 0x5a, 0x98, 0xb5, 0x45, 0xd3, 0xce,
]);

/// Anchor discriminator for the Verify VAA Shim's `post_signatures` instruction
/// (`sha256("global:post_signatures")[..8]`). Equal to the constant produced by
/// `make_anchor_discriminator(b"global:post_signatures")` in the shim source.
const POST_SIGNATURES_SELECTOR: [u8; 8] =
    [0x8a, 0x02, 0x35, 0xa6, 0x2d, 0x4d, 0x89, 0x33];

/// Compute Budget program ID (`ComputeBudget111111111111111111111111111111`).
/// Native program, address is the same on every cluster.
const COMPUTE_BUDGET_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
    0x03, 0x06, 0x46, 0x6f, 0xe5, 0x21, 0x17, 0x32, 0xff, 0xec, 0xad, 0xba, 0x72, 0xc3, 0x9b, 0xe7,
    0xbc, 0x8c, 0xe5, 0xbb, 0xc5, 0xf7, 0x12, 0x6b, 0x2c, 0x43, 0x9b, 0x3a, 0x40, 0x00, 0x00, 0x00,
]);

/// Compute-unit ceiling for the `close_digest` tx. The Shim's `VerifyHash`
/// burns ~198_787 CU recovering 13 secp256k1 pubkeys; bumping to 400_000 gives
/// our own pre/post-CPI bookkeeping plenty of headroom without inflating the
/// reservation past Solana's per-block ceiling.
const CLOSE_DIGEST_CU_LIMIT: u32 = 400_000;

/// Byte length of one guardian signature record on the wire (1-byte index +
/// 64-byte r||s + 1-byte recovery id). Mirrors
/// `wormhole_svm_definitions::GUARDIAN_SIGNATURE_LENGTH`.
const GUARDIAN_SIGNATURE_LENGTH: usize = 66;

/// Embedded VAA bytes. Including the file inline keeps the test single-binary
/// and avoids a runtime path lookup that would break under `cargo test`'s
/// per-binary cwd handling.
const VAA_BYTES: &[u8] = include_bytes!("fixtures/mainnet_solana_token_bridge_seq2211.vaa");

/// Pre-decoded VAA metadata. Hardcoded rather than re-parsed at runtime so the
/// test surfaces fixture corruption immediately rather than at the first
/// `PostSignatures` CU explosion.
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

/// Active mainnet `GuardianSet` PDA for index 6, derived from
/// `("GuardianSet", 6_u32_be)` under `CORE_BRIDGE_PROGRAM_ID`.
/// `qHpgKQfi2166hrKgLPBJxdJwTzwq4D14g3D4i4eU5TK`, bump 254.
const GUARDIAN_SET_PDA: Pubkey = Pubkey::new_from_array([
    0x0c, 0x5e, 0xe6, 0x4a, 0x1d, 0x73, 0x0f, 0xc8, 0xfc, 0x2b, 0xe7, 0x1f, 0xea, 0xa6, 0x34, 0x78,
    0xee, 0xc5, 0x1d, 0xf3, 0x82, 0x08, 0x20, 0x6e, 0x6c, 0x96, 0xa1, 0xc3, 0xcf, 0xef, 0x94, 0xae,
]);
const GUARDIAN_SET_BUMP: u8 = 254;

/// Datasource URL for surfpool's mainnet fork. Defaults to the public RPC from
/// `w7-registry/chains/mainnet/solana.yaml` because surfpool v1.2.1 has no way
/// to inject the `Origin` header the labsapis proxy requires. Override via
/// `GA_E2E_DATASOURCE_RPC` for endpoints that do not need custom headers.
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

/// Compute Budget program instruction discriminator for
/// `SetComputeUnitLimit`. Wire format: `[0x02, u32_le_units]`. Source:
/// `solana-sdk` `ComputeBudgetInstruction::SetComputeUnitLimit`.
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

/// Build the `post_signatures` instruction data for the Verify VAA Shim. Wire
/// format (after the 8-byte selector):
///
/// | offset | size  | field                       |
/// |--------|-------|-----------------------------|
/// | 0      | 4     | guardian_set_index (LE)     |
/// | 4      | 1     | total_signatures (u8)       |
/// | 5      | 4     | guardian_signatures_len (LE) |
/// | 9..    | 66*N  | guardian_signatures (contiguous)|
///
/// Mirrors `wormhole_svm_shim::verify_vaa::PostSignaturesData::to_vec` in
/// `svm/wormhole-core-shims/crates/shim/src/verify_vaa/mod.rs`.
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

/// Post all 13 historical signatures to a fresh `GuardianSignatures` PDA in one
/// transaction. The historical VAA fits inside one 1232-byte tx because the
/// quorum size is small enough (13 sigs × 66 bytes = 858 bytes) to clear the
/// envelope after instruction-data overhead.
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

/// Rent-exempt lamport balance for a 120-byte `DigestAccountLayout`. Computed
/// once via `Rent::default().minimum_balance(120)` (= `1_572_960`); pinned as
/// a const so the test does not need a `solana-rent` dev-dep.
const DIGEST_PDA_RENT_LAMPORTS: u64 = 1_572_960;

/// Common setup: boot surfpool with mainnet fork, deploy our program, fund
/// payer, post signatures, hand-craft the `DigestAccount` PDA via the
/// `surfnet_setAccount` cheatcode. Returns the live RPC client, payer,
/// program id, PDA, and the guardian-signatures keypair so the calling test
/// can drive the `close_digest` step.
struct Fixture {
    _guard: common::SurfpoolGuard,
    rpc: solana_client::rpc_client::RpcClient,
    rpc_url: String,
    payer: Keypair,
    program_id: Pubkey,
    digest_pda: Pubkey,
    guardian_signatures_pda: Pubkey,
}

/// Hand-build a `DigestAccountLayout` and inject it as a PDA owned by our
/// program via `surfnet_setAccount`. Replaces `open_digest` (which the
/// production-shape `.so` does not expose — see the module doc).
///
/// surfpool's `surfnet_setAccount` cheatcode shape (verified by experiment on
/// v1.2.1): a tuple `[<pubkey_b58>, <override-object>]`. The object carries
/// `lamports`, `owner`, `executable`, `rent_epoch`, and `data`. `data` is a
/// **bare hex string**, not the `[base64_str, "base64"]` tuple the standard
/// `getAccountInfo` response uses.
fn write_digest_pda(
    rpc_url: &str,
    program_id: &Pubkey,
    digest_pda: &Pubkey,
    digest: [u8; 32],
    payer: Pubkey,
) {
    // Construct via `Zeroable` + bytemuck mutation rather than a struct
    // literal because `_padding` is `pub(crate)` and inaccessible to test
    // code. The zero-initialised value is exactly the layout `open_digest`
    // would have produced for these inputs (modulo `quorum_at_slot`, which is
    // immaterial to `close_digest`'s checks).
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
            "could not read {}: {e}. Run `make build-prod` first (this test \
             deploys the production-shape `.so` with the real Verify VAA Shim CPI).",
            so.display()
        )
    });
    eprintln!("[e2e] loaded {} bytes from {}", so_bytes.len(), so.display());

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

    // Fund payer. The simnet's airdrop facility credits the account in the
    // next slot tick.
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

    // Deploy our program (production-shape — real CPI in `close_digest`).
    deploy_program(&rpc_url, &program_id, &so_bytes);
    let acct = rpc.get_account(&program_id).expect("program account");
    assert!(acct.executable, "deployed program is executable");

    // Lazy-fetch the Shim, Core Bridge, and the GuardianSet PDA so the
    // first-touch happens before the CPI logs land.
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

    // Post the 13 historical signatures into a fresh GuardianSignatures PDA.
    let sigs_slice =
        &VAA_BYTES[6..6 + NUM_SIGNATURES * GUARDIAN_SIGNATURE_LENGTH];
    let post_start = Instant::now();
    post_signatures(&rpc, &payer, &guardian_signatures_kp, sigs_slice);
    eprintln!("[e2e] PostSignatures elapsed: {:?}", post_start.elapsed());

    let gs_acct = rpc
        .get_account_with_commitment(&guardian_signatures_kp.pubkey(), CommitmentConfig::confirmed())
        .expect("GuardianSignatures lookup")
        .value
        .expect("GuardianSignatures account materialised");
    assert_eq!(gs_acct.owner, shim_program_id, "GS owned by Shim");
    eprintln!(
        "[e2e] GuardianSignatures lamports={} data_len={}",
        gs_acct.lamports,
        gs_acct.data.len()
    );

    // Seed the DigestAccount PDA. Production-shape `.so` has no public
    // `open_digest` entrypoint (it lives behind the `test-only-open-digest`
    // feature, which is forced off whenever `mock-vaa` is off — see the
    // paired-feature fence in `src/lib.rs`). The cheatcode writes the exact
    // same byte image the real `open_digest` would have produced.
    let (digest_pda, _bump) =
        derive_digest_pda(&program_id, EMITTER_CHAIN, &EMITTER_ADDRESS, SEQUENCE);
    write_digest_pda(&rpc_url, &program_id, &digest_pda, stored_digest, payer.pubkey());

    // Spot-check the stored layout we just injected.
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

/// Build the `close_digest` Instruction shared by both happy- and unhappy-path
/// tests. Caller chooses which digest to put in the instruction data — the
/// happy path uses the stored one, the negative path tampers with it.
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
            AccountMeta::new_readonly(payer, true),                     // closer
            AccountMeta::new(digest_pda, false),
            AccountMeta::new(payer, false),                             // rent recipient
            AccountMeta::new_readonly(guardian_signatures_pda, false),  // GS PDA
            AccountMeta::new_readonly(GUARDIAN_SET_PDA, false),         // GuardianSet PDA
            AccountMeta::new_readonly(shim_program_id, false),          // CPI target
        ],
        data: close_digest_ix_data(&digest_in_data, GUARDIAN_SET_BUMP),
    }
}

#[test]
#[ignore = "spawns surfpool subprocess + lazy-fetches mainnet; run via \
            `make test-e2e-mainnet-fork` or `cargo test -- --ignored`"]
fn close_digest_with_real_cpi_against_mainnet_fork_succeeds() {
    // Happy path. Open the PDA with the digest the VAA was signed over,
    // then close it. The Shim's CPI must succeed end-to-end against the
    // real on-chain guardian set.
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
        // CU limit first; the Shim's 13-sig VerifyHash blows past the 200k
        // default CU reservation. The CU-limit instruction itself costs 150 CU.
        &[set_compute_unit_limit_ix(CLOSE_DIGEST_CU_LIMIT), close_ix],
        Some(&fx.payer.pubkey()),
        &[&fx.payer],
        blockhash,
    );

    // Use `send_and_confirm` for the close — if the CPI rejects we want the
    // panic to surface the on-chain log lines directly.
    let close_sig = fx
        .rpc
        .send_and_confirm_transaction(&close_tx)
        .expect("close_digest with real CPI must succeed");
    eprintln!("[e2e] close_digest tx={close_sig}");

    // Pull the tx back via raw RPC so program logs and CU consumption land in
    // the test output. Going through `RpcClient::get_transaction` would pull
    // in `solana-transaction-status-client-types` as a dev-dep for the sake of
    // one struct shape; the JSON path is plenty and lets us keep the dep set
    // tight.
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

    // PDA must be gone (or zeroed).
    match fx.rpc.get_account_with_commitment(&fx.digest_pda, CommitmentConfig::confirmed()) {
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
    eprintln!(
        "[e2e] payer balance: after_open={payer_after_open} after_close={payer_after_close}",
    );
}

#[test]
#[ignore = "spawns surfpool subprocess + lazy-fetches mainnet; run via \
            `make test-e2e-mainnet-fork` or `cargo test -- --ignored`"]
fn close_digest_with_tampered_digest_fails_at_cpi() {
    // Negative path. Open the PDA with a tampered digest — single
    // last-byte flip is enough to break signature recovery — then call
    // `close_digest` with the same tampered digest in the instruction data.
    //
    // The digest-equality check inside `close_digest` will pass (tampered ==
    // tampered), so failure must come from the Shim CPI's signature recovery
    // step. That's the protection we care about: even a payload-equal opener
    // cannot close without a real quorum signing the digest it stored.
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
        // Same CU bump as the happy path so the rejection comes from the
        // Shim's signature-mismatch check, not a budget-exceeded error.
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

    // PDA must still exist — failed tx leaves state untouched.
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

    // Defensive: keep the var alive so the compiler does not warn about
    // unused fields in the fixture struct under future refactors.
    let _ = &fx.rpc_url;
}
