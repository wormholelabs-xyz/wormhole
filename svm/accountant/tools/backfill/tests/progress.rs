//! Integration tests for the resumable progress file.

mod common;

use ga_backfill::accountant::Accountant;
use ga_backfill::batcher::TxBatch;
use ga_backfill::expected_accounts::derive_pda;
use ga_backfill::progress::ProgressFile;
use ga_backfill::snapshot::{BalanceRow, TransferRow};
use ga_backfill::submitter::{submit_batches, SubmitterConfig};
use ga_backfill::tx_builder::BackfillTarget;
use global_accountant_definitions::BalanceAccountKey;
use solana_client::nonblocking::rpc_client::RpcClient as AsyncRpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use std::time::Duration;
use tempfile::TempDir;

use common::surfpool::{
    deploy_program, noreplay_so_path, parent_so_path, start_surfpool, SurfpoolOptions,
};
use global_accountant_definitions::NOREPLAY_PROGRAM_ID;

const BACKFILL_PROGRAM_NAME: &str = "global_accountant_backfill";

const HASH: &str = "0x7ea3b17ceb23a426a4b46d469fc24a9c4fc0e5239b2d196058d623720bb1d363";
const PROGRAM: &str = "TKyKMUPncinqKyoqVyrfCfa8jNaWATxpVBvkQ67jXzS";

fn setup() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
    let progress_path = tmp.path().join("progress.json");
    let snapshot_path = tmp.path().join("catalogue.jsonl");
    std::fs::write(&snapshot_path, "stub").expect("write snapshot");
    (tmp, progress_path, snapshot_path)
}

#[test]
fn fresh_progress_starts_at_zero() {
    let (_tmp, progress_path, snapshot) = setup();
    let progress_file =
        ProgressFile::load_or_init(progress_path, &snapshot, HASH, PROGRAM).expect("load");
    assert_eq!(progress_file.batches_done(), 0);
    assert_eq!(progress_file.state().submitted, 0);
    assert_eq!(progress_file.state().already_written, 0);
    assert_eq!(progress_file.state().fees_lamports, 0);
}

#[test]
fn observed_events_advance_skip_count() {
    let (_tmp, progress_path, snapshot) = setup();
    let mut progress_file =
        ProgressFile::load_or_init(progress_path, &snapshot, HASH, PROGRAM).expect("load");
    progress_file
        .observe_confirmed(Some("sig1"), 5_000)
        .expect("observe");
    progress_file
        .observe_confirmed(Some("sig2"), 5_000)
        .expect("observe");
    progress_file.observe_already_written().expect("observe");
    assert_eq!(progress_file.batches_done(), 3);
    assert_eq!(progress_file.state().submitted, 2);
    assert_eq!(progress_file.state().already_written, 1);
    assert_eq!(progress_file.state().fees_lamports, 10_000);
    assert_eq!(progress_file.state().last_tx_sig.as_deref(), Some("sig2"));
}

#[test]
fn progress_persists_and_resumes() {
    let (_tmp, progress_path, snapshot) = setup();
    {
        let mut progress_file =
            ProgressFile::load_or_init(progress_path.clone(), &snapshot, HASH, PROGRAM)
                .expect("load");
        progress_file
            .observe_confirmed(Some("sig1"), 5_000)
            .expect("observe");
        progress_file.flush().expect("flush");
    }
    let progress_file =
        ProgressFile::load_or_init(progress_path, &snapshot, HASH, PROGRAM).expect("resume");
    assert_eq!(progress_file.batches_done(), 1);
    assert_eq!(progress_file.state().submitted, 1);
    assert_eq!(progress_file.state().fees_lamports, 5_000);
}

#[test]
fn progress_rejects_wrong_snapshot_hash() {
    let (_tmp, progress_path, snapshot) = setup();
    {
        let mut progress_file =
            ProgressFile::load_or_init(progress_path.clone(), &snapshot, HASH, PROGRAM)
                .expect("load");
        progress_file.flush().expect("flush");
    }
    let result = ProgressFile::load_or_init(progress_path, &snapshot, "0xDIFFERENT", PROGRAM);
    assert!(result.is_err(), "must reject snapshot-hash drift");
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("snapshot_sha256") || msg.contains("snapshot"),
        "error should mention snapshot mismatch, got: {msg}"
    );
}

#[test]
fn progress_rejects_wrong_program_id() {
    let (_tmp, progress_path, snapshot) = setup();
    {
        let mut progress_file =
            ProgressFile::load_or_init(progress_path.clone(), &snapshot, HASH, PROGRAM)
                .expect("load");
        progress_file.flush().expect("flush");
    }
    let result = ProgressFile::load_or_init(progress_path, &snapshot, HASH, "DIFFERENT_PROGRAM");
    assert!(result.is_err(), "must reject program-id drift");
}

#[test]
fn progress_persists_on_stride_threshold() {
    let (_tmp, progress_path, snapshot) = setup();
    let mut progress_file =
        ProgressFile::load_or_init_with_stride(progress_path.clone(), &snapshot, HASH, PROGRAM, 3)
            .expect("load");

    progress_file
        .observe_confirmed(Some("a"), 5_000)
        .expect("observe");
    progress_file
        .observe_confirmed(Some("b"), 5_000)
        .expect("observe");
    // After 2 events, stride (3) not yet hit; on-disk still reflects initial state
    let on_disk = std::fs::read_to_string(&progress_path).expect("read");
    assert!(
        on_disk.contains("\"submitted\": 0") || on_disk.contains("\"submitted\":0"),
        "stride not yet hit; on-disk submitted should be 0, was: {on_disk}"
    );

    progress_file
        .observe_confirmed(Some("c"), 5_000)
        .expect("observe");
    // Third event hits stride; on-disk now reflects the latest state
    let on_disk = std::fs::read_to_string(&progress_path).expect("read");
    assert!(
        on_disk.contains("\"submitted\": 3") || on_disk.contains("\"submitted\":3"),
        "stride hit; on-disk submitted should be 3, was: {on_disk}"
    );
}

#[test]
fn final_flush_persists_pending_events() {
    let (_tmp, progress_path, snapshot) = setup();
    let mut progress_file = ProgressFile::load_or_init_with_stride(
        progress_path.clone(),
        &snapshot,
        HASH,
        PROGRAM,
        1_000,
    )
    .expect("load");
    progress_file
        .observe_confirmed(Some("a"), 5_000)
        .expect("observe");
    progress_file
        .observe_confirmed(Some("b"), 5_000)
        .expect("observe");
    // No flush triggered yet (stride 1000). Final flush must persist.
    progress_file.flush().expect("final flush");
    let reloaded =
        ProgressFile::load_or_init(progress_path, &snapshot, HASH, PROGRAM).expect("reload");
    assert_eq!(reloaded.batches_done(), 2);
    assert_eq!(reloaded.state().submitted, 2);
}

#[test]
fn no_tmp_file_left_after_successful_write() {
    let (_tmp, progress_path, snapshot) = setup();
    let mut progress_file =
        ProgressFile::load_or_init(progress_path.clone(), &snapshot, HASH, PROGRAM).expect("load");
    progress_file
        .observe_confirmed(Some("a"), 5_000)
        .expect("observe");
    progress_file.flush().expect("flush");
    // Verify no stray .tmp file alongside progress.json
    let tmp_path = progress_path.with_extension("json.tmp");
    assert!(!tmp_path.exists(), "temp file should be renamed away");
    // The progress file itself exists
    assert!(progress_path.exists());
}

// Corrupted / partially-written progress.json (disk corruption, manual edit,
// interrupted copy, older non-atomic writer) must fail loudly rather than
// be silently treated as absent — a silent reinit would reset
// `batches_done` to 0 and resubmit the whole snapshot.

#[test]
fn truncated_mid_write_progress_json_fails_loudly_not_silently() {
    let (_tmp, progress_path, snapshot) = setup();
    // Build a real, valid progress file first...
    {
        let mut progress_file =
            ProgressFile::load_or_init(progress_path.clone(), &snapshot, HASH, PROGRAM)
                .expect("load");
        progress_file
            .observe_confirmed(Some("sig1"), 5_000)
            .expect("observe");
        progress_file.flush().expect("flush");
    }
    // Simulate a crash mid-write by truncating the file partway through.
    let full = std::fs::read(&progress_path).expect("read written progress_file");
    assert!(
        full.len() > 10,
        "sanity: progress_file file has real content"
    );
    let truncated = &full[..full.len() / 2];
    std::fs::write(&progress_path, truncated).expect("write truncated progress_file");

    let result = ProgressFile::load_or_init(progress_path, &snapshot, HASH, PROGRAM);
    assert!(
        result.is_err(),
        "a truncated progress_file.json must fail loudly, not silently reinitialise \
         (a silent reinit would blindly resubmit the whole snapshot on resume)"
    );
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("parse") || msg.contains("JSON") || msg.contains("json"),
        "error should identify the parse failure, got: {msg}"
    );
}

#[test]
fn invalid_json_progress_file_fails_loudly() {
    let (_tmp, progress_path, snapshot) = setup();
    std::fs::write(&progress_path, b"{ this is not valid json at all")
        .expect("write garbage progress_file");

    let result = ProgressFile::load_or_init(progress_path, &snapshot, HASH, PROGRAM);
    assert!(result.is_err(), "invalid JSON must fail loudly");
}

#[test]
fn empty_progress_file_fails_loudly() {
    // A zero-byte file (e.g. from an external tool or manual `touch`) must
    // not be treated as fresh state.
    let (_tmp, progress_path, snapshot) = setup();
    std::fs::write(&progress_path, b"").expect("write empty progress_file");

    let result = ProgressFile::load_or_init(progress_path, &snapshot, HASH, PROGRAM);
    assert!(
        result.is_err(),
        "empty progress_file.json must fail loudly, not be treated as absent"
    );
}

// Resume/idempotency against a real deployed program (surfpool). `#[ignore]`
// for the same reasons as the other surfpool e2e tests: heavy, spawns a
// subprocess, run via `just tools-e2e`.

fn noreplay_batch(sequence: u64) -> TxBatch {
    TxBatch::BackfillNoReplay(vec![TransferRow {
        chain: 1,
        emitter: {
            let mut e = [0u8; 32];
            e[31] = 0xec;
            e
        },
        sequence,
        digest: [0xaa; 32],
        amount: [0u8; 32],
        token_chain: 1,
        token_address: {
            let mut a = [0u8; 32];
            a[31] = 0xaa;
            a
        },
        recipient_chain: 2,
    }])
}

fn balance_batch(addr_seed: u8) -> TxBatch {
    TxBatch::BackfillBalance(vec![BalanceRow {
        chain: 1,
        token_chain: 2,
        token_address: {
            let mut a = [0u8; 32];
            a[31] = addr_seed;
            a
        },
        balance: {
            let mut b = [0u8; 32];
            b[31] = 0x42;
            b
        },
    }])
}

async fn boot(scratch_prefix: &'static str) -> (common::surfpool::SurfpoolGuard, Pubkey, Keypair) {
    let backfill_so_path = parent_so_path(BACKFILL_PROGRAM_NAME);
    let backfill_bytes = std::fs::read(&backfill_so_path).unwrap_or_else(|e| {
        panic!(
            "missing {}: {e}. Run `just build-devnet` in the parent workspace first.",
            backfill_so_path.display()
        )
    });
    let noreplay_bytes = std::fs::read(noreplay_so_path()).expect("read noreplay fixture");

    let guard = start_surfpool(SurfpoolOptions::offline(scratch_prefix));
    let rpc_url = guard.rpc_url();

    let program_id = Accountant::Wtt.program_id();
    deploy_program(&rpc_url, &program_id, &backfill_bytes);
    deploy_program(
        &rpc_url,
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        &noreplay_bytes,
    );

    let payer = Keypair::new_from_array([1u8; 32]);
    let async_rpc =
        AsyncRpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());
    async_rpc
        .request_airdrop(&payer.pubkey(), 10_000_000_000_000)
        .await
        .expect("airdrop");
    tokio::time::sleep(Duration::from_millis(500)).await;

    (guard, program_id, payer)
}

/// Proves against a real deployed program that resubmitting an
/// already-landed `BackfillNoReplay` batch fails with custom error 7
/// (`AlreadyAccounted`); the submitter counts it already accounted after
/// verifying the batch on chain.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "spawns surfpool subprocess; deploys real .so; run via `just tools-e2e`"]
async fn resubmitting_same_noreplay_batch_is_already_written() {
    let (guard, program_id, payer) = boot("progress_file-resubmit-noreplay").await;
    let target = BackfillTarget::new(Accountant::Wtt, program_id, payer.pubkey());
    let config = SubmitterConfig {
        rpc_url: guard.rpc_url(),
        concurrency: 2,
        max_retries: 3,
        initial_retry_delay_ms: 200,
        max_retry_delay_ms: 2_000,
    };

    let batch = noreplay_batch(5);

    let stats1 = submit_batches(
        target.clone(),
        payer.insecure_clone(),
        config.clone(),
        vec![batch.clone()],
    )
    .await
    .expect("first submission");
    assert_eq!(stats1.submitted, 1, "first submission lands normally");
    assert_eq!(stats1.already_written, 0);

    // Resubmit the identical batch — same (chain, emitter, sequence, digest).
    let stats2 = submit_batches(target.clone(), payer.insecure_clone(), config, vec![batch])
        .await
        .expect("second submission must not error");
    eprintln!("[progress_file-resubmit] second-submission stats: {stats2:?}");

    // `reject_if_marked` fails the replayed batch at its first entry.
    assert_eq!(
        stats2.already_written, 1,
        "replay classified as already accounted"
    );
    assert_eq!(stats2.submitted, 0, "replay lands no second transaction");

    drop(guard);
}

/// ProgressFile-ahead-of-chain: the progress file claims a batch confirmed that was
/// never actually submitted. Drives the raw `ProgressFile`/`submit_batches`
/// primitives directly (no on-chain verification) and shows the skipped
/// batch's on-chain state is never written — the reason `onchain_check.rs`
/// exists. `reconcile_balances` is the independent backstop that catches
/// the loss as `missing_from_chain`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "spawns surfpool subprocess; deploys real .so; run via `just tools-e2e`"]
async fn progress_ahead_of_chain_resume_silently_skips_unwritten_batch() {
    let (guard, program_id, payer) = boot("progress_file-ahead-of-chain").await;
    let target = BackfillTarget::new(Accountant::Wtt, program_id, payer.pubkey());
    let config = SubmitterConfig {
        rpc_url: guard.rpc_url(),
        concurrency: 2,
        max_retries: 3,
        initial_retry_delay_ms: 200,
        max_retry_delay_ms: 2_000,
    };

    // Two independent BackfillBalance batches — each a single writable PDA,
    // so we can directly probe whether batch A's PDA exists on chain.
    let batch_a = balance_batch(0xA0);
    let batch_b = balance_batch(0xB0);
    let batches = vec![batch_a, batch_b];

    let tmp = TempDir::new().expect("tempdir");
    let progress_path = tmp.path().join("progress.json");
    let snapshot_path = tmp.path().join("catalogue.jsonl");
    std::fs::write(&snapshot_path, "stub").expect("write snapshot");

    // ProgressFile claims batch A confirmed, but it was never actually submitted.
    {
        let mut progress_file = ProgressFile::load_or_init(
            progress_path.clone(),
            &snapshot_path,
            "0xprogress-ahead-test",
            &program_id.to_string(),
        )
        .expect("init progress_file");
        progress_file
            .observe_confirmed(Some("fake-sig-never-landed"), 5_000)
            .expect("observe");
        progress_file.flush().expect("flush");
    }

    // Resume: reload the progress file fresh and skip the batches it claims are done.
    let resumed_progress = ProgressFile::load_or_init(
        progress_path,
        &snapshot_path,
        "0xprogress-ahead-test",
        &program_id.to_string(),
    )
    .expect("reload progress_file");
    let skip = resumed_progress.batches_done() as usize;
    assert_eq!(skip, 1, "progress_file claims batch A already confirmed");

    let remaining: Vec<TxBatch> = batches.into_iter().skip(skip).collect();
    assert_eq!(remaining.len(), 1, "only batch B is resubmitted");

    let stats = submit_batches(target.clone(), payer.insecure_clone(), config, remaining)
        .await
        .expect("resumed submission");
    assert_eq!(stats.submitted, 1);

    // Prove batch A's PDA was never written: query it directly.
    let async_rpc =
        AsyncRpcClient::new_with_commitment(guard.rpc_url(), CommitmentConfig::confirmed());
    let pda_a = derive_pda(
        &program_id,
        &BalanceAccountKey::new(1, 2, {
            let mut a = [0u8; 32];
            a[31] = 0xA0;
            a
        }),
    );
    let account_a = async_rpc.get_account(&pda_a).await;
    eprintln!("[progress_file-ahead] batch A PDA lookup: {account_a:?}");
    assert!(
        account_a.is_err(),
        "GAP CONFIRMED: batch A's balance PDA does not exist on chain, yet the \
         progress_file-driven resume permanently skipped resubmitting it — the resume \
         path has no on-chain cross-check against the progress file's claims"
    );

    // Independent reconcile pass catches the loss as missing_from_chain.
    let mut expected = std::collections::HashMap::new();
    expected.insert(
        (1u16, 2u16, {
            let mut a = [0u8; 32];
            a[31] = 0xA0;
            a
        }),
        {
            let mut b = [0u8; 32];
            b[31] = 0x42;
            b
        },
    );
    expected.insert(
        (1u16, 2u16, {
            let mut a = [0u8; 32];
            a[31] = 0xB0;
            a
        }),
        {
            let mut b = [0u8; 32];
            b[31] = 0x42;
            b
        },
    );
    let actual = ga_backfill::reconcile::fetch_on_chain_balances(&async_rpc, &program_id)
        .await
        .expect("fetch on-chain balances");
    let report = ga_backfill::reconcile::compare_balances(&expected, &actual);
    eprintln!("[progress_file-ahead] reconcile report: {report:?}");
    assert!(
        !report.is_clean(),
        "reconcile must NOT be clean — batch A is genuinely missing on chain"
    );
    assert_eq!(report.missing_from_chain.len(), 1);
    assert_eq!(report.matched, 1, "batch B is present and correct");

    drop(guard);
}
