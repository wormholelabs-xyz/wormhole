//! End-to-end test of the actual `ga-backfill run` CLI subcommand — not just
//! the library functions it's built from — through a real crash-mid-run,
//! resume, and re-reconcile cycle against a real surfpool validator.
//!
//! Spawns the real binary (`env!("CARGO_BIN_EXE_ga-backfill")`) as a
//! subprocess twice:
//!
//! 1. First invocation, against the full test catalogue: let it run for a
//!    bounded time, then `SIGKILL` it mid-flight — a real process crash.
//! 2. Second invocation, identical arguments (same catalogue, same cursor
//!    file, same program): the resume. It must complete cleanly — reconcile
//!    runs by default and a non-clean reconciliation is a hard failure
//!    (non-zero exit) — proving no chunk was silently lost and nothing was
//!    catastrophically double-applied.
//!
//! ## Timing note
//!
//! Catching the first invocation genuinely mid-flight needs a real
//! wall-clock race. To make that race wide and reliable, this test starts
//! surfpool with a slowed slot time (see
//! `SurfpoolOptions::offline_with_slot_time_ms`) so a short, fixed sleep
//! reliably lands mid-run. The catalogue is ordered so the single
//! `BackfillBalance` chunk (the only chunk kind that is NOT safely
//! idempotent to resubmit — see `preflight.rs`'s doc) is first and is
//! virtually certain to have landed before the kill, so the chunk(s)
//! actually caught in-flight are `BackfillNoReplay` ones, safe (if
//! wasteful) to resubmit.
//!
//! Note: `run`'s verify-before-skip preflight only re-checks the
//! cursor-claimed prefix, not chunks still in flight beyond it — a
//! `BackfillBalance`/NTT-map chunk that lands on-chain but crashes before
//! its cursor flush is durably written could hit a hard `InvalidPda` halt
//! on resume. This test's catalogue ordering sidesteps that case rather
//! than exercising it.
//!
//! `#[ignore]` by default — heavy and requires the backfill `.so` to be
//! built first. Run with:
//!   `just build && cargo test --test e2e_run_subcommand_against_surfpool -- --ignored --nocapture`

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use serde_json::Value;
use solana_client::nonblocking::rpc_client::RpcClient as AsyncRpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use ga_backfill::reconcile::reconcile_balances;
use global_accountant_definitions::NOREPLAY_PROGRAM_ID;

mod common;
use common::surfpool::{deploy_program, noreplay_so_path, parent_so_path, start_surfpool, SurfpoolOptions};

const BACKFILL_PROGRAM_NAME: &str = "global_accountant_backfill";

/// 2 accounts → 1 `BackfillBalance` chunk (first, per catalogue sort order),
/// then 10 distinct-emitter single-transfer records → 10 separate
/// `BackfillNoReplay` chunks (the chunker closes a chunk on every emitter
/// change). 11 chunks total — enough to give a wide, real window between
/// "some work done" and "all work done" at `concurrency: 1`.
fn build_test_catalogue() -> String {
    let mut out = String::new();
    out.push_str(
        "{\"kind\":\"account\",\"chain\":1,\"token_chain\":2,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"balance\":\"0x0000000000000000000000000000000000000000000000000000000000001234\"}\n",
    );
    out.push_str(
        "{\"kind\":\"account\",\"chain\":1,\"token_chain\":2,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000bb\",\"balance\":\"0x0000000000000000000000000000000000000000000000000000000000005678\"}\n",
    );
    for i in 0u8..10 {
        out.push_str(&format!(
            "{{\"kind\":\"transfer\",\"chain\":1,\"emitter\":\"0x{:062x}{:02x}\",\"sequence\":1,\"digest\":\"0x{:062x}{:02x}\",\"amount\":\"0x0000000000000000000000000000000000000000000000000000000000000001\",\"token_chain\":1,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"recipient_chain\":2}}\n",
            0, i, 0, i
        ));
    }
    out
}

/// Pump a child's stdout/stderr to this test's own stderr on background
/// threads, exactly like `common::surfpool`'s guard does for the validator
/// — otherwise a full pipe buffer can deadlock the child.
fn pump(mut child: std::process::Child, tag: &'static str) -> std::process::Child {
    if let Some(out) = child.stdout.take() {
        thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                eprintln!("[{tag} stdout] {line}");
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                eprintln!("[{tag} stderr] {line}");
            }
        });
    }
    child
}

/// Read `last_confirmed_chunk_index` out of a cursor.json without needing
/// to reconstruct the exact catalogue-content-hash the binary computed
/// internally (private to `main.rs`) — this only needs the raw counter.
fn cursor_skip_count(cursor_path: &std::path::Path) -> Option<u64> {
    let bytes = std::fs::read(cursor_path).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v.get("last_confirmed_chunk_index")?.as_u64()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns surfpool + ga-backfill subprocesses; deploys real .so; requires `just build` first"]
async fn run_subcommand_survives_a_real_crash_mid_run_and_resumes_cleanly() {
    let bin_path = env!("CARGO_BIN_EXE_ga-backfill");

    // ---------- Boot surfpool (slowed down) + deploy ----------
    let backfill_so_path = parent_so_path(BACKFILL_PROGRAM_NAME);
    let backfill_bytes = std::fs::read(&backfill_so_path).unwrap_or_else(|e| {
        panic!(
            "missing {}: {e}. Run `just build` in the parent workspace first.",
            backfill_so_path.display()
        )
    });
    let noreplay_bytes = std::fs::read(noreplay_so_path()).expect("read noreplay fixture");

    let guard = start_surfpool(SurfpoolOptions::offline_with_slot_time_ms(
        "run-subcommand-e2e",
        300,
    ));
    let rpc_url = guard.rpc_url();

    let program_id = Keypair::new().pubkey();
    deploy_program(&rpc_url, &program_id, &backfill_bytes);
    deploy_program(&rpc_url, &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID), &noreplay_bytes);
    eprintln!("[run-e2e] program_id={program_id}");

    // ---------- Fund payer (must equal BACKFILL_AUTHORITY) ----------
    let payer = Keypair::new_from_array([1u8; 32]);
    let async_rpc = AsyncRpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());
    async_rpc
        .request_airdrop(&payer.pubkey(), 10_000_000_000_000)
        .await
        .expect("airdrop");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // ---------- Write catalogue + payer keypair file ----------
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let catalogue_path = tmp.path().join("catalogue.jsonl");
    std::fs::write(&catalogue_path, build_test_catalogue()).expect("write catalogue");
    let cursor_path = tmp.path().join("cursor.json");
    let payer_path = tmp.path().join("payer.json");
    solana_keypair::write_keypair_file(&payer, &payer_path).expect("write payer keypair file");

    let common_args = |no_reconcile: bool| -> Vec<String> {
        let mut args = vec![
            "run".to_string(),
            "--catalogue".to_string(),
            catalogue_path.to_string_lossy().into_owned(),
            "--program-id".to_string(),
            program_id.to_string(),
            "--payer".to_string(),
            payer_path.to_string_lossy().into_owned(),
            "--rpc-url".to_string(),
            rpc_url.clone(),
            "--cursor".to_string(),
            cursor_path.to_string_lossy().into_owned(),
            "--concurrency".to_string(),
            "1".to_string(),
            "--max-retries".to_string(),
            "5".to_string(),
            "--initial-retry-delay-ms".to_string(),
            "100".to_string(),
            "--max-retry-delay-ms".to_string(),
            "1000".to_string(),
        ];
        if no_reconcile {
            args.push("--no-reconcile".to_string());
        }
        args
    };

    // ============================================================
    // PHASE 1: spawn the real binary, let it run partway, SIGKILL it.
    // ============================================================
    let child = Command::new(bin_path)
        .args(common_args(true)) // skip reconcile — we're about to kill it anyway
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ga-backfill run (phase 1)");
    let mut child = pump(child, "run-phase1");

    // See the module doc's "Timing note".
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let _ = child.kill();
    let _ = child.wait();

    let after_kill = cursor_skip_count(&cursor_path);
    eprintln!("[run-e2e] cursor after SIGKILL: {after_kill:?} (11 total chunks)");
    // Best-effort timing assertion — see the module doc's timing note.
    if let Some(n) = after_kill {
        assert!(
            n < 11,
            "expected the kill to interrupt the run before completion, but \
             the cursor already shows all 11 chunks confirmed — the sleep \
             before SIGKILL needs to be shorter (or the slot time slower) \
             for this test's timing margin to hold"
        );
    }

    // ============================================================
    // PHASE 2: resume — identical args, no kill this time, reconcile on.
    // ============================================================
    let status = Command::new(bin_path)
        .args(common_args(false))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map(|c| pump(c, "run-phase2"))
        .expect("spawn ga-backfill run (phase 2, resume)")
        .wait()
        .expect("wait on resumed run");

    assert!(
        status.success(),
        "resumed `run` must exit 0 — it runs reconcile by default and bails \
         non-zero on any discrepancy, so success here already proves a clean \
         final state; got exit status {status:?}"
    );

    let after_resume = cursor_skip_count(&cursor_path).expect("cursor exists after resume");
    assert_eq!(
        after_resume, 11,
        "cursor must reflect all 11 chunks confirmed after the resume completes"
    );

    // ---------- Independent, out-of-process confirmation ----------
    let report = reconcile_balances(&async_rpc, &program_id, &catalogue_path)
        .await
        .expect("independent reconcile");
    eprintln!(
        "[run-e2e] independent reconcile: matched={} mismatched={} missing={} unexpected={}",
        report.matched,
        report.mismatched.len(),
        report.missing_from_chain.len(),
        report.unexpected_on_chain.len()
    );
    assert!(report.is_clean(), "independent reconcile found diffs: {report:#?}");
    assert_eq!(report.matched, 2, "both Balance entries present and correct");

    eprintln!("[run-e2e] crash-mid-run -> resume -> reconcile cycle completed cleanly");
}
