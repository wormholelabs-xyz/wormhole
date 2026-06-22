//! End-to-end orchestrator integration test against a real surfpool validator.
//!
//! Spins up surfpool, deploys the backfill program + noreplay, writes a tiny
//! handcrafted catalogue file, then drives the full orchestrator path:
//! catalogue read → chunker → tx_builder → submitter → cursor → reconcile.
//! Asserts on-chain state matches the catalogue and the `BACKFILL_AUTHORITY`
//! const-check rejects a tx signed by a stranger.
//!
//! `#[ignore]` by default — heavy and requires the backfill `.so` to be
//! built first. Run with:
//!   `just build && cargo test --test e2e_against_surfpool -- --ignored --nocapture`

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
use ga_backfill::reconcile::reconcile_balances;
use ga_backfill::submitter::{submit_chunks, SubmitterConfig};
use ga_backfill::tx_builder::{build_backfill_noreplay_ix, BackfillCtx};
use global_accountant_definitions::NOREPLAY_PROGRAM_ID;

mod common;
use common::surfpool::{
    deploy_program, noreplay_so_path, parent_so_path, start_surfpool, SurfpoolOptions,
};

const BACKFILL_PROGRAM_NAME: &str = "global_accountant_backfill";

const TEST_CATALOGUE: &str = concat!(
    // 2 accounts so we get one Balance chunk
    "{\"kind\":\"account\",\"chain\":1,\"token_chain\":2,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"balance\":\"0x0000000000000000000000000000000000000000000000000000000000001234\"}\n",
    "{\"kind\":\"account\",\"chain\":1,\"token_chain\":2,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000bb\",\"balance\":\"0x0000000000000000000000000000000000000000000000000000000000005678\"}\n",
    // 1 registration — orchestrator will skip (Phase 7)
    "{\"kind\":\"registration\",\"chain\":2,\"registered_emitter\":\"0x0000000000000000000000000000000000000000000000000000000000000001\"}\n",
    // 3 transfers same emitter → 1 BackfillNoReplay chunk
    "{\"kind\":\"transfer\",\"chain\":1,\"emitter\":\"0x00000000000000000000000000000000000000000000000000000000000000ec\",\"sequence\":5,\"digest\":\"0x000000000000000000000000000000000000000000000000000000000000aaaa\",\"amount\":\"0x0000000000000000000000000000000000000000000000000000000000000001\",\"token_chain\":1,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"recipient_chain\":2}\n",
    "{\"kind\":\"transfer\",\"chain\":1,\"emitter\":\"0x00000000000000000000000000000000000000000000000000000000000000ec\",\"sequence\":6,\"digest\":\"0x000000000000000000000000000000000000000000000000000000000000bbbb\",\"amount\":\"0x0000000000000000000000000000000000000000000000000000000000000002\",\"token_chain\":1,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"recipient_chain\":2}\n",
    "{\"kind\":\"transfer\",\"chain\":1,\"emitter\":\"0x00000000000000000000000000000000000000000000000000000000000000ec\",\"sequence\":7,\"digest\":\"0x000000000000000000000000000000000000000000000000000000000000cccc\",\"amount\":\"0x0000000000000000000000000000000000000000000000000000000000000003\",\"token_chain\":1,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"recipient_chain\":2}\n",
);

fn write_test_catalogue() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("test_catalogue.jsonl");
    std::fs::write(&path, TEST_CATALOGUE).expect("write catalogue");
    (tmp, path)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns surfpool subprocess; deploys real .so; requires `just build` first"]
async fn e2e_orchestrator_full_lifecycle() {
    // ---------- Boot surfpool + deploy ----------
    let backfill_so_path = parent_so_path(BACKFILL_PROGRAM_NAME);
    let backfill_bytes = std::fs::read(&backfill_so_path).unwrap_or_else(|e| {
        panic!(
            "missing {}: {e}. Run `just build` in the parent workspace first.",
            backfill_so_path.display()
        )
    });
    let noreplay_bytes = std::fs::read(noreplay_so_path()).expect("read noreplay fixture");
    eprintln!(
        "[e2e] backfill_so={} bytes, noreplay_so={} bytes",
        backfill_bytes.len(),
        noreplay_bytes.len()
    );

    let guard = start_surfpool(SurfpoolOptions::offline("orchestrator-e2e"));
    let rpc_url = guard.rpc_url();

    let program_id = Keypair::new().pubkey();
    deploy_program(&rpc_url, &program_id, &backfill_bytes);
    deploy_program(
        &rpc_url,
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        &noreplay_bytes,
    );
    eprintln!("[e2e] program_id={program_id}");

    // ---------- Fund payer (must equal `BACKFILL_AUTHORITY`) ----------
    let payer = Keypair::new_from_array([1u8; 32]);
    let async_rpc =
        AsyncRpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());
    async_rpc
        .request_airdrop(&payer.pubkey(), 10_000_000_000_000)
        .await
        .expect("airdrop");
    tokio::time::sleep(Duration::from_millis(500)).await;
    eprintln!("[e2e] payer={}", payer.pubkey());

    // ---------- Build catalogue + cursor ----------
    let (_tmp, catalogue_path) = write_test_catalogue();
    let catalogue_hash = "0xe2e-test-content-hash";
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
    eprintln!("[e2e] planned {} chunks", chunks.len());
    // Expect: 1 BackfillBalance + 1 DeferredRegistration + 1 BackfillNoReplay = 3
    assert_eq!(chunks.len(), 3);

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
    let stats = submit_chunks(ctx.clone(), payer_clone, config, chunks)
        .await
        .expect("submit_chunks");
    eprintln!("[e2e] submission stats: {stats:?}");
    // 2 backfill txs (1 NoReplay + 1 Balance) submitted, 1 deferred (registration) skipped.
    assert_eq!(stats.submitted, 2);
    assert_eq!(stats.deferred_skipped, 1);
    assert_eq!(stats.already_accounted, 0);

    // ---------- Reconcile ----------
    let report = reconcile_balances(&async_rpc, &program_id, &catalogue_path)
        .await
        .expect("reconcile");
    eprintln!(
        "[e2e] reconcile: matched={} mismatched={} missing={} unexpected={}",
        report.matched,
        report.mismatched.len(),
        report.missing_from_chain.len(),
        report.unexpected_on_chain.len()
    );
    assert!(report.is_clean(), "reconcile diffs: {report:#?}");
    assert_eq!(report.matched, 2);

    // ---------- Wrong-signer control: stranger must be rejected ----------
    //
    // Builds the same `BackfillNoReplay` ix the authority sends, but signs
    // with a fresh keypair. The program's `require_authority` check must
    // reject with `UnauthorizedCaller` (Custom(3)) — that's the only thing
    // standing between an attacker and arbitrary state writes.
    let stranger = Keypair::new();
    async_rpc
        .request_airdrop(&stranger.pubkey(), 1_000_000_000)
        .await
        .expect("airdrop stranger");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let replay_chunk = vec![ga_backfill::catalogue::TransferRecord {
        chain: 1,
        emitter: {
            let mut e = [0u8; 32];
            e[31] = 0xec;
            e
        },
        sequence: 100, // fresh seq, not previously written
        digest: [0xdd; 32],
        amount: [0u8; 32],
        token_chain: 1,
        token_address: {
            let mut a = [0u8; 32];
            a[31] = 0xaa;
            a
        },
        recipient_chain: 2,
    }];
    // Bypass `BackfillCtx::new`'s assertion — we deliberately want a ctx
    // whose payer != BACKFILL_AUTHORITY so we can hit the on-chain check.
    let stranger_ctx = BackfillCtx {
        program_id,
        payer: stranger.pubkey(),
        system_program: ctx.system_program,
        noreplay_program: ctx.noreplay_program,
    };
    let stranger_ix = build_backfill_noreplay_ix(&stranger_ctx, &replay_chunk);
    let blockhash = async_rpc.get_latest_blockhash().await.expect("blockhash");
    let stranger_tx = Transaction::new_signed_with_payer(
        std::slice::from_ref(&stranger_ix),
        Some(&stranger.pubkey()),
        &[&stranger],
        blockhash,
    );
    let result = async_rpc.send_and_confirm_transaction(&stranger_tx).await;
    eprintln!("[e2e] wrong-signer result: {result:?}");
    assert!(result.is_err(), "wrong-signer submission must fail");
    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(
        err_msg.contains("0x3"),
        "expected UnauthorizedCaller (custom error 3) in error message; got: {err_msg}"
    );

    eprintln!("[e2e] all phases green");
}
