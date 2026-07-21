//! Unit tests for the submitter's error classifier, plus orchestration tests
//! against a hand-rolled mock JSON-RPC server (`tests/common/mock_rpc.rs`).
//!
//! Integration tests against a real validator land in Phase 9
//! (`tests/e2e_against_surfpool.rs`) — they're heavier and require surfpool
//! deployment. The classifier tests below cover pure parsing/classification
//! logic with no I/O; the mock-RPC tests further down drive the actual
//! `submit_chunks` orchestration loop (retry/backoff, halt behavior,
//! concurrent stats aggregation) end to end.

mod common;

use ga_backfill::catalogue::{
    AccountRecord, ModificationRecord, ModifyKind, RegistrationRecord, TransferRecord,
};
use ga_backfill::chunker::ChunkPlan;
use ga_backfill::submitter::{
    extract_custom_program_error, submit_chunks, SubmitterConfig, ALREADY_ACCOUNTED_CUSTOM,
    UNAUTHORIZED_CALLER_CUSTOM,
};
use ga_backfill::tx_builder::{backfill_authority_pubkey, BackfillCtx};
use solana_keypair::Keypair;

use common::mock_rpc::{MockRpc, SendTxBehavior};

#[test]
fn extracts_already_accounted_hex_code() {
    // ALREADY_ACCOUNTED = 7 → 0x7
    let msg = "RpcError(\"Transaction simulation failed: Error processing Instruction 0: custom program error: 0x7\")";
    assert_eq!(extract_custom_program_error(msg), Some(7));
    assert_eq!(
        extract_custom_program_error(msg),
        Some(ALREADY_ACCOUNTED_CUSTOM)
    );
}

#[test]
fn extracts_unauthorized_caller_hex_code() {
    // UNAUTHORIZED_CALLER = 3 → 0x3
    let msg =
        "Transaction simulation failed: Error processing Instruction 0: custom program error: 0x3";
    assert_eq!(extract_custom_program_error(msg), Some(3));
    assert_eq!(
        extract_custom_program_error(msg),
        Some(UNAUTHORIZED_CALLER_CUSTOM)
    );
}

#[test]
fn extracts_arbitrary_hex_codes() {
    assert_eq!(
        extract_custom_program_error("custom program error: 0x1"),
        Some(1)
    );
    assert_eq!(
        extract_custom_program_error("custom program error: 0x1a"),
        Some(26)
    );
    assert_eq!(
        extract_custom_program_error("custom program error: 0xff"),
        Some(255)
    );
}

#[test]
fn returns_none_for_non_program_errors() {
    assert_eq!(
        extract_custom_program_error("transport error: connection refused"),
        None
    );
    assert_eq!(extract_custom_program_error(""), None);
    assert_eq!(extract_custom_program_error("blockhash not found"), None);
}

#[test]
fn stops_at_non_hex_character() {
    // Hex parsing stops at the first non-hex char.
    let msg = "custom program error: 0x7; logs: [...]";
    assert_eq!(extract_custom_program_error(msg), Some(7));
}

#[test]
fn handles_multi_digit_codes_in_context() {
    // Real Solana RPC error wrapping with surrounding context.
    let msg = r#"RpcResponseError { code: -32002, message: "Transaction simulation failed: Error processing Instruction 0: custom program error: 0x1d", data: ... }"#;
    assert_eq!(extract_custom_program_error(msg), Some(0x1d));
}

// Orchestration tests against a hand-rolled mock JSON-RPC server
// (`tests/common/mock_rpc.rs`): drive the actual `submit_chunks` async loop
// (retry/backoff, halt behavior, concurrent stats aggregation) over a real
// local HTTP endpoint, since `submit_chunks` always constructs a real
// `HttpSender`-backed `RpcClient` internally.

fn authority_keypair() -> Keypair {
    // Must equal `BACKFILL_AUTHORITY` — `BackfillCtx::new` asserts this.
    // Same fixed keypair the surfpool e2e tests use.
    Keypair::new_from_array([1u8; 32])
}

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
        balance: [0u8; 32],
    }])
}

#[tokio::test]
async fn retries_on_transient_error_then_succeeds() {
    let mock = MockRpc::start(vec![
        SendTxBehavior::TransientError,
        SendTxBehavior::TransientError,
        SendTxBehavior::Success,
    ]);
    let ctx = BackfillCtx::new(solana_pubkey::Pubkey::new_unique(), backfill_authority_pubkey());
    let config = SubmitterConfig {
        rpc_url: mock.url(),
        concurrency: 1,
        max_retries: 5,
        initial_retry_delay_ms: 1,
        max_retry_delay_ms: 5,
    };

    let stats = submit_chunks(
        ctx,
        authority_keypair(),
        config,
        vec![noreplay_chunk(1)],
    )
    .await
    .expect("eventually succeeds after retrying transient errors");

    assert_eq!(stats.submitted, 1);
    assert_eq!(stats.already_accounted, 0);
    assert_eq!(
        mock.send_transaction_calls(),
        3,
        "2 transient failures + 1 success = 3 sendTransaction calls"
    );

    // Two transient failures were scripted (attempts 1 and 2 fail, attempt
    // 3 succeeds), so `SubmissionStats.retries` should read 2.
    assert_eq!(
        stats.retries, 2,
        "2 transient failures were retried before the 3rd attempt succeeded; \
         SubmissionStats.retries should reflect that"
    );
}

#[tokio::test]
async fn halt_on_unrecoverable_error_stops_dispatch_of_remaining_chunks() {
    // Custom program error 99 is not 7 (AlreadyAccounted) or 3
    // (UnauthorizedCaller) → `classify()` returns `Halt`. `concurrency: 1`
    // makes the loop fully serial so call order is deterministic.
    let mock = MockRpc::start(vec![
        SendTxBehavior::CustomProgramError(99),
        SendTxBehavior::Success,
        SendTxBehavior::Success,
        SendTxBehavior::Success,
        SendTxBehavior::Success,
    ]);
    let ctx = BackfillCtx::new(solana_pubkey::Pubkey::new_unique(), backfill_authority_pubkey());
    let config = SubmitterConfig {
        rpc_url: mock.url(),
        concurrency: 1,
        max_retries: 3,
        initial_retry_delay_ms: 1,
        max_retry_delay_ms: 5,
    };

    let chunks = vec![
        noreplay_chunk(1),
        balance_chunk(1),
        balance_chunk(2),
        balance_chunk(3),
        balance_chunk(4),
    ];
    let result = submit_chunks(ctx, authority_keypair(), config, chunks).await;

    assert!(
        result.is_err(),
        "an unrecoverable custom program error must surface as an Err"
    );

    // With `concurrency: 1`, the first chunk fails outright (Halt, not
    // Retry), so only ONE `sendTransaction` call should have happened.
    // The sleep is a margin of safety against unrelated event-loop timing.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(
        mock.send_transaction_calls(),
        1,
        "halt must stop dispatch of every chunk after the failing one — only \
         the first chunk's sendTransaction call should have happened"
    );
}

#[tokio::test]
async fn concurrent_submission_aggregates_stats_across_all_tasks_not_just_the_last() {
    let mock = MockRpc::start(vec![SendTxBehavior::Success; 8]);
    let ctx = BackfillCtx::new(solana_pubkey::Pubkey::new_unique(), backfill_authority_pubkey());
    let config = SubmitterConfig {
        rpc_url: mock.url(),
        concurrency: 4,
        max_retries: 3,
        initial_retry_delay_ms: 1,
        max_retry_delay_ms: 5,
    };

    // 8 real backfill chunks in flight concurrently (4 at a time), plus 2
    // deferred chunks the submitter must skip-and-count rather than send.
    let mut chunks: Vec<ChunkPlan> = (0..8u64).map(noreplay_chunk).collect();
    chunks.push(ChunkPlan::DeferredModification(ModificationRecord {
        sequence: 1,
        chain_id: 1,
        token_chain: 1,
        token_address: [0u8; 32],
        amount: [0u8; 32],
        reason: "test".into(),
        modify_kind: ModifyKind::Add,
    }));
    chunks.push(ChunkPlan::DeferredRegistration(RegistrationRecord {
        chain: 2,
        registered_emitter: [0u8; 32],
    }));

    let stats = submit_chunks(ctx, authority_keypair(), config, chunks)
        .await
        .expect("all chunks succeed");

    assert_eq!(stats.submitted, 8, "every concurrent chunk counted, not just the last");
    assert_eq!(stats.deferred_skipped, 2);
    assert_eq!(stats.already_accounted, 0);
    assert_eq!(mock.send_transaction_calls(), 8);
}

#[tokio::test]
async fn halt_awaits_already_in_flight_work_instead_of_abandoning_it() {
    // concurrency: 3 with 6 chunks: the first 3 are all dispatched
    // concurrently before any of them can complete.
    let mock = MockRpc::start(vec![
        SendTxBehavior::Success,
        SendTxBehavior::CustomProgramError(99),
        SendTxBehavior::Success,
    ]);
    let ctx = BackfillCtx::new(solana_pubkey::Pubkey::new_unique(), backfill_authority_pubkey());
    let config = SubmitterConfig {
        rpc_url: mock.url(),
        concurrency: 3,
        max_retries: 3,
        initial_retry_delay_ms: 1,
        max_retry_delay_ms: 5,
    };

    let chunks: Vec<ChunkPlan> = (0..6u64).map(noreplay_chunk).collect();
    let result = submit_chunks(ctx, authority_keypair(), config, chunks).await;

    assert!(result.is_err(), "one unrecoverable error must surface as Err");
    assert_eq!(
        mock.send_transaction_calls(),
        3,
        "only the first in-flight batch (concurrency=3) should have been \
         dispatched; the halt flag must stop the other 3 before they start"
    );
}

#[tokio::test]
async fn progress_channel_reports_every_chunk_with_its_original_index() {
    // `run`'s resume bookkeeping (main.rs) depends on `ChunkProgress::index`
    // identifying which original chunk a completion belongs to, since
    // concurrent completion order need not match dispatch order.
    let mock = MockRpc::start(vec![SendTxBehavior::Success; 4]);
    let ctx = BackfillCtx::new(solana_pubkey::Pubkey::new_unique(), backfill_authority_pubkey());
    let config = SubmitterConfig {
        rpc_url: mock.url(),
        concurrency: 4,
        max_retries: 3,
        initial_retry_delay_ms: 1,
        max_retry_delay_ms: 5,
    };

    let mut chunks: Vec<ChunkPlan> = (0..4u64).map(noreplay_chunk).collect();
    chunks.insert(
        2,
        ChunkPlan::DeferredModification(ModificationRecord {
            sequence: 1,
            chain_id: 1,
            token_chain: 1,
            token_address: [0u8; 32],
            amount: [0u8; 32],
            reason: "test".into(),
            modify_kind: ModifyKind::Add,
        }),
    );
    // chunks: [noreplay0, noreplay1, deferred(idx 2), noreplay2, noreplay3]

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let stats = ga_backfill::submitter::submit_chunks_with_progress(
        ctx,
        authority_keypair(),
        config,
        chunks,
        Some(tx),
    )
    .await
    .expect("all chunks succeed");

    assert_eq!(stats.submitted, 4);
    assert_eq!(stats.deferred_skipped, 1);

    let mut seen: std::collections::BTreeMap<usize, String> = std::collections::BTreeMap::new();
    while let Ok(p) = rx.try_recv() {
        let kind = match p.outcome {
            ga_backfill::submitter::ChunkOutcome::Confirmed(_) => "confirmed",
            ga_backfill::submitter::ChunkOutcome::AlreadyAccounted => "already_accounted",
            ga_backfill::submitter::ChunkOutcome::DeferredSkipped => "deferred",
        };
        seen.insert(p.index, kind.to_string());
    }

    assert_eq!(seen.len(), 5, "every one of the 5 original chunks must be reported exactly once");
    assert_eq!(seen[&0], "confirmed");
    assert_eq!(seen[&1], "confirmed");
    assert_eq!(seen[&2], "deferred", "the deferred chunk keeps its original index (2)");
    assert_eq!(seen[&3], "confirmed");
    assert_eq!(seen[&4], "confirmed");
}
