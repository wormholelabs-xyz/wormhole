//! End-to-end orchestrator integration test against a real surfpool validator.
//!
//! Spins up surfpool, deploys the backfill program + noreplay, writes a tiny
//! handcrafted snapshot file, then drives the full orchestrator path:
//! snapshot read → batcher → tx_builder → submitter → progress file → parity check.
//! Asserts on-chain state matches the snapshot and the `BACKFILL_AUTHORITY`
//! const-check rejects a tx signed by a stranger.
//!
//! `#[ignore]` by default — heavy and requires the backfill `.so` to be
//! built first. Run with:
//!   `just tools-e2e`

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

use global_accountant_definitions::{Uint256, NOREPLAY_PROGRAM_ID};
use svm_accountant_backfill::progress::ProgressFile;
use svm_accountant_backfill::run::plan_batches;
use svm_accountant_backfill::submitter::{submit_batches, SubmitterConfig};
use svm_accountant_backfill::tx_builder::build_backfill_noreplay_ix;
use svm_accountant_tools_common::accountant::Accountant;
use svm_accountant_tools_common::ids::{Address, ChainId, Digest, Sequence};
use svm_accountant_tools_common::snapshot::read_rows;

mod common;
use svm_accountant_tools_common::test_support::surfpool::{
    deploy_program, noreplay_so_path, parent_so_path, start_surfpool, SurfpoolOptions,
};

const BACKFILL_PROGRAM_NAME: &str = "global_accountant_backfill";

const TEST_SNAPSHOT: &str = concat!(
    // 2 accounts so we get one Balance batch
    "{\"kind\":\"account\",\"chain\":1,\"token_chain\":2,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"balance\":\"0x0000000000000000000000000000000000000000000000000000000000001234\"}\n",
    "{\"kind\":\"account\",\"chain\":1,\"token_chain\":2,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000bb\",\"balance\":\"0x0000000000000000000000000000000000000000000000000000000000005678\"}\n",
    // 1 registration → 1 BackfillChainRegistration batch
    "{\"kind\":\"registration\",\"chain\":2,\"registered_emitter\":\"0x0000000000000000000000000000000000000000000000000000000000000001\",\"sequence\":18159247149180908832}\n",
    // 3 transfers same emitter → 1 BackfillNoReplay batch
    "{\"kind\":\"transfer\",\"chain\":1,\"emitter\":\"0x00000000000000000000000000000000000000000000000000000000000000ec\",\"sequence\":5,\"digest\":\"0x000000000000000000000000000000000000000000000000000000000000aaaa\",\"amount\":\"0x0000000000000000000000000000000000000000000000000000000000000001\",\"token_chain\":1,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"recipient_chain\":2}\n",
    "{\"kind\":\"transfer\",\"chain\":1,\"emitter\":\"0x00000000000000000000000000000000000000000000000000000000000000ec\",\"sequence\":6,\"digest\":\"0x000000000000000000000000000000000000000000000000000000000000bbbb\",\"amount\":\"0x0000000000000000000000000000000000000000000000000000000000000002\",\"token_chain\":1,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"recipient_chain\":2}\n",
    "{\"kind\":\"transfer\",\"chain\":1,\"emitter\":\"0x00000000000000000000000000000000000000000000000000000000000000ec\",\"sequence\":7,\"digest\":\"0x000000000000000000000000000000000000000000000000000000000000cccc\",\"amount\":\"0x0000000000000000000000000000000000000000000000000000000000000003\",\"token_chain\":1,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"recipient_chain\":2}\n",
);

fn write_test_snapshot() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("test_snapshot.jsonl");
    std::fs::write(&path, TEST_SNAPSHOT).expect("write snapshot");
    (tmp, path)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns surfpool subprocess; deploys real .so; run via `just tools-e2e`"]
async fn e2e_orchestrator_full_lifecycle() {
    // ---------- Boot surfpool + deploy ----------
    let backfill_so_path = parent_so_path(BACKFILL_PROGRAM_NAME);
    let backfill_bytes = std::fs::read(&backfill_so_path).unwrap_or_else(|e| {
        panic!(
            "missing {}: {e}. Run `just build-devnet` in the parent workspace first.",
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

    // ---------- Build snapshot + progress file ----------
    let (_tmp, snapshot_path) = write_test_snapshot();
    let snapshot_hash = "0xe2e-test-content-hash";
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
        Accountant::Wtt,
    )
    .expect("plan");
    eprintln!("[e2e] planned {} batches", batches.len());
    // Expect: 1 BackfillBalance + 1 BackfillChainRegistration + 1 BackfillNoReplay = 3
    assert_eq!(batches.len(), 3);

    // ---------- Run orchestrator submitter ----------
    let target = common::target(Accountant::Wtt, program_id, payer.pubkey());
    let payer_clone = payer.insecure_clone();
    let config = SubmitterConfig {
        concurrency: 4,
        max_retries: 3,
        initial_retry_delay_ms: 200,
        max_retry_delay_ms: 2_000,
    };
    let stats = submit_batches(
        Arc::new(common::async_rpc_for(&guard)),
        target.clone(),
        payer_clone,
        config,
        batches,
    )
    .await
    .expect("submit_batches");
    eprintln!("[e2e] submission stats: {stats:?}");
    // 3 backfill txs: NoReplay, Balance, ChainRegistration.
    assert_eq!(stats.confirmed, 3);
    assert_eq!(stats.already_written, 0);

    // ---------- Parity check ----------
    let findings = common::parity_findings(&rpc_url, &target, &snapshot_path).await;
    assert!(findings.is_empty(), "parity findings: {findings:#?}");

    // ---------- Wrong-signer control: stranger must be rejected ----------
    //
    // Builds the same `BackfillNoReplay` ix the authority sends, but signs
    // with a fresh keypair. The program's `require_authority` check must
    // reject with `UnauthorizedCaller` (Custom(33)).
    let stranger = Keypair::new();
    async_rpc
        .request_airdrop(&stranger.pubkey(), 1_000_000_000)
        .await
        .expect("airdrop stranger");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let replay_batch = vec![svm_accountant_tools_common::snapshot::TransferRow {
        chain: ChainId(1),
        emitter: Address({
            let mut e = [0u8; 32];
            e[31] = 0xec;
            e
        }),
        sequence: Sequence(100), // fresh seq, not previously written
        digest: Digest([0xdd; 32]),
        amount: Uint256([0u8; 32]),
        token_chain: ChainId(1),
        token_address: Address({
            let mut a = [0u8; 32];
            a[31] = 0xaa;
            a
        }),
        recipient_chain: ChainId(2),
    }];
    // Swap the payer meta after building, bypassing `BackfillTarget::try_new`'s
    // check so the on-chain authority check is what rejects the tx.
    let mut stranger_ix = build_backfill_noreplay_ix(&target, &replay_batch);
    stranger_ix.accounts[0].pubkey = stranger.pubkey();
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
        err_msg.contains("0x21"),
        "expected UnauthorizedCaller (custom error 33) in error message; got: {err_msg}"
    );

    eprintln!("[e2e] all phases green");
}
