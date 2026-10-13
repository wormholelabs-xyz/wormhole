//! End-to-end test of the actual `svm-accountant-backfill run` CLI subcommand — not just
//! the library functions it's built from — through a real crash-mid-run,
//! resume, and parity-check cycle against a real surfpool validator.
//!
//! Spawns the real binary (`env!("CARGO_BIN_EXE_svm-accountant-backfill")`) as a
//! subprocess twice:
//!
//! 1. First invocation, against the full test snapshot: let it run for a
//!    bounded time, then `SIGKILL` it mid-flight — a real process crash.
//! 2. Second invocation, identical arguments (same snapshot, same progress
//!    file, same program): the resume. It must complete cleanly — the parity
//!    check runs by default and any finding is a hard failure (non-zero
//!    exit) — proving no batch was silently lost and nothing was
//!    catastrophically double-applied.
//! 3. Third invocation after an account of an unknown tag is written to the
//!    program: the parity check must report it and fail the run.
//!
//! ## Timing note
//!
//! Catching the first invocation genuinely mid-flight needs a real
//! wall-clock race. To make that race wide and reliable, this test starts
//! surfpool with a slowed slot time (see
//! `SurfpoolOptions::offline_with_slot_time_ms`) so a short, fixed sleep
//! reliably lands mid-run. The snapshot is ordered so the single
//! `BackfillBalance` batch (the only batch kind that is NOT safely
//! idempotent to resubmit — see `onchain_check.rs`'s doc) is first and is
//! virtually certain to have landed before the kill, so the batch(s)
//! actually caught in-flight are `BackfillNoReplay` ones, safe (if
//! wasteful) to resubmit.
//!
//! Note: `run`'s verify-before-skip check only re-checks the
//! prefix the progress file claims, not batches still in flight beyond it — a
//! `BackfillBalance`/NTT-map batch that lands on-chain but crashes before
//! its progress-file flush is durably written could hit a hard `InvalidPda` halt
//! on resume. This test's snapshot ordering sidesteps that case rather
//! than exercising it.
//!
//! `#[ignore]` by default — heavy and requires the backfill `.so` to be
//! built first. Run with:
//!   `just tools-e2e`

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

use global_accountant_definitions::NOREPLAY_PROGRAM_ID;
use svm_accountant_tools_common::accountant::Accountant;

mod common;
use svm_accountant_tools_common::test_support::surfpool::{
    deploy_program, noreplay_so_path, parent_so_path, set_account, start_surfpool, SurfpoolOptions,
};

const BACKFILL_PROGRAM_NAME: &str = "global_accountant_backfill";

/// 2 accounts → 1 `BackfillBalance` batch (first, per snapshot sort order),
/// then 10 distinct-emitter single-transfer records → 10 separate
/// `BackfillNoReplay` batches (the batcher closes a batch on every emitter
/// change). 11 batches total — enough to give a wide, real window between
/// "some work done" and "all work done" at `concurrency: 1`.
fn build_test_snapshot() -> String {
    let mut out = String::new();
    out.push_str(
        "{\"kind\":\"account\",\"chain\":1,\"token_chain\":2,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"balance\":\"0x0000000000000000000000000000000000000000000000000000000000001234\"}\n",
    );
    out.push_str(
        "{\"kind\":\"account\",\"chain\":1,\"token_chain\":2,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000bb\",\"balance\":\"0x0000000000000000000000000000000000000000000000000000000000005678\"}\n",
    );
    for i in 0u8..10 {
        out.push_str(&format!(
            "{{\"kind\":\"transfer\",\"chain\":2,\"emitter\":\"0x{:062x}{:02x}\",\"sequence\":1,\"digest\":\"0x{:062x}{:02x}\",\"amount\":\"0x0000000000000000000000000000000000000000000000000000000000000001\",\"token_chain\":1,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"recipient_chain\":2}}\n",
            0, i, 0, i
        ));
    }
    out
}

/// Pump a child's stdout/stderr to this test's own stderr on background
/// threads, exactly like the surfpool guard does for the validator
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

/// Read `batches_done` out of a progress.json without needing
/// to reconstruct the exact run identity the binary computed — this only
/// needs the raw counter.
fn progress_skip_count(progress_path: &std::path::Path) -> Option<u64> {
    let bytes = std::fs::read(progress_path).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v.get("batches_done")?.as_u64()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns surfpool + svm-accountant-backfill subprocesses; deploys real .so; run via `just tools-e2e`"]
async fn run_subcommand_survives_a_real_crash_mid_run_and_resumes_cleanly() {
    let bin_path = env!("CARGO_BIN_EXE_svm-accountant-backfill");

    // ---------- Boot surfpool (slowed down) + deploy ----------
    let backfill_so_path = parent_so_path(BACKFILL_PROGRAM_NAME);
    let backfill_bytes = std::fs::read(&backfill_so_path).unwrap_or_else(|e| {
        panic!(
            "missing {}: {e}. Run `just build-devnet` in the parent workspace first.",
            backfill_so_path.display()
        )
    });
    let noreplay_bytes = std::fs::read(noreplay_so_path()).expect("read noreplay fixture");

    let guard = start_surfpool(SurfpoolOptions::offline_with_slot_time_ms(
        "run-subcommand-e2e",
        300,
    ));
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
    eprintln!("[run-e2e] program_id={program_id}");

    // ---------- Fund payer (must equal BACKFILL_AUTHORITY) ----------
    let payer = Keypair::new_from_array([1u8; 32]);
    let async_rpc =
        AsyncRpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());
    async_rpc
        .request_airdrop(&payer.pubkey(), 10_000_000_000_000)
        .await
        .expect("airdrop");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // ---------- Write snapshot + payer keypair file ----------
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let snapshot_path = tmp.path().join("snapshot.jsonl");
    std::fs::write(&snapshot_path, build_test_snapshot()).expect("write snapshot");
    let progress_path = tmp.path().join("progress.json");
    let payer_path = tmp.path().join("payer.json");
    solana_keypair::write_keypair_file(&payer, &payer_path).expect("write payer keypair file");

    let config_path = tmp.path().join("svm-accountant.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"rpc_url = "{rpc_url}"

[backfill]
concurrency = 1
max_retries = 5
initial_retry_delay_ms = 100
max_retry_delay_ms = 1000

[wtt]
snapshot = "snapshot.jsonl"
progress_file = "progress.json"
payer = "payer.json"
"#
        ),
    )
    .expect("write config");
    let common_args = |no_parity_check: bool| -> Vec<String> {
        let mut args = vec![
            "--config".to_string(),
            config_path.to_string_lossy().into_owned(),
            "run".to_string(),
            "wtt".to_string(),
        ];
        if no_parity_check {
            args.push("--no-parity-check".to_string());
        }
        args
    };

    // ============================================================
    // PHASE 1: spawn the real binary, let it run partway, SIGKILL it.
    // ============================================================
    let child = Command::new(bin_path)
        .args(common_args(true)) // skip the parity check — we're about to kill it anyway
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn svm-accountant-backfill run (phase 1)");
    let mut child = pump(child, "run-phase1");

    // See the module doc's "Timing note".
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let _ = child.kill();
    let _ = child.wait();

    let after_kill = progress_skip_count(&progress_path);
    eprintln!("[run-e2e] progress file after SIGKILL: {after_kill:?} (11 total batches)");
    // Best-effort timing assertion — see the module doc's timing note.
    if let Some(n) = after_kill {
        assert!(
            n < 11,
            "expected the kill to interrupt the run before completion, but \
             the progress file already shows all 11 batches confirmed — the sleep \
             before SIGKILL needs to be shorter (or the slot time slower) \
             for this test's timing margin to hold"
        );
    }

    // ============================================================
    // PHASE 2: resume — identical args, no kill this time, parity check on.
    // ============================================================
    let status = Command::new(bin_path)
        .args(common_args(false))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map(|c| pump(c, "run-phase2"))
        .expect("spawn svm-accountant-backfill run (phase 2, resume)")
        .wait()
        .expect("wait on resumed run");

    assert!(
        status.success(),
        "resumed `run` must exit 0 — it runs the parity check by default and \
         bails non-zero on any finding, so success here already proves a clean \
         final state; got exit status {status:?}"
    );

    let after_resume =
        progress_skip_count(&progress_path).expect("progress file exists after resume");
    assert_eq!(
        after_resume, 11,
        "progress file must reflect all 11 batches confirmed after the resume completes"
    );

    // ---------- Independent, out-of-process confirmation ----------
    let target = common::target(Accountant::Wtt, program_id, payer.pubkey());
    let findings = common::parity_findings(&rpc_url, &target, &snapshot_path).await;
    assert!(
        findings.is_empty(),
        "independent parity check: {findings:#?}"
    );

    // ============================================================
    // PHASE 3: an account with an unknown tag under the program must fail
    // the run, though every planned batch is written.
    // ============================================================
    let stray = Pubkey::new_unique();
    set_account(&rpc_url, &stray, &program_id, 1_000_000_000, &[0xEE; 70]);
    let output = Command::new(bin_path)
        .args(common_args(false))
        .output()
        .expect("run svm-accountant-backfill (phase 3)");
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("[run-e2e] phase 3 stderr:\n{stderr}");
    assert!(
        !output.status.success(),
        "a malformed program account must fail the run"
    );
    assert!(
        stderr.contains("[malformed_accounts]") && stderr.contains(&stray.to_string()),
        "the parity check must name the stray account; stderr: {stderr}"
    );

    eprintln!("[run-e2e] crash-mid-run -> resume -> parity check cycle completed");
}
