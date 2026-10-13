//! Resume and idempotency against a real deployed program (surfpool).
//!
//! `#[ignore]` by default — heavy, spawns a subprocess, and requires the
//! backfill `.so` to be built first. Run with `just tools-e2e`.

mod common;

use std::sync::Arc;
use std::time::Duration;

use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use tempfile::TempDir;

use global_accountant_definitions::{BalanceAccountKey, Uint256, NOREPLAY_PROGRAM_ID};
use svm_accountant_backfill::batcher::TxBatch;
use svm_accountant_backfill::progress::ProgressFile;
use svm_accountant_backfill::submitter::{submit_batches, SubmitterConfig};
use svm_accountant_tools_common::accountant::Accountant;
use svm_accountant_tools_common::ids::{Address, ChainId, Digest, Sequence};
use svm_accountant_tools_common::layouts::derive_pda;
use svm_accountant_tools_common::parity::state::{FindingKind, StateKind};
use svm_accountant_tools_common::snapshot::{BalanceRow, TransferRow};
use svm_accountant_tools_common::test_support::surfpool::{
    deploy_program, noreplay_so_path, parent_so_path, start_surfpool, SurfpoolGuard,
    SurfpoolOptions,
};

const BACKFILL_PROGRAM_NAME: &str = "global_accountant_backfill";

fn noreplay_batch(sequence: u64) -> TxBatch {
    let mut emitter = [0u8; 32];
    emitter[31] = 0xec;
    let mut token_address = [0u8; 32];
    token_address[31] = 0xaa;
    TxBatch::BackfillNoReplay(vec![TransferRow {
        chain: ChainId(1),
        emitter: Address(emitter),
        sequence: Sequence(sequence),
        digest: Digest([0xaa; 32]),
        amount: Uint256::ZERO,
        token_chain: ChainId(1),
        token_address: Address(token_address),
        recipient_chain: ChainId(2),
    }])
}

fn token_address(seed: u8) -> [u8; 32] {
    let mut a = [0u8; 32];
    a[31] = seed;
    a
}

fn balance_row(addr_seed: u8) -> BalanceRow {
    BalanceRow {
        chain: ChainId(1),
        token_chain: ChainId(2),
        token_address: Address(token_address(addr_seed)),
        balance: Uint256::from_u128(0x42),
    }
}

fn submitter_config() -> SubmitterConfig {
    SubmitterConfig {
        concurrency: 2,
        max_retries: 3,
        initial_retry_delay_ms: 200,
        max_retry_delay_ms: 2_000,
    }
}

async fn boot(scratch_prefix: &'static str) -> (SurfpoolGuard, Pubkey, Keypair) {
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
    common::async_rpc_for(&guard)
        .request_airdrop(&payer.pubkey(), 10_000_000_000_000)
        .await
        .expect("airdrop");
    tokio::time::sleep(Duration::from_millis(500)).await;

    (guard, program_id, payer)
}

/// Resubmitting an already-landed `BackfillNoReplay` batch fails with custom
/// error 7 (`AlreadyAccounted`); the submitter counts it already written after
/// verifying the batch on chain.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "spawns surfpool subprocess; deploys real .so; run via `just tools-e2e`"]
async fn resubmitting_same_noreplay_batch_is_already_written() {
    let (guard, program_id, payer) = boot("progress-resubmit-noreplay").await;
    let target = common::target(Accountant::Wtt, program_id, payer.pubkey());
    let rpc = Arc::new(common::async_rpc_for(&guard));
    let batch = noreplay_batch(5);

    let stats1 = submit_batches(
        Arc::clone(&rpc),
        target.clone(),
        payer.insecure_clone(),
        submitter_config(),
        vec![batch.clone()],
    )
    .await
    .expect("first submission");
    assert_eq!(stats1.confirmed, 1, "first submission lands normally");
    assert_eq!(stats1.already_written, 0);

    // Resubmit the identical batch — same (chain, emitter, sequence, digest).
    let stats2 = submit_batches(
        rpc,
        target,
        payer.insecure_clone(),
        submitter_config(),
        vec![batch],
    )
    .await
    .expect("second submission must not error");
    eprintln!("[progress-resubmit] second-submission stats: {stats2:?}");

    // `reject_if_marked` fails the replayed batch at its first entry.
    assert_eq!(
        stats2.already_written, 1,
        "replay classified as already written"
    );
    assert_eq!(stats2.confirmed, 0, "replay lands no second transaction");
}

/// A progress file ahead of the chain: it claims a batch confirmed that was
/// never submitted. Driving the raw `ProgressFile`/`submit_batches` primitives
/// skips that batch for good — the reason `onchain_check.rs` exists. The parity
/// check catches the loss as a Balances finding absent on Solana.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "spawns surfpool subprocess; deploys real .so; run via `just tools-e2e`"]
async fn progress_ahead_of_chain_resume_silently_skips_unwritten_batch() {
    let (guard, program_id, payer) = boot("progress-ahead-of-chain").await;
    let target = common::target(Accountant::Wtt, program_id, payer.pubkey());

    // Two independent BackfillBalance batches — each a single writable PDA,
    // so we can directly probe whether batch A's PDA exists on chain.
    let rows = [balance_row(0xA0), balance_row(0xB0)];
    let batches: Vec<TxBatch> = rows
        .iter()
        .map(|row| TxBatch::BackfillBalance(vec![row.clone()]))
        .collect();

    let tmp = TempDir::new().expect("tempdir");
    let progress_path = tmp.path().join("progress.json");
    let snapshot_path = tmp.path().join("snapshot.jsonl");
    std::fs::write(&snapshot_path, "stub").expect("write snapshot");
    let identity = common::test_identity("0xprogress-ahead-test", &program_id.to_string());

    // The progress file claims batch A confirmed, but it was never submitted.
    {
        let mut progress_file =
            ProgressFile::open(progress_path.clone(), &snapshot_path, &identity, 1)
                .expect("init progress file");
        progress_file
            .observe_confirmed(Some("fake-sig-never-landed"), 5_000)
            .expect("observe");
        progress_file.flush().expect("flush");
    }

    // Resume: reload the progress file and skip the batches it claims are done.
    let resumed_progress = ProgressFile::open(progress_path, &snapshot_path, &identity, 1)
        .expect("reload progress file");
    let skip = resumed_progress.batches_done() as usize;
    assert_eq!(skip, 1, "progress file claims batch A already confirmed");

    let remaining: Vec<TxBatch> = batches.into_iter().skip(skip).collect();
    assert_eq!(remaining.len(), 1, "only batch B is resubmitted");

    let stats = submit_batches(
        Arc::new(common::async_rpc_for(&guard)),
        target.clone(),
        payer.insecure_clone(),
        submitter_config(),
        remaining,
    )
    .await
    .expect("resumed submission");
    assert_eq!(stats.confirmed, 1);

    // Batch A's PDA was never written.
    let pda_a = derive_pda(
        &program_id,
        &BalanceAccountKey::new(1, 2, token_address(0xA0)),
    );
    let account_a = common::async_rpc_for(&guard).get_account(&pda_a).await;
    eprintln!("[progress-ahead] batch A PDA lookup: {account_a:?}");
    assert!(
        account_a.is_err(),
        "batch A's balance PDA does not exist on chain, yet the resume driven by \
         the progress file skipped it for good"
    );

    // The parity check catches the loss: one Balances finding, absent on Solana.
    let snapshot_rows: String = rows
        .iter()
        .map(|row| {
            format!(
                "{{\"kind\":\"account\",\"chain\":1,\"token_chain\":2,\"token_address\":\"{}\",\"balance\":\"0x{}\"}}\n",
                row.token_address,
                hex::encode(row.balance.0)
            )
        })
        .collect();
    std::fs::write(&snapshot_path, snapshot_rows).expect("write snapshot rows");
    let findings = common::parity_findings(&guard.rpc_url(), &target, &snapshot_path).await;
    eprintln!("[progress-ahead] parity findings: {findings:?}");
    assert_eq!(findings.len(), 1, "batch A is missing on chain");
    assert_eq!(findings[0].kind, FindingKind::State(StateKind::BALANCES));
    assert!(findings[0].expected.is_some(), "batch A is expected");
    assert!(findings[0].actual.is_none(), "batch A is absent on Solana");
}
