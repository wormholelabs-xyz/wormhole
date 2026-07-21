//! End-to-end crash-mid-run / resume simulation against a real surfpool
//! validator.
//!
//! Submits the first half of a planned chunk list, simulates a crash (drops
//! everything and reloads the cursor from disk as a fresh process would),
//! submits only the remaining half using the reloaded cursor's
//! `skip_count()`, then reconciles the full catalogue and asserts a clean
//! match with the right total submitted count — proving no chunk was
//! double-submitted and none was silently skipped.
//!
//! This test wires `Cursor` and `submit_chunks` together itself (advancing
//! the cursor only for chunks it knows, by construction, actually landed);
//! `e2e_run_subcommand_against_surfpool.rs` covers the same claim through
//! the actual `run` orchestrator.
//!
//! `#[ignore]` by default — heavy and requires the backfill `.so` to be
//! built first. Run with:
//!   `just build && cargo test --test e2e_resume_against_surfpool -- --ignored --nocapture`

use std::path::PathBuf;
use std::time::Duration;

use solana_client::nonblocking::rpc_client::RpcClient as AsyncRpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use ga_backfill::catalogue::CatalogueReader;
use ga_backfill::chunker::{ChunkPlan, Chunker};
use ga_backfill::cursor::Cursor;
use ga_backfill::reconcile::reconcile_balances;
use ga_backfill::submitter::{submit_chunks, SubmitterConfig};
use ga_backfill::tx_builder::BackfillCtx;
use global_accountant_definitions::NOREPLAY_PROGRAM_ID;

mod common;
use common::surfpool::{
    deploy_program, noreplay_so_path, parent_so_path, start_surfpool, SurfpoolOptions,
};

const BACKFILL_PROGRAM_NAME: &str = "global_accountant_backfill";

/// 2 accounts (→ 1 BackfillBalance chunk) + 5 distinct-emitter transfers (→ 5
/// separate BackfillNoReplay chunks, one entry each, since the chunker closes
/// a chunk on every emitter change) = 6 total chunks. Enough to split
/// meaningfully into a "first half" / "second half" for the crash simulation.
const TEST_CATALOGUE: &str = concat!(
    "{\"kind\":\"account\",\"chain\":1,\"token_chain\":2,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"balance\":\"0x0000000000000000000000000000000000000000000000000000000000001234\"}\n",
    "{\"kind\":\"account\",\"chain\":1,\"token_chain\":2,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000bb\",\"balance\":\"0x0000000000000000000000000000000000000000000000000000000000005678\"}\n",
    "{\"kind\":\"transfer\",\"chain\":1,\"emitter\":\"0x0000000000000000000000000000000000000000000000000000000000000001\",\"sequence\":1,\"digest\":\"0x0000000000000000000000000000000000000000000000000000000000000a01\",\"amount\":\"0x0000000000000000000000000000000000000000000000000000000000000011\",\"token_chain\":1,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"recipient_chain\":2}\n",
    "{\"kind\":\"transfer\",\"chain\":1,\"emitter\":\"0x0000000000000000000000000000000000000000000000000000000000000002\",\"sequence\":1,\"digest\":\"0x0000000000000000000000000000000000000000000000000000000000000a02\",\"amount\":\"0x0000000000000000000000000000000000000000000000000000000000000022\",\"token_chain\":1,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"recipient_chain\":2}\n",
    "{\"kind\":\"transfer\",\"chain\":1,\"emitter\":\"0x0000000000000000000000000000000000000000000000000000000000000003\",\"sequence\":1,\"digest\":\"0x0000000000000000000000000000000000000000000000000000000000000a03\",\"amount\":\"0x0000000000000000000000000000000000000000000000000000000000000033\",\"token_chain\":1,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"recipient_chain\":2}\n",
    "{\"kind\":\"transfer\",\"chain\":1,\"emitter\":\"0x0000000000000000000000000000000000000000000000000000000000000004\",\"sequence\":1,\"digest\":\"0x0000000000000000000000000000000000000000000000000000000000000a04\",\"amount\":\"0x0000000000000000000000000000000000000000000000000000000000000044\",\"token_chain\":1,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"recipient_chain\":2}\n",
    "{\"kind\":\"transfer\",\"chain\":1,\"emitter\":\"0x0000000000000000000000000000000000000000000000000000000000000005\",\"sequence\":1,\"digest\":\"0x0000000000000000000000000000000000000000000000000000000000000a05\",\"amount\":\"0x0000000000000000000000000000000000000000000000000000000000000055\",\"token_chain\":1,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"recipient_chain\":2}\n",
);

fn write_test_catalogue() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("test_catalogue.jsonl");
    std::fs::write(&path, TEST_CATALOGUE).expect("write catalogue");
    (tmp, path)
}

fn plan_chunks(catalogue_path: &std::path::Path) -> Vec<ChunkPlan> {
    Chunker::new(
        CatalogueReader::open(catalogue_path)
            .expect("open catalogue")
            .filter_map(Result::ok),
    )
    .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns surfpool subprocess; deploys real .so; requires `just build` first"]
async fn resume_after_simulated_crash_completes_with_no_double_submission_and_no_skipped_chunk() {
    // ---------- Boot surfpool + deploy ----------
    let backfill_so_path = parent_so_path(BACKFILL_PROGRAM_NAME);
    let backfill_bytes = std::fs::read(&backfill_so_path).unwrap_or_else(|e| {
        panic!(
            "missing {}: {e}. Run `just build` in the parent workspace first.",
            backfill_so_path.display()
        )
    });
    let noreplay_bytes = std::fs::read(noreplay_so_path()).expect("read noreplay fixture");

    let guard = start_surfpool(SurfpoolOptions::offline("resume-e2e"));
    let rpc_url = guard.rpc_url();

    let program_id = Keypair::new().pubkey();
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

    // ---------- Plan the full chunk list up front (mirrors what a real
    // orchestrator does once per run, before either the first or the
    // resumed submission pass) ----------
    let (_tmp, catalogue_path) = write_test_catalogue();
    let all_chunks = plan_chunks(&catalogue_path);
    eprintln!("[resume-e2e] planned {} chunks total", all_chunks.len());
    assert_eq!(all_chunks.len(), 6, "1 balance chunk + 5 single-entry noreplay chunks");

    let cursor_path = catalogue_path.with_file_name("cursor.json");
    let catalogue_hash = "0xresume-e2e-test-content-hash";

    let ctx = BackfillCtx::new(program_id, payer.pubkey());
    let config = SubmitterConfig {
        rpc_url: rpc_url.clone(),
        concurrency: 1, // serial: this test's own bookkeeping needs to know exactly which/how many chunks landed each phase
        max_retries: 3,
        initial_retry_delay_ms: 200,
        max_retry_delay_ms: 2_000,
    };

    // ============================================================
    // PHASE 1: "first run" — submit only the first half, then crash.
    // ============================================================
    let first_half: Vec<ChunkPlan> = all_chunks.iter().take(3).cloned().collect();
    {
        let mut cursor = Cursor::load_or_init_with_stride(
            cursor_path.clone(),
            &catalogue_path,
            catalogue_hash,
            &program_id.to_string(),
            1, // flush every event — deterministic "crash boundary"
        )
        .expect("init cursor");
        assert_eq!(cursor.skip_count(), 0, "fresh cursor must skip nothing");

        let stats = submit_chunks(
            ctx.clone(),
            payer.insecure_clone(),
            config.clone(),
            first_half.clone(),
        )
        .await
        .expect("first-half submission");
        eprintln!("[resume-e2e] phase 1 stats: {stats:?}");
        assert_eq!(stats.submitted, 3);
        assert_eq!(stats.already_accounted, 0);

        // Advance the cursor once per chunk known (concurrency=1, no error)
        // to have landed.
        for _ in 0..stats.submitted {
            cursor
                .observe_confirmed(None, 5_000)
                .expect("observe confirmed");
        }
        cursor.flush().expect("flush before simulated crash");
        // `cursor` is dropped here, simulating the crash.
    }

    // ============================================================
    // PHASE 2: "resume" — a fresh process reloads the cursor from disk and
    // must submit exactly the remaining chunks: no fewer (a skipped chunk),
    // no more (a double-submission of already-landed work).
    // ============================================================
    let resumed_cursor = Cursor::load_or_init_with_stride(
        cursor_path.clone(),
        &catalogue_path,
        catalogue_hash,
        &program_id.to_string(),
        1,
    )
    .expect("reload cursor after simulated crash");
    let skip = resumed_cursor.skip_count() as usize;
    assert_eq!(skip, 3, "cursor remembers exactly the 3 confirmed chunks");

    let second_half: Vec<ChunkPlan> = all_chunks.into_iter().skip(skip).collect();
    assert_eq!(second_half.len(), 3, "exactly the remaining 3 chunks, none skipped");

    let mut cursor = resumed_cursor;
    let stats = submit_chunks(ctx, payer.insecure_clone(), config, second_half)
        .await
        .expect("resumed submission");
    eprintln!("[resume-e2e] phase 2 (resumed) stats: {stats:?}");
    assert_eq!(stats.submitted, 3, "only the remaining 3 chunks submitted — no double-submission");
    assert_eq!(stats.already_accounted, 0);
    for _ in 0..stats.submitted {
        cursor
            .observe_confirmed(None, 5_000)
            .expect("observe confirmed");
    }
    cursor.flush().expect("final flush");
    assert_eq!(cursor.skip_count(), 6, "cursor reflects all 6 chunks across both phases");
    assert_eq!(cursor.state().submitted, 6);

    // ============================================================
    // Reconcile the FULL catalogue against on-chain state: a clean match
    // proves every chunk from both phases landed exactly once — nothing
    // double-submitted, nothing silently skipped.
    // ============================================================
    let report = reconcile_balances(&async_rpc, &program_id, &catalogue_path)
        .await
        .expect("reconcile");
    eprintln!(
        "[resume-e2e] reconcile: matched={} mismatched={} missing={} unexpected={}",
        report.matched,
        report.mismatched.len(),
        report.missing_from_chain.len(),
        report.unexpected_on_chain.len()
    );
    assert!(report.is_clean(), "reconcile diffs after resume: {report:#?}");
    assert_eq!(report.matched, 2, "both Balance entries present and correct");

    eprintln!("[resume-e2e] resume-after-crash completed cleanly, no double-submission, no skipped chunk");
}
