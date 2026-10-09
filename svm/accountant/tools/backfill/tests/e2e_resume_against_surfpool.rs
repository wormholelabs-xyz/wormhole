//! End-to-end crash-mid-run / resume simulation against a real surfpool
//! validator.
//!
//! Submits the first half of a planned batch list, simulates a crash (drops
//! everything and reloads the progress file from disk as a fresh process would),
//! submits only the remaining half using the reloaded progress file's
//! `batches_done()`, then reconciles the full snapshot and asserts a clean
//! match with the right total submitted count — proving no batch was
//! double-submitted and none was silently skipped.
//!
//! This test wires `ProgressFile` and `submit_batches` together itself (advancing
//! the progress file only for batches it knows, by construction, actually landed);
//! `e2e_run_subcommand_against_surfpool.rs` covers the same claim through
//! the actual `run` orchestrator.
//!
//! `#[ignore]` by default — heavy and requires the backfill `.so` to be
//! built first. Run with:
//!   `just tools-e2e` (or, from the tool directory, the test-env prefix in `justfile` then `cargo test --locked --test e2e_resume_against_surfpool -- --ignored`)

use std::path::PathBuf;
use std::time::Duration;

use solana_client::nonblocking::rpc_client::RpcClient as AsyncRpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use ga_backfill::batcher::{Batcher, TxBatch};
use ga_backfill::progress::ProgressFile;
use ga_backfill::reconcile::reconcile_balances;
use ga_backfill::snapshot::SnapshotReader;
use ga_backfill::submitter::{submit_batches, SubmitterConfig};
use ga_backfill::tx_builder::BackfillTarget;
use global_accountant_definitions::NOREPLAY_PROGRAM_ID;

mod common;
use common::surfpool::{
    deploy_program, noreplay_so_path, parent_so_path, start_surfpool, SurfpoolOptions,
};

const BACKFILL_PROGRAM_NAME: &str = "global_accountant_backfill";

/// 2 accounts (→ 1 BackfillBalance batch) + 5 distinct-emitter transfers (→ 5
/// separate BackfillNoReplay batches, one entry each, since the batcher closes
/// a batch on every emitter change) = 6 total batches. Enough to split
/// meaningfully into a "first half" / "second half" for the crash simulation.
const TEST_SNAPSHOT: &str = concat!(
    "{\"kind\":\"account\",\"chain\":1,\"token_chain\":2,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"balance\":\"0x0000000000000000000000000000000000000000000000000000000000001234\"}\n",
    "{\"kind\":\"account\",\"chain\":1,\"token_chain\":2,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000bb\",\"balance\":\"0x0000000000000000000000000000000000000000000000000000000000005678\"}\n",
    "{\"kind\":\"transfer\",\"chain\":1,\"emitter\":\"0x0000000000000000000000000000000000000000000000000000000000000001\",\"sequence\":1,\"digest\":\"0x0000000000000000000000000000000000000000000000000000000000000a01\",\"amount\":\"0x0000000000000000000000000000000000000000000000000000000000000011\",\"token_chain\":1,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"recipient_chain\":2}\n",
    "{\"kind\":\"transfer\",\"chain\":1,\"emitter\":\"0x0000000000000000000000000000000000000000000000000000000000000002\",\"sequence\":1,\"digest\":\"0x0000000000000000000000000000000000000000000000000000000000000a02\",\"amount\":\"0x0000000000000000000000000000000000000000000000000000000000000022\",\"token_chain\":1,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"recipient_chain\":2}\n",
    "{\"kind\":\"transfer\",\"chain\":1,\"emitter\":\"0x0000000000000000000000000000000000000000000000000000000000000003\",\"sequence\":1,\"digest\":\"0x0000000000000000000000000000000000000000000000000000000000000a03\",\"amount\":\"0x0000000000000000000000000000000000000000000000000000000000000033\",\"token_chain\":1,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"recipient_chain\":2}\n",
    "{\"kind\":\"transfer\",\"chain\":1,\"emitter\":\"0x0000000000000000000000000000000000000000000000000000000000000006\",\"sequence\":1,\"digest\":\"0x0000000000000000000000000000000000000000000000000000000000000a04\",\"amount\":\"0x0000000000000000000000000000000000000000000000000000000000000044\",\"token_chain\":1,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"recipient_chain\":2}\n",
    "{\"kind\":\"transfer\",\"chain\":1,\"emitter\":\"0x0000000000000000000000000000000000000000000000000000000000000005\",\"sequence\":1,\"digest\":\"0x0000000000000000000000000000000000000000000000000000000000000a05\",\"amount\":\"0x0000000000000000000000000000000000000000000000000000000000000055\",\"token_chain\":1,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"recipient_chain\":2}\n",
);

fn write_test_snapshot() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("test_snapshot.jsonl");
    std::fs::write(&path, TEST_SNAPSHOT).expect("write snapshot");
    (tmp, path)
}

fn plan_batches(snapshot_path: &std::path::Path) -> Vec<TxBatch> {
    Batcher::new(
        SnapshotReader::open(snapshot_path)
            .expect("open snapshot")
            .map(|r| r.expect("snapshot row parses")),
    )
    .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns surfpool subprocess; deploys real .so; run via `just tools-e2e`"]
async fn resume_after_simulated_crash_completes_with_no_double_submission_and_no_skipped_batch() {
    // ---------- Boot surfpool + deploy ----------
    let backfill_so_path = parent_so_path(BACKFILL_PROGRAM_NAME);
    let backfill_bytes = std::fs::read(&backfill_so_path).unwrap_or_else(|e| {
        panic!(
            "missing {}: {e}. Run `just build-devnet` in the parent workspace first.",
            backfill_so_path.display()
        )
    });
    let noreplay_bytes = std::fs::read(noreplay_so_path()).expect("read noreplay fixture");

    let guard = start_surfpool(SurfpoolOptions::offline("resume-e2e"));
    let rpc_url = guard.rpc_url();

    // Anchor's `declare_id!` pins this program to a single fixed address,
    // checked on every entry (`DeclaredProgramIdMismatch` otherwise) — unlike
    // the pre-migration pinocchio program, which derived every PDA from the
    // runtime `program_id` and so could deploy at a fresh random keypair per
    // run. `surfnet_writeProgram` writes program bytecode directly at an
    // arbitrary address without requiring a signing keypair for that
    // address, so deploying at the compiled-in ID works here even though no
    // private key for it is known.
    let program_id = Pubkey::new_from_array(global_accountant_backfill::ID.to_bytes());
    deploy_program(&rpc_url, &program_id, &backfill_bytes);
    deploy_program(
        &rpc_url,
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        &noreplay_bytes,
    );
    eprintln!("[resume-e2e] program_id={program_id}");

    // ---------- Fund payer (must equal BACKFILL_AUTHORITY) ----------
    let payer = Keypair::new_from_array([1u8; 32]);
    let async_rpc =
        AsyncRpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());
    async_rpc
        .request_airdrop(&payer.pubkey(), 10_000_000_000_000)
        .await
        .expect("airdrop");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // ---------- Plan the full batch list up front (mirrors what a real
    // orchestrator does once per run, before either the first or the
    // resumed submission pass) ----------
    let (_tmp, snapshot_path) = write_test_snapshot();
    let all_batches = plan_batches(&snapshot_path);
    eprintln!("[resume-e2e] planned {} batches total", all_batches.len());
    assert_eq!(
        all_batches.len(),
        6,
        "1 balance batch + 5 single-entry noreplay batches"
    );

    let progress_path = snapshot_path.with_file_name("progress.json");
    let snapshot_hash = "0xresume-e2e-test-content-hash";

    let target = BackfillTarget::new(
        ga_backfill::accountant::Accountant::Wtt,
        program_id,
        payer.pubkey(),
    );
    let config = SubmitterConfig {
        rpc_url: rpc_url.clone(),
        concurrency: 1, // serial: this test's own bookkeeping needs to know exactly which/how many batches landed each phase
        max_retries: 3,
        initial_retry_delay_ms: 200,
        max_retry_delay_ms: 2_000,
    };

    // ============================================================
    // PHASE 1: "first run" — submit only the first half, then crash.
    // ============================================================
    let first_half: Vec<TxBatch> = all_batches.iter().take(3).cloned().collect();
    {
        let mut progress_file = ProgressFile::load_or_init_with_stride(
            progress_path.clone(),
            &snapshot_path,
            snapshot_hash,
            &program_id.to_string(),
            1, // flush every event — deterministic "crash boundary"
        )
        .expect("init progress_file");
        assert_eq!(
            progress_file.batches_done(),
            0,
            "fresh progress_file must skip nothing"
        );

        let stats = submit_batches(
            target.clone(),
            payer.insecure_clone(),
            config.clone(),
            first_half.clone(),
        )
        .await
        .expect("first-half submission");
        eprintln!("[resume-e2e] phase 1 stats: {stats:?}");
        assert_eq!(stats.submitted, 3);
        assert_eq!(stats.already_written, 0);

        // Advance the progress file once per batch known (concurrency=1, no error)
        // to have landed.
        for _ in 0..stats.submitted {
            progress_file
                .observe_confirmed(None, 5_000)
                .expect("observe confirmed");
        }
        progress_file.flush().expect("flush before simulated crash");
        // `progress_file` is dropped here, simulating the crash.
    }

    // ============================================================
    // PHASE 2: "resume" — a fresh process reloads the progress file from disk and
    // must submit exactly the remaining batches: no fewer (a skipped batch),
    // no more (a double-submission of already-landed work).
    // ============================================================
    let resumed_progress = ProgressFile::load_or_init_with_stride(
        progress_path.clone(),
        &snapshot_path,
        snapshot_hash,
        &program_id.to_string(),
        1,
    )
    .expect("reload progress_file after simulated crash");
    let skip = resumed_progress.batches_done() as usize;
    assert_eq!(
        skip, 3,
        "progress_file remembers exactly the 3 confirmed batches"
    );

    let second_half: Vec<TxBatch> = all_batches.into_iter().skip(skip).collect();
    assert_eq!(
        second_half.len(),
        3,
        "exactly the remaining 3 batches, none skipped"
    );

    let mut progress_file = resumed_progress;
    let stats = submit_batches(target, payer.insecure_clone(), config, second_half)
        .await
        .expect("resumed submission");
    eprintln!("[resume-e2e] phase 2 (resumed) stats: {stats:?}");
    assert_eq!(
        stats.submitted, 3,
        "only the remaining 3 batches submitted — no double-submission"
    );
    assert_eq!(stats.already_written, 0);
    for _ in 0..stats.submitted {
        progress_file
            .observe_confirmed(None, 5_000)
            .expect("observe confirmed");
    }
    progress_file.flush().expect("final flush");
    assert_eq!(
        progress_file.batches_done(),
        6,
        "progress_file reflects all 6 batches across both phases"
    );
    assert_eq!(progress_file.state().submitted, 6);

    // ============================================================
    // Reconcile the FULL snapshot against on-chain state: a clean match
    // proves every batch from both phases landed exactly once — nothing
    // double-submitted, nothing silently skipped.
    // ============================================================
    let report = reconcile_balances(&async_rpc, &program_id, &snapshot_path)
        .await
        .expect("reconcile");
    eprintln!(
        "[resume-e2e] reconcile: matched={} mismatched={} missing={} unexpected={}",
        report.matched,
        report.mismatched.len(),
        report.missing_from_chain.len(),
        report.unexpected_on_chain.len()
    );
    assert!(
        report.is_clean(),
        "reconcile findings after resume: {report:#?}"
    );
    assert_eq!(
        report.matched, 2,
        "both Balance entries present and correct"
    );

    eprintln!(
        "[resume-e2e] resume-after-crash completed cleanly, no double-submission, no skipped batch"
    );
}
