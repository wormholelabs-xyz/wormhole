//! End-to-end NTT backfill test against a real surfpool validator, driven by
//! a subset of the **real wormchain mainnet** NTT global-accountant state
//! (contract `wormhole1mc23…hmwm7`, height 18,669,029).
//!
//! Spins up surfpool, deploys the NTT backfill program + noreplay, then runs
//! the full orchestrator path against `fixtures/ntt_catalogue_mainnet_subset.jsonl`:
//! catalogue read → chunker → tx_builder → submitter → cursor → reconcile.
//! Asserts the reconstructed on-chain PDAs (Balance + the three NTT-native
//! maps) byte-match the catalogue, and that a stranger signer is rejected.
//!
//! The fixture is a faithful subset of the mainnet drain (all 9 accounts, 22
//! relayer registrations, 9 transceiver hubs, 24 peers, plus a 30-transfer
//! sample) so the test is reproducible without the multi-GB archive. The
//! full 146k-transfer replay is a separate scale exercise run against the
//! complete catalogue, not this fixture.
//!
//! `#[ignore]` by default — heavy and requires the NTT backfill `.so` to be
//! built first. Run with:
//!   `just build && cargo test --test e2e_ntt_against_surfpool -- --ignored --nocapture`

#![allow(clippy::too_many_arguments)]

use std::path::PathBuf;
use std::time::Duration;

use solana_client::nonblocking::rpc_client::RpcClient as AsyncRpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

use ga_backfill::catalogue::CatalogueReader;
use ga_backfill::chunker::Chunker;
use ga_backfill::cursor::Cursor;
use ga_backfill::reconcile::{
    compare_maps, fetch_on_chain_relayer_registrations, fetch_on_chain_transceiver_hubs,
    fetch_on_chain_transceiver_peers, load_expected_relayer_registrations,
    load_expected_transceiver_hubs, load_expected_transceiver_peers, reconcile_balances,
};
use ga_backfill::submitter::{submit_chunks, SubmitterConfig};
use ga_backfill::tx_builder::{build_backfill_relayer_registration_ix, BackfillCtx};
use global_accountant_definitions::NOREPLAY_PROGRAM_ID;

mod common;
use common::surfpool::{
    deploy_program, noreplay_so_path, parent_so_path, start_surfpool, SurfpoolOptions,
};

const NTT_BACKFILL_PROGRAM_NAME: &str = "ntt_global_accountant_backfill";

/// Mainnet-drain record counts in the committed subset fixture. Reconcile
/// must match exactly these many entries per class.
const EXPECTED_ACCOUNTS: usize = 9;
const EXPECTED_RELAYER_REGS: usize = 22;
const EXPECTED_HUBS: usize = 9;
const EXPECTED_PEERS: usize = 24;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ntt_catalogue_mainnet_subset.jsonl")
}

/// Copy the committed fixture into a scratch dir so the cursor file the
/// orchestrator writes lands beside a throwaway copy, not in the repo.
fn stage_catalogue() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("ntt_catalogue.jsonl");
    std::fs::copy(fixture_path(), &path).expect("copy fixture");
    (tmp, path)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns surfpool subprocess; deploys real .so; requires `just build` first"]
async fn e2e_ntt_backfill_reconciles_mainnet_subset() {
    // ---------- Boot surfpool + deploy ----------
    let backfill_so_path = parent_so_path(NTT_BACKFILL_PROGRAM_NAME);
    let backfill_bytes = std::fs::read(&backfill_so_path).unwrap_or_else(|e| {
        panic!(
            "missing {}: {e}. Run `just build` in the parent workspace first.",
            backfill_so_path.display()
        )
    });
    let noreplay_bytes = std::fs::read(noreplay_so_path()).expect("read noreplay fixture");
    eprintln!(
        "[ntt-e2e] backfill_so={} bytes, noreplay_so={} bytes",
        backfill_bytes.len(),
        noreplay_bytes.len()
    );

    let guard = start_surfpool(SurfpoolOptions::offline("ntt-orchestrator-e2e"));
    let rpc_url = guard.rpc_url();

    let program_id = Keypair::new().pubkey();
    deploy_program(&rpc_url, &program_id, &backfill_bytes);
    deploy_program(
        &rpc_url,
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        &noreplay_bytes,
    );
    eprintln!("[ntt-e2e] program_id={program_id}");

    // ---------- Fund payer (must equal `BACKFILL_AUTHORITY`) ----------
    let payer = Keypair::new_from_array([1u8; 32]);
    let async_rpc =
        AsyncRpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());
    async_rpc
        .request_airdrop(&payer.pubkey(), 10_000_000_000_000)
        .await
        .expect("airdrop");
    tokio::time::sleep(Duration::from_millis(500)).await;
    eprintln!("[ntt-e2e] payer={}", payer.pubkey());

    // ---------- Stage catalogue + cursor ----------
    let (_tmp, catalogue_path) = stage_catalogue();
    let catalogue_hash = "0xntt-e2e-mainnet-subset";
    let cursor_path = catalogue_path.with_file_name("cursor.json");
    let cursor = Cursor::load_or_init(
        cursor_path.clone(),
        &catalogue_path,
        catalogue_hash,
        &program_id.to_string(),
    )
    .expect("init cursor");
    assert_eq!(cursor.skip_count(), 0, "fresh cursor must skip 0");

    // ---------- Plan chunks ----------
    let chunks: Vec<_> = Chunker::new(
        CatalogueReader::open(&catalogue_path)
            .expect("open catalogue")
            .filter_map(Result::ok),
    )
    .collect();
    eprintln!("[ntt-e2e] planned {} chunks", chunks.len());
    // Fixed-size classes: Balance ceil(9/8)=2, Relayer ceil(22/12)=2,
    // Hub ceil(9/8)=2, Peer ceil(24/8)=3 = 9, plus >=1 emitter-grouped
    // BackfillNoReplay chunk for the 30 transfers. The NTT catalogue carries
    // no modification/registration records, so nothing is deferred.
    assert!(
        chunks.len() >= 10,
        "expected >=10 chunks (9 fixed + >=1 transfer), got {}",
        chunks.len()
    );

    // ---------- Run orchestrator submitter ----------
    let ctx = BackfillCtx::new(program_id, payer.pubkey());
    let payer_clone = payer.insecure_clone();
    let config = SubmitterConfig {
        rpc_url: rpc_url.clone(),
        concurrency: 4,
        max_retries: 3,
        initial_retry_delay_ms: 200,
        max_retry_delay_ms: 2_000,
    };
    let chunk_count = chunks.len() as u64;
    let stats = submit_chunks(ctx.clone(), payer_clone, config, chunks)
        .await
        .expect("submit_chunks");
    eprintln!("[ntt-e2e] submission stats: {stats:?}");
    // Every chunk is a real backfill ix — nothing deferred, nothing pre-accounted.
    assert_eq!(stats.submitted, chunk_count);
    assert_eq!(stats.deferred_skipped, 0);
    assert_eq!(stats.already_accounted, 0);

    // ---------- Reconcile: Balance + the three NTT-native maps ----------
    let balance_report = reconcile_balances(&async_rpc, &program_id, &catalogue_path)
        .await
        .expect("reconcile balances");
    eprintln!(
        "[ntt-e2e] balances: matched={} diffs={}",
        balance_report.matched,
        balance_report.total_diffs()
    );
    assert!(balance_report.is_clean(), "balance diffs: {balance_report:#?}");
    assert_eq!(balance_report.matched, EXPECTED_ACCOUNTS);

    let relayer_expected =
        load_expected_relayer_registrations(&catalogue_path).expect("expected relayer regs");
    let relayer_actual = fetch_on_chain_relayer_registrations(&async_rpc, &program_id)
        .await
        .expect("fetch relayer regs");
    let relayer_verdict = compare_maps(&relayer_expected, &relayer_actual);
    eprintln!("[ntt-e2e] relayer regs: {relayer_verdict:?}");
    assert!(relayer_verdict.is_clean(), "relayer diffs: {relayer_verdict:?}");
    assert_eq!(relayer_verdict.matched, EXPECTED_RELAYER_REGS);

    let hub_expected = load_expected_transceiver_hubs(&catalogue_path).expect("expected hubs");
    let hub_actual = fetch_on_chain_transceiver_hubs(&async_rpc, &program_id)
        .await
        .expect("fetch hubs");
    let hub_verdict = compare_maps(&hub_expected, &hub_actual);
    eprintln!("[ntt-e2e] hubs: {hub_verdict:?}");
    assert!(hub_verdict.is_clean(), "hub diffs: {hub_verdict:?}");
    assert_eq!(hub_verdict.matched, EXPECTED_HUBS);

    let peer_expected = load_expected_transceiver_peers(&catalogue_path).expect("expected peers");
    let peer_actual = fetch_on_chain_transceiver_peers(&async_rpc, &program_id)
        .await
        .expect("fetch peers");
    let peer_verdict = compare_maps(&peer_expected, &peer_actual);
    eprintln!("[ntt-e2e] peers: {peer_verdict:?}");
    assert!(peer_verdict.is_clean(), "peer diffs: {peer_verdict:?}");
    assert_eq!(peer_verdict.matched, EXPECTED_PEERS);

    // ---------- Wrong-signer control: stranger must be rejected ----------
    //
    // Builds a `BackfillRelayerRegistration` ix the authority would send, but
    // signs with a fresh keypair. `require_authority` must reject with
    // `UnauthorizedCaller` (Custom(3)).
    let stranger = Keypair::new();
    async_rpc
        .request_airdrop(&stranger.pubkey(), 1_000_000_000)
        .await
        .expect("airdrop stranger");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let reg = vec![ga_backfill::catalogue::RelayerChainRegistrationRecord {
        chain: 9999, // fresh chain id, not previously written
        registered_emitter: [0x77; 32],
    }];
    // Bypass `BackfillCtx::new`'s assertion — we deliberately want a ctx whose
    // payer != BACKFILL_AUTHORITY so we hit the on-chain check.
    let stranger_ctx = BackfillCtx {
        program_id,
        payer: stranger.pubkey(),
        system_program: ctx.system_program,
        noreplay_program: ctx.noreplay_program,
    };
    let stranger_ix = build_backfill_relayer_registration_ix(&stranger_ctx, &reg);
    let blockhash = async_rpc.get_latest_blockhash().await.expect("blockhash");
    let stranger_tx = Transaction::new_signed_with_payer(
        std::slice::from_ref(&stranger_ix),
        Some(&stranger.pubkey()),
        &[&stranger],
        blockhash,
    );
    let result = async_rpc.send_and_confirm_transaction(&stranger_tx).await;
    eprintln!("[ntt-e2e] wrong-signer result: {result:?}");
    assert!(result.is_err(), "wrong-signer submission must fail");
    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(
        err_msg.contains("0x3"),
        "expected UnauthorizedCaller (custom error 3); got: {err_msg}"
    );

    eprintln!("[ntt-e2e] all phases green");
}
