//! End-to-end NTT backfill test against a real surfpool validator, driven by
//! a subset of the **real wormchain mainnet** NTT global-accountant state
//! (contract `wormhole1mc23…hmwm7`, height 18,669,029).
//!
//! Spins up surfpool, deploys the NTT backfill program + noreplay, then runs
//! the full orchestrator path against `fixtures/ntt_snapshot_mainnet_subset.jsonl`:
//! snapshot read → batcher → tx_builder → submitter → progress file → reconcile.
//! Asserts the reconstructed on-chain PDAs (Balance + the three NTT-native
//! maps) byte-match the snapshot, and that a stranger signer is rejected.
//!
//! The fixture is a faithful subset of the mainnet drain (all 9 accounts, 16
//! of 22 relayer registrations, 9 transceiver hubs, 24 peers, plus a
//! 30-transfer sample) so the test is reproducible without the multi-GB
//! archive. Registration sequences are the governance VAA sequences from
//! Wormholescan; the 6 chains without exactly one matching VAA are omitted. The
//! full 146k-transfer replay is a separate scale exercise run against the
//! complete snapshot, not this fixture.
//!
//! `#[ignore]` by default — heavy and requires the NTT backfill `.so` to be
//! built first. Run with:
//!   `just tools-e2e` (or, from the tool directory, the test-env prefix in `justfile` then `cargo test --locked --test e2e_ntt_against_surfpool -- --ignored`)

#![allow(clippy::too_many_arguments)]

use std::path::PathBuf;
use std::time::Duration;

use solana_client::nonblocking::rpc_client::RpcClient as AsyncRpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

use ga_backfill::accountant::Accountant;
use ga_backfill::batcher::Batcher;
use ga_backfill::expected_accounts::{
    chain_registration_account, register_chain_account, transceiver_hub_account,
    transceiver_peer_account,
};
use ga_backfill::progress::ProgressFile;
use ga_backfill::reconcile::{reconcile_balances, reconcile_layouts};
use ga_backfill::snapshot::SnapshotReader;
use ga_backfill::snapshot::SnapshotRow;
use ga_backfill::submitter::{submit_batches, SubmitterConfig};
use ga_backfill::tx_builder::{build_backfill_chain_registration_ix, BackfillTarget};
use global_accountant_definitions::NOREPLAY_PROGRAM_ID;

mod common;
use common::surfpool::{
    deploy_program, noreplay_so_path, parent_so_path, start_surfpool, SurfpoolOptions,
};

const NTT_BACKFILL_PROGRAM_NAME: &str = "ntt_global_accountant_backfill";

/// Mainnet-drain record counts in the committed subset fixture. Reconcile
/// must match exactly these many entries per class.
const EXPECTED_ACCOUNTS: usize = 9;
const EXPECTED_RELAYER_REGS: usize = 16;
const EXPECTED_HUBS: usize = 9;
const EXPECTED_PEERS: usize = 24;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ntt_snapshot_mainnet_subset.jsonl")
}

/// Copy the committed fixture into a scratch dir so the progress file the
/// orchestrator writes lands beside a throwaway copy, not in the repo.
fn stage_snapshot() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("ntt_snapshot.jsonl");
    std::fs::copy(fixture_path(), &path).expect("copy fixture");
    (tmp, path)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns surfpool subprocess; deploys real .so; run via `just tools-e2e`"]
async fn e2e_ntt_backfill_reconciles_mainnet_subset() {
    // ---------- Boot surfpool + deploy ----------
    let backfill_so_path = parent_so_path(NTT_BACKFILL_PROGRAM_NAME);
    let backfill_bytes = std::fs::read(&backfill_so_path).unwrap_or_else(|e| {
        panic!(
            "missing {}: {e}. Run `just build-devnet` in the parent workspace first.",
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

    // Anchor's `declare_id!` pins this program to a single fixed address,
    // checked on every entry (`DeclaredProgramIdMismatch` otherwise) — unlike
    // the pre-migration pinocchio program, which derived every PDA from the
    // runtime `program_id` and so could deploy at a fresh random keypair per
    // run. `surfnet_writeProgram` writes program bytecode directly at an
    // arbitrary address without requiring a signing keypair for that
    // address, so deploying at the compiled-in ID works here even though no
    // private key for it is known.
    let program_id = Pubkey::new_from_array(ntt_global_accountant_backfill::ID.to_bytes());
    deploy_program(&rpc_url, &program_id, &backfill_bytes);
    deploy_program(
        &rpc_url,
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        &noreplay_bytes,
    );
    eprintln!("[ntt-e2e] program_id={program_id}");

    // ---------- Fund payer (must equal `NTT_BACKFILL_AUTHORITY`) ----------
    let payer = Keypair::new_from_array([2u8; 32]);
    let async_rpc =
        AsyncRpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());
    async_rpc
        .request_airdrop(&payer.pubkey(), 10_000_000_000_000)
        .await
        .expect("airdrop");
    tokio::time::sleep(Duration::from_millis(500)).await;
    eprintln!("[ntt-e2e] payer={}", payer.pubkey());

    // ---------- Stage snapshot + progress file ----------
    let (_tmp, snapshot_path) = stage_snapshot();
    let snapshot_hash = "0xntt-e2e-mainnet-subset";
    let progress_path = snapshot_path.with_file_name("progress.json");
    let progress_file = ProgressFile::load_or_init(
        progress_path.clone(),
        &snapshot_path,
        snapshot_hash,
        &program_id.to_string(),
    )
    .expect("init progress_file");
    assert_eq!(
        progress_file.batches_done(),
        0,
        "fresh progress_file must skip 0"
    );

    // ---------- Plan batches ----------
    let batches: Vec<_> = Batcher::new(
        SnapshotReader::open(&snapshot_path)
            .expect("open snapshot")
            .filter_map(Result::ok),
    )
    .collect();
    eprintln!("[ntt-e2e] planned {} batches", batches.len());
    // Fixed-size classes: Balance ceil(9/8)=2, Registration ceil(16/7)=3,
    // Hub ceil(9/8)=2, Peer ceil(24/8)=3 = 10, plus >=1 emitter-grouped
    // BackfillNoReplay batch for the 30 transfers.
    assert!(
        batches.len() >= 11,
        "expected >=11 batches (10 fixed + >=1 transfer), got {}",
        batches.len()
    );

    // ---------- Run orchestrator submitter ----------
    let target = BackfillTarget::new(Accountant::Ntt, program_id, payer.pubkey());
    let payer_clone = payer.insecure_clone();
    let config = SubmitterConfig {
        rpc_url: rpc_url.clone(),
        concurrency: 4,
        max_retries: 3,
        initial_retry_delay_ms: 200,
        max_retry_delay_ms: 2_000,
    };
    let batch_count = batches.len() as u64;
    let stats = submit_batches(target.clone(), payer_clone, config, batches)
        .await
        .expect("submit_batches");
    eprintln!("[ntt-e2e] submission stats: {stats:?}");
    // Every batch is a real backfill ix, nothing pre-accounted.
    assert_eq!(stats.submitted, batch_count);
    assert_eq!(stats.already_written, 0);

    // ---------- Reconcile: Balance + the three NTT-native maps ----------
    let balance_report = reconcile_balances(&async_rpc, &program_id, &snapshot_path)
        .await
        .expect("reconcile balances");
    eprintln!(
        "[ntt-e2e] balances: matched={} findings={}",
        balance_report.matched,
        balance_report.total_diffs()
    );
    assert!(
        balance_report.is_clean(),
        "balance findings: {balance_report:#?}"
    );
    assert_eq!(balance_report.matched, EXPECTED_ACCOUNTS);

    let registration_verdict =
        reconcile_layouts(&async_rpc, &program_id, &snapshot_path, |r| match r {
            SnapshotRow::RelayerChainRegistration(x) => vec![chain_registration_account(x)],
            _ => vec![],
        })
        .await
        .expect("reconcile chain registrations");
    eprintln!("[ntt-e2e] chain registrations: {registration_verdict:?}");
    assert!(
        registration_verdict.is_clean(),
        "registration findings: {registration_verdict:?}"
    );
    assert_eq!(registration_verdict.matched, EXPECTED_RELAYER_REGS);

    let record_verdict = reconcile_layouts(&async_rpc, &program_id, &snapshot_path, |r| match r {
        SnapshotRow::RelayerChainRegistration(x) => vec![register_chain_account(x)],
        _ => vec![],
    })
    .await
    .expect("reconcile register_chain records");
    eprintln!("[ntt-e2e] register_chain records: {record_verdict:?}");
    assert!(
        record_verdict.is_clean(),
        "record findings: {record_verdict:?}"
    );
    assert_eq!(record_verdict.matched, EXPECTED_RELAYER_REGS);

    let hub_verdict = reconcile_layouts(&async_rpc, &program_id, &snapshot_path, |r| match r {
        SnapshotRow::TransceiverHub(h) => vec![transceiver_hub_account(h)],
        _ => vec![],
    })
    .await
    .expect("reconcile hubs");
    eprintln!("[ntt-e2e] hubs: {hub_verdict:?}");
    assert!(hub_verdict.is_clean(), "hub findings: {hub_verdict:?}");
    assert_eq!(hub_verdict.matched, EXPECTED_HUBS);

    let peer_verdict = reconcile_layouts(&async_rpc, &program_id, &snapshot_path, |r| match r {
        SnapshotRow::TransceiverPeer(p) => vec![transceiver_peer_account(p)],
        _ => vec![],
    })
    .await
    .expect("reconcile peers");
    eprintln!("[ntt-e2e] peers: {peer_verdict:?}");
    assert!(peer_verdict.is_clean(), "peer findings: {peer_verdict:?}");
    assert_eq!(peer_verdict.matched, EXPECTED_PEERS);

    // ---------- Wrong-signer control: stranger must be rejected ----------
    //
    // Builds a `BackfillRelayerChainRegistration` ix the authority would send,
    // but signs with a fresh keypair. `require_authority` must reject with
    // `UnauthorizedCaller` (Custom(33)).
    let stranger = Keypair::new();
    async_rpc
        .request_airdrop(&stranger.pubkey(), 1_000_000_000)
        .await
        .expect("airdrop stranger");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let reg = vec![ga_backfill::snapshot::ChainRegistrationRow {
        chain: 9999, // fresh chain id, not previously written
        registered_emitter: [0x77; 32],
        sequence: 1,
    }];
    // Swap the payer meta after building, bypassing `BackfillTarget::new`'s
    // assertion so the on-chain authority check is what rejects the tx.
    let mut stranger_ix = build_backfill_chain_registration_ix(&target, &reg);
    stranger_ix.accounts[0].pubkey = stranger.pubkey();
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
        err_msg.contains("0x21"),
        "expected UnauthorizedCaller (custom error 33); got: {err_msg}"
    );

    eprintln!("[ntt-e2e] all phases green");
}
