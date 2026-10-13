//! End-to-end NTT backfill test against a real surfpool validator, driven by
//! a subset of the **real wormchain mainnet** NTT global-accountant state
//! (contract `wormhole1mc23…hmwm7`, height 18,669,029).
//!
//! Spins up surfpool, deploys the NTT backfill program + noreplay, then runs
//! the full orchestrator path against `ntt_mainnet_subset_snapshot.jsonl`:
//! snapshot read → batcher → tx_builder → submitter → progress file → parity check.
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
use std::sync::Arc;
use std::time::Duration;

use solana_client::nonblocking::rpc_client::RpcClient as AsyncRpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

use global_accountant_definitions::NOREPLAY_PROGRAM_ID;
use svm_accountant_backfill::progress::ProgressFile;
use svm_accountant_backfill::run::plan_batches;
use svm_accountant_backfill::submitter::{submit_batches, SubmitterConfig};
use svm_accountant_backfill::tx_builder::build_backfill_chain_registration_ix;
use svm_accountant_tools_common::accountant::Accountant;
use svm_accountant_tools_common::ids::{Address, ChainId, Sequence};
use svm_accountant_tools_common::parity::expected_state::from_snapshot;
use svm_accountant_tools_common::parity::state::{KeyedKind, StateKind};
use svm_accountant_tools_common::snapshot::read_rows;

mod common;
use svm_accountant_tools_common::test_support::surfpool::{
    deploy_program, noreplay_so_path, parent_so_path, start_surfpool, SurfpoolOptions,
};

const NTT_BACKFILL_PROGRAM_NAME: &str = "ntt_global_accountant_backfill";

/// Mainnet-drain row counts in the committed subset fixture. The parity
/// check must match exactly these many entries per class.
const EXPECTED_BALANCES: usize = 9;
const EXPECTED_RELAYER_REGS: usize = 16;
const EXPECTED_HUBS: usize = 9;
const EXPECTED_PEERS: usize = 24;

fn fixture_path() -> PathBuf {
    svm_accountant_tools_common::test_support::fixture_path("ntt_mainnet_subset_snapshot.jsonl")
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
async fn e2e_ntt_backfill_matches_mainnet_subset() {
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
    let progress_file = ProgressFile::open(
        progress_path.clone(),
        &snapshot_path,
        &common::test_identity(snapshot_hash, &program_id.to_string()),
        1,
    )
    .expect("init progress file");
    assert_eq!(
        progress_file.batches_done(),
        0,
        "fresh progress file must skip 0"
    );

    // ---------- Plan batches ----------
    let batches = plan_batches(
        read_rows(&snapshot_path).expect("read snapshot"),
        Accountant::Ntt,
    )
    .expect("plan");
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
    let target = common::target(Accountant::Ntt, program_id, payer.pubkey());
    let payer_clone = payer.insecure_clone();
    let config = SubmitterConfig {
        concurrency: 4,
        max_retries: 3,
        initial_retry_delay_ms: 200,
        max_retry_delay_ms: 2_000,
    };
    let batch_count = batches.len() as u64;
    let stats = submit_batches(
        Arc::new(common::async_rpc_for(&guard)),
        target.clone(),
        payer_clone,
        config,
        batches,
    )
    .await
    .expect("submit_batches");
    eprintln!("[ntt-e2e] submission stats: {stats:?}");
    // Every batch is a real backfill ix, nothing pre-accounted.
    assert_eq!(stats.confirmed, batch_count);
    assert_eq!(stats.already_written, 0);

    // ---------- Parity check: Balance, NoReplay and the three NTT-native maps ----------
    let rows = read_rows(&snapshot_path).expect("read snapshot");
    let (expected, _) = from_snapshot(&rows, Accountant::Ntt).expect("expected state");
    let counts = [
        (StateKind::BALANCES, EXPECTED_BALANCES),
        (
            StateKind::Keyed(KeyedKind::ChainRegistrations),
            EXPECTED_RELAYER_REGS,
        ),
        (
            StateKind::Keyed(KeyedKind::RegisterChainRecords),
            EXPECTED_RELAYER_REGS,
        ),
        (StateKind::Keyed(KeyedKind::TransceiverHubs), EXPECTED_HUBS),
        (
            StateKind::Keyed(KeyedKind::TransceiverPeers),
            EXPECTED_PEERS,
        ),
    ];
    for (kind, want) in counts {
        assert_eq!(expected.len_of(kind), want, "{kind} rows in the fixture");
    }
    let findings = common::parity_findings(&rpc_url, &target, &snapshot_path).await;
    assert!(findings.is_empty(), "parity findings: {findings:#?}");

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

    let reg = vec![
        svm_accountant_tools_common::snapshot::ChainRegistrationRow {
            chain: ChainId(9999), // fresh chain id, not previously written
            registered_emitter: Address([0x77; 32]),
            sequence: Sequence(1),
        },
    ];
    // Swap the payer meta after building, bypassing `BackfillTarget::try_new`'s
    // check so the on-chain authority check is what rejects the tx.
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
