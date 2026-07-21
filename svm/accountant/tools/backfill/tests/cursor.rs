//! Integration tests for the resumable cursor.

mod common;

use ga_backfill::catalogue::{AccountRecord, TransferRecord};
use ga_backfill::chunker::ChunkPlan;
use ga_backfill::cursor::Cursor;
use ga_backfill::submitter::{submit_chunks, SubmitterConfig};
use ga_backfill::tx_builder::BackfillCtx;
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
    let cursor_path = tmp.path().join("cursor.json");
    let catalogue_path = tmp.path().join("catalogue.jsonl");
    std::fs::write(&catalogue_path, "stub").expect("write catalogue");
    (tmp, cursor_path, catalogue_path)
}

#[test]
fn fresh_cursor_starts_at_zero() {
    let (_tmp, cursor_path, catalogue) = setup();
    let cursor = Cursor::load_or_init(cursor_path, &catalogue, HASH, PROGRAM).expect("load");
    assert_eq!(cursor.skip_count(), 0);
    assert_eq!(cursor.state().submitted, 0);
    assert_eq!(cursor.state().already_accounted, 0);
    assert_eq!(cursor.state().fees_lamports, 0);
}

#[test]
fn observed_events_advance_skip_count() {
    let (_tmp, cursor_path, catalogue) = setup();
    let mut cursor = Cursor::load_or_init(cursor_path, &catalogue, HASH, PROGRAM).expect("load");
    cursor
        .observe_confirmed(Some("sig1"), 5_000)
        .expect("observe");
    cursor
        .observe_confirmed(Some("sig2"), 5_000)
        .expect("observe");
    cursor.observe_already_accounted().expect("observe");
    assert_eq!(cursor.skip_count(), 3);
    assert_eq!(cursor.state().submitted, 2);
    assert_eq!(cursor.state().already_accounted, 1);
    assert_eq!(cursor.state().fees_lamports, 10_000);
    assert_eq!(cursor.state().last_tx_sig.as_deref(), Some("sig2"));
}

#[test]
fn cursor_persists_and_resumes() {
    let (_tmp, cursor_path, catalogue) = setup();
    {
        let mut cursor =
            Cursor::load_or_init(cursor_path.clone(), &catalogue, HASH, PROGRAM).expect("load");
        cursor
            .observe_confirmed(Some("sig1"), 5_000)
            .expect("observe");
        cursor.flush().expect("flush");
    }
    let cursor = Cursor::load_or_init(cursor_path, &catalogue, HASH, PROGRAM).expect("resume");
    assert_eq!(cursor.skip_count(), 1);
    assert_eq!(cursor.state().submitted, 1);
    assert_eq!(cursor.state().fees_lamports, 5_000);
}

#[test]
fn cursor_rejects_wrong_catalogue_hash() {
    let (_tmp, cursor_path, catalogue) = setup();
    {
        let mut cursor =
            Cursor::load_or_init(cursor_path.clone(), &catalogue, HASH, PROGRAM).expect("load");
        cursor.flush().expect("flush");
    }
    let result = Cursor::load_or_init(cursor_path, &catalogue, "0xDIFFERENT", PROGRAM);
    assert!(result.is_err(), "must reject catalogue-hash drift");
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("catalogue_content_hash") || msg.contains("catalogue"),
        "error should mention catalogue mismatch, got: {msg}"
    );
}

#[test]
fn cursor_rejects_wrong_program_id() {
    let (_tmp, cursor_path, catalogue) = setup();
    {
        let mut cursor =
            Cursor::load_or_init(cursor_path.clone(), &catalogue, HASH, PROGRAM).expect("load");
        cursor.flush().expect("flush");
    }
    let result = Cursor::load_or_init(cursor_path, &catalogue, HASH, "DIFFERENT_PROGRAM");
    assert!(result.is_err(), "must reject program-id drift");
}

#[test]
fn cursor_persists_on_stride_threshold() {
    let (_tmp, cursor_path, catalogue) = setup();
    let mut cursor =
        Cursor::load_or_init_with_stride(cursor_path.clone(), &catalogue, HASH, PROGRAM, 3)
            .expect("load");

    cursor.observe_confirmed(Some("a"), 5_000).expect("observe");
    cursor.observe_confirmed(Some("b"), 5_000).expect("observe");
    // After 2 events, stride (3) not yet hit; on-disk still reflects initial state
    let on_disk = std::fs::read_to_string(&cursor_path).expect("read");
    assert!(
        on_disk.contains("\"submitted\": 0") || on_disk.contains("\"submitted\":0"),
        "stride not yet hit; on-disk submitted should be 0, was: {on_disk}"
    );

    cursor.observe_confirmed(Some("c"), 5_000).expect("observe");
    // Third event hits stride; on-disk now reflects the latest state
    let on_disk = std::fs::read_to_string(&cursor_path).expect("read");
    assert!(
        on_disk.contains("\"submitted\": 3") || on_disk.contains("\"submitted\":3"),
        "stride hit; on-disk submitted should be 3, was: {on_disk}"
    );
}

#[test]
fn final_flush_persists_pending_events() {
    let (_tmp, cursor_path, catalogue) = setup();
    let mut cursor =
        Cursor::load_or_init_with_stride(cursor_path.clone(), &catalogue, HASH, PROGRAM, 1_000)
            .expect("load");
    cursor.observe_confirmed(Some("a"), 5_000).expect("observe");
    cursor.observe_confirmed(Some("b"), 5_000).expect("observe");
    // No flush triggered yet (stride 1000). Final flush must persist.
    cursor.flush().expect("final flush");
    let reloaded = Cursor::load_or_init(cursor_path, &catalogue, HASH, PROGRAM).expect("reload");
    assert_eq!(reloaded.skip_count(), 2);
    assert_eq!(reloaded.state().submitted, 2);
}

#[test]
fn no_tmp_file_left_after_successful_write() {
    let (_tmp, cursor_path, catalogue) = setup();
    let mut cursor =
        Cursor::load_or_init(cursor_path.clone(), &catalogue, HASH, PROGRAM).expect("load");
    cursor.observe_confirmed(Some("a"), 5_000).expect("observe");
    cursor.flush().expect("flush");
    // Verify no stray .tmp file alongside cursor.json
    let tmp_path = cursor_path.with_extension("json.tmp");
    assert!(!tmp_path.exists(), "temp file should be renamed away");
    // The cursor file itself exists
    assert!(cursor_path.exists());
}

// Corrupted / partially-written cursor.json (disk corruption, manual edit,
// interrupted copy, older non-atomic writer) must fail loudly rather than
// be silently treated as absent — a silent reinit would reset
// `last_confirmed_chunk_index` to 0 and resubmit the whole catalogue.

#[test]
fn truncated_mid_write_cursor_json_fails_loudly_not_silently() {
    let (_tmp, cursor_path, catalogue) = setup();
    // Build a real, valid cursor file first...
    {
        let mut cursor =
            Cursor::load_or_init(cursor_path.clone(), &catalogue, HASH, PROGRAM).expect("load");
        cursor
            .observe_confirmed(Some("sig1"), 5_000)
            .expect("observe");
        cursor.flush().expect("flush");
    }
    // Simulate a crash mid-write by truncating the file partway through.
    let full = std::fs::read(&cursor_path).expect("read written cursor");
    assert!(full.len() > 10, "sanity: cursor file has real content");
    let truncated = &full[..full.len() / 2];
    std::fs::write(&cursor_path, truncated).expect("write truncated cursor");

    let result = Cursor::load_or_init(cursor_path, &catalogue, HASH, PROGRAM);
    assert!(
        result.is_err(),
        "a truncated cursor.json must fail loudly, not silently reinitialise \
         (a silent reinit would blindly resubmit the whole catalogue on resume)"
    );
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("parse") || msg.contains("JSON") || msg.contains("json"),
        "error should identify the parse failure, got: {msg}"
    );
}

#[test]
fn invalid_json_cursor_file_fails_loudly() {
    let (_tmp, cursor_path, catalogue) = setup();
    std::fs::write(&cursor_path, b"{ this is not valid json at all")
        .expect("write garbage cursor");

    let result = Cursor::load_or_init(cursor_path, &catalogue, HASH, PROGRAM);
    assert!(result.is_err(), "invalid JSON must fail loudly");
}

#[test]
fn empty_cursor_file_fails_loudly() {
    // A zero-byte file (e.g. from an external tool or manual `touch`) must
    // not be treated as fresh state.
    let (_tmp, cursor_path, catalogue) = setup();
    std::fs::write(&cursor_path, b"").expect("write empty cursor");

    let result = Cursor::load_or_init(cursor_path, &catalogue, HASH, PROGRAM);
    assert!(result.is_err(), "empty cursor.json must fail loudly, not be treated as absent");
}

// Resume/idempotency against a real deployed program (surfpool). `#[ignore]`
// for the same reasons as the other surfpool e2e tests: heavy, spawns a
// subprocess, requires `just build` first.

fn noreplay_chunk(sequence: u64) -> ChunkPlan {
    ChunkPlan::BackfillNoReplay(vec![TransferRecord {
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

fn balance_chunk(addr_seed: u8) -> ChunkPlan {
    ChunkPlan::BackfillBalance(vec![AccountRecord {
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
            "missing {}: {e}. Run `just build` in the parent workspace first.",
            backfill_so_path.display()
        )
    });
    let noreplay_bytes = std::fs::read(noreplay_so_path()).expect("read noreplay fixture");

    let guard = start_surfpool(SurfpoolOptions::offline(scratch_prefix));
    let rpc_url = guard.rpc_url();

    let program_id = Keypair::new().pubkey();
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
/// already-landed `BackfillNoReplay` chunk does not fail with custom error
/// 7 (`AlreadyAccounted`) — `MarkUsedBulk`'s unconditional OR-merge means
/// it just succeeds again as a second `Confirmed`, paying a second tx fee.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "spawns surfpool subprocess; deploys real .so; requires `just build` first"]
async fn resubmitting_same_noreplay_chunk_reconfirms_rather_than_already_accounted() {
    let (guard, program_id, payer) = boot("cursor-resubmit-noreplay").await;
    let ctx = BackfillCtx::new(program_id, payer.pubkey());
    let config = SubmitterConfig {
        rpc_url: guard.rpc_url(),
        concurrency: 2,
        max_retries: 3,
        initial_retry_delay_ms: 200,
        max_retry_delay_ms: 2_000,
    };

    let chunk = noreplay_chunk(5);

    let stats1 = submit_chunks(
        ctx.clone(),
        payer.insecure_clone(),
        config.clone(),
        vec![chunk.clone()],
    )
    .await
    .expect("first submission");
    assert_eq!(stats1.submitted, 1, "first submission lands normally");
    assert_eq!(stats1.already_accounted, 0);

    // Resubmit the identical chunk — same (chain, emitter, sequence, digest).
    let stats2 = submit_chunks(ctx.clone(), payer.insecure_clone(), config, vec![chunk])
        .await
        .expect("second submission must not error");
    eprintln!("[cursor-resubmit] second-submission stats: {stats2:?}");

    // The resubmission re-lands as a second `Confirmed`, not a classified
    // `already_accounted` skip — the OR-merge is idempotent on-chain, but
    // the operator pays a second real tx fee.
    assert_eq!(
        stats2.already_accounted, 0,
        "documents the gap: MarkUsedBulk never returns AlreadyAccounted, so \
         the submitter can never classify a NoReplay resubmission as \
         already-done — see the doc comment above this test"
    );
    assert_eq!(
        stats2.submitted, 1,
        "the resubmission actually re-lands as a second Confirmed, not a \
         classified already-accounted skip"
    );

    drop(guard);
}

/// Cursor-ahead-of-chain: the cursor claims a chunk confirmed that was
/// never actually submitted. Drives the raw `Cursor`/`submit_chunks`
/// primitives directly (no preflight verification) and shows the skipped
/// chunk's on-chain state is never written — the reason `preflight.rs`
/// exists. `reconcile_balances` is the independent backstop that catches
/// the loss as `missing_from_chain`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "spawns surfpool subprocess; deploys real .so; requires `just build` first"]
async fn cursor_ahead_of_chain_resume_silently_skips_unwritten_chunk() {
    let (guard, program_id, payer) = boot("cursor-ahead-of-chain").await;
    let ctx = BackfillCtx::new(program_id, payer.pubkey());
    let config = SubmitterConfig {
        rpc_url: guard.rpc_url(),
        concurrency: 2,
        max_retries: 3,
        initial_retry_delay_ms: 200,
        max_retry_delay_ms: 2_000,
    };

    // Two independent BackfillBalance chunks — each a single writable PDA,
    // so we can directly probe whether chunk A's PDA exists on chain.
    let chunk_a = balance_chunk(0xA0);
    let chunk_b = balance_chunk(0xB0);
    let chunks = vec![chunk_a, chunk_b];

    let tmp = TempDir::new().expect("tempdir");
    let cursor_path = tmp.path().join("cursor.json");
    let catalogue_path = tmp.path().join("catalogue.jsonl");
    std::fs::write(&catalogue_path, "stub").expect("write catalogue");

    // Cursor claims chunk A confirmed, but it was never actually submitted.
    {
        let mut cursor = Cursor::load_or_init(
            cursor_path.clone(),
            &catalogue_path,
            "0xcursor-ahead-test",
            &program_id.to_string(),
        )
        .expect("init cursor");
        cursor
            .observe_confirmed(Some("fake-sig-never-landed"), 5_000)
            .expect("observe");
        cursor.flush().expect("flush");
    }

    // Resume: reload the cursor fresh and skip the chunks it claims are done.
    let resumed_cursor = Cursor::load_or_init(
        cursor_path,
        &catalogue_path,
        "0xcursor-ahead-test",
        &program_id.to_string(),
    )
    .expect("reload cursor");
    let skip = resumed_cursor.skip_count() as usize;
    assert_eq!(skip, 1, "cursor claims chunk A already confirmed");

    let remaining: Vec<ChunkPlan> = chunks.into_iter().skip(skip).collect();
    assert_eq!(remaining.len(), 1, "only chunk B is resubmitted");

    let stats = submit_chunks(ctx.clone(), payer.insecure_clone(), config, remaining)
        .await
        .expect("resumed submission");
    assert_eq!(stats.submitted, 1);

    // Prove chunk A's PDA was never written: query it directly.
    let async_rpc =
        AsyncRpcClient::new_with_commitment(guard.rpc_url(), CommitmentConfig::confirmed());
    let pda_a =
        ga_backfill::tx_builder::derive_balance_pda(&program_id, 1, 2, &{
            let mut a = [0u8; 32];
            a[31] = 0xA0;
            a
        });
    let account_a = async_rpc.get_account(&pda_a).await;
    eprintln!("[cursor-ahead] chunk A PDA lookup: {account_a:?}");
    assert!(
        account_a.is_err(),
        "GAP CONFIRMED: chunk A's balance PDA does not exist on chain, yet the \
         cursor-driven resume permanently skipped resubmitting it — the resume \
         path has no on-chain cross-check against the cursor's claims"
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
    eprintln!("[cursor-ahead] reconcile report: {report:?}");
    assert!(
        !report.is_clean(),
        "reconcile must NOT be clean — chunk A is genuinely missing on chain"
    );
    assert_eq!(report.missing_from_chain.len(), 1);
    assert_eq!(report.matched, 1, "chunk B is present and correct");

    drop(guard);
}
