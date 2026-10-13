//! Unit tests for the submitter's error classifier, plus orchestration tests
//! against a hand-rolled mock JSON-RPC server (`tests/common/mock_rpc.rs`).
//!
//! Real-validator coverage: `tests/e2e_*_against_surfpool.rs`. The classifier tests below cover pure parsing/classification
//! logic with no I/O; the mock-RPC tests further down drive the actual
//! `submit_batches` orchestration loop (retry/backoff, halt behavior,
//! concurrent stats aggregation) end to end.

mod common;

use global_accountant_definitions::Uint256;
use std::sync::Arc;
use svm_accountant_backfill::batcher::TxBatch;
use svm_accountant_backfill::submitter::{
    parse_custom_error_code, submit_batches, SubmitterConfig, ALREADY_ACCOUNTED_ERROR_CODE,
    UNAUTHORIZED_CALLER_ERROR_CODE,
};
use svm_accountant_backfill::target::BackfillTarget;
use svm_accountant_tools_common::accountant::Accountant;
use svm_accountant_tools_common::ids::{Address, ChainId, Digest, Sequence};
use svm_accountant_tools_common::snapshot::{
    BalanceRow, ChainRegistrationRow, ModificationKind, ModifyBalanceRow, TransferRow,
};

use solana_keypair::Keypair;

use common::mock_rpc::{MockRpc, SendTxBehavior};

#[test]
fn extracts_already_accounted_hex_code() {
    // ALREADY_ACCOUNTED = 7 → 0x7
    let msg = "RpcError(\"Transaction simulation failed: Error processing Instruction 0: custom program error: 0x7\")";
    assert_eq!(parse_custom_error_code(msg), Some(7));
    assert_eq!(
        parse_custom_error_code(msg),
        Some(ALREADY_ACCOUNTED_ERROR_CODE)
    );
}

#[test]
fn extracts_unauthorized_caller_hex_code() {
    // UNAUTHORIZED_CALLER = 33 → 0x21
    let msg =
        "Transaction simulation failed: Error processing Instruction 0: custom program error: 0x21";
    assert_eq!(parse_custom_error_code(msg), Some(33));
    assert_eq!(
        parse_custom_error_code(msg),
        Some(UNAUTHORIZED_CALLER_ERROR_CODE)
    );
}

#[test]
fn extracts_arbitrary_hex_codes() {
    assert_eq!(
        parse_custom_error_code("custom program error: 0x1"),
        Some(1)
    );
    assert_eq!(
        parse_custom_error_code("custom program error: 0x1a"),
        Some(26)
    );
    assert_eq!(
        parse_custom_error_code("custom program error: 0xff"),
        Some(255)
    );
}

#[test]
fn returns_none_for_non_program_errors() {
    assert_eq!(
        parse_custom_error_code("transport error: connection refused"),
        None
    );
    assert_eq!(parse_custom_error_code(""), None);
    assert_eq!(parse_custom_error_code("blockhash not found"), None);
}

#[test]
fn stops_at_non_hex_character() {
    // Hex parsing stops at the first non-hex char.
    let msg = "custom program error: 0x7; logs: [...]";
    assert_eq!(parse_custom_error_code(msg), Some(7));
}

#[test]
fn handles_multi_digit_codes_in_context() {
    // Real Solana RPC error wrapping with surrounding context.
    let msg = r#"RpcResponseError { code: -32002, message: "Transaction simulation failed: Error processing Instruction 0: custom program error: 0x1d", data: ... }"#;
    assert_eq!(parse_custom_error_code(msg), Some(0x1d));
}

// Orchestration tests against a hand-rolled mock JSON-RPC server
// (`tests/common/mock_rpc.rs`): drive the actual `submit_batches` async loop
// (retry/backoff, halt behavior, concurrent stats aggregation) over a real
// local HTTP endpoint.

fn authority_keypair() -> Keypair {
    // Must equal the WTT backfill authority — `BackfillTarget::try_new` checks this.
    // Same fixed keypair the surfpool e2e tests use.
    Keypair::new_from_array([1u8; 32])
}

fn wtt_target() -> BackfillTarget {
    use solana_signer::Signer;
    common::target(
        Accountant::Wtt,
        Accountant::Wtt.program_id(),
        authority_keypair().pubkey(),
    )
}

fn modification_batch() -> TxBatch {
    TxBatch::BackfillModifyBalance(vec![ModifyBalanceRow {
        sequence: Sequence(1),
        chain_id: ChainId(1),
        token_chain: ChainId(1),
        token_address: Address([0u8; 32]),
        amount: Uint256([0u8; 32]),
        reason: [0u8; 32],
        modify_kind: ModificationKind::Add,
    }])
}

fn noreplay_batch(sequence: u64) -> TxBatch {
    TxBatch::BackfillNoReplay(vec![TransferRow {
        chain: ChainId(1),
        emitter: Address({
            let mut e = [0u8; 32];
            e[31] = 0xec;
            e
        }),
        sequence: Sequence(sequence),
        digest: Digest([0xaa; 32]),
        amount: Uint256([0u8; 32]),
        token_chain: ChainId(1),
        token_address: Address({
            let mut a = [0u8; 32];
            a[31] = 0xaa;
            a
        }),
        recipient_chain: ChainId(2),
    }])
}

fn balance_batch(addr_seed: u8) -> TxBatch {
    TxBatch::BackfillBalance(vec![BalanceRow {
        chain: ChainId(1),
        token_chain: ChainId(2),
        token_address: Address({
            let mut a = [0u8; 32];
            a[31] = addr_seed;
            a
        }),
        balance: Uint256([0u8; 32]),
    }])
}

#[tokio::test]
async fn retries_on_transient_error_then_succeeds() {
    let mock = MockRpc::start(vec![
        SendTxBehavior::TransientError,
        SendTxBehavior::TransientError,
        SendTxBehavior::Success,
    ]);
    let target = wtt_target();
    let config = SubmitterConfig {
        concurrency: 1,
        max_retries: 5,
        initial_retry_delay_ms: 1,
        max_retry_delay_ms: 5,
    };

    let stats = submit_batches(
        Arc::new(mock.client()),
        target,
        authority_keypair(),
        config,
        vec![noreplay_batch(1)],
    )
    .await
    .expect("eventually succeeds after retrying transient errors");

    assert_eq!(stats.confirmed, 1);
    assert_eq!(stats.already_written, 0);
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
async fn halt_on_unrecoverable_error_stops_dispatch_of_remaining_batches() {
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
    let target = wtt_target();
    let config = SubmitterConfig {
        concurrency: 1,
        max_retries: 3,
        initial_retry_delay_ms: 1,
        max_retry_delay_ms: 5,
    };

    let batches = vec![
        noreplay_batch(1),
        balance_batch(1),
        balance_batch(2),
        balance_batch(3),
        balance_batch(4),
    ];
    let result = submit_batches(
        Arc::new(mock.client()),
        target,
        authority_keypair(),
        config,
        batches,
    )
    .await;

    assert!(
        result.is_err(),
        "an unrecoverable custom program error must kind as an Err"
    );

    // With `concurrency: 1`, the first batch fails outright (Halt, not
    // Retry), so only ONE `sendTransaction` call should have happened.
    // The sleep is a margin of safety against unrelated event-loop timing.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(
        mock.send_transaction_calls(),
        1,
        "halt must stop dispatch of every batch after the failing one — only \
         the first batch's sendTransaction call should have happened"
    );
}

#[tokio::test]
async fn concurrent_submission_aggregates_stats_across_all_tasks_not_just_the_last() {
    let mock = MockRpc::start(vec![SendTxBehavior::Success; 10]);
    let target = wtt_target();
    let config = SubmitterConfig {
        concurrency: 4,
        max_retries: 3,
        initial_retry_delay_ms: 1,
        max_retry_delay_ms: 5,
    };

    // 10 backfill batches of mixed kinds in flight concurrently (4 at a time).
    let mut batches: Vec<TxBatch> = (0..8u64).map(noreplay_batch).collect();
    batches.push(modification_batch());
    batches.push(TxBatch::BackfillChainRegistration(vec![
        ChainRegistrationRow {
            chain: ChainId(2),
            registered_emitter: Address([0u8; 32]),
            sequence: Sequence(1),
        },
    ]));

    let stats = submit_batches(
        Arc::new(mock.client()),
        target,
        authority_keypair(),
        config,
        batches,
    )
    .await
    .expect("all batches succeed");

    assert_eq!(
        stats.confirmed, 10,
        "every concurrent batch counted, not just the last"
    );
    assert_eq!(stats.already_written, 0);
    assert_eq!(mock.send_transaction_calls(), 10);
}

#[tokio::test]
async fn halt_awaits_already_in_flight_work_instead_of_abandoning_it() {
    // concurrency: 3 with 6 batches: the first 3 are all dispatched
    // concurrently before any of them can complete.
    let mock = MockRpc::start(vec![
        SendTxBehavior::Success,
        SendTxBehavior::CustomProgramError(99),
        SendTxBehavior::Success,
    ]);
    let target = wtt_target();
    let config = SubmitterConfig {
        concurrency: 3,
        max_retries: 3,
        initial_retry_delay_ms: 1,
        max_retry_delay_ms: 5,
    };

    let batches: Vec<TxBatch> = (0..6u64).map(noreplay_batch).collect();
    let result = submit_batches(
        Arc::new(mock.client()),
        target,
        authority_keypair(),
        config,
        batches,
    )
    .await;

    assert!(result.is_err(), "one unrecoverable error must kind as Err");
    assert_eq!(
        mock.send_transaction_calls(),
        3,
        "only the first in-flight batch (concurrency=3) should have been \
         dispatched; the halt flag must stop the other 3 before they start"
    );
}

#[tokio::test]
async fn progress_channel_reports_every_batch_with_its_original_index() {
    // `run`'s resume bookkeeping (main.rs) depends on `BatchProgress::index`
    // identifying which original batch a completion belongs to, since
    // concurrent completion order need not match dispatch order.
    let mock = MockRpc::start(vec![SendTxBehavior::Success; 5]);
    let target = wtt_target();
    let config = SubmitterConfig {
        concurrency: 4,
        max_retries: 3,
        initial_retry_delay_ms: 1,
        max_retry_delay_ms: 5,
    };

    let mut batches: Vec<TxBatch> = (0..4u64).map(noreplay_batch).collect();
    batches.insert(2, modification_batch());
    // batches: [noreplay0, noreplay1, modify(idx 2), noreplay2, noreplay3]

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let stats = svm_accountant_backfill::submitter::submit_batches_with_progress(
        Arc::new(mock.client()),
        target,
        authority_keypair(),
        config,
        batches,
        Some(tx),
    )
    .await
    .expect("all batches succeed");

    assert_eq!(stats.confirmed, 5);

    let mut seen: std::collections::BTreeMap<usize, String> = std::collections::BTreeMap::new();
    while let Ok(p) = rx.try_recv() {
        let kind = match p.outcome {
            svm_accountant_backfill::submitter::BatchOutcome::Confirmed(_) => "confirmed",
            svm_accountant_backfill::submitter::BatchOutcome::AlreadyWritten => "already_written",
        };
        seen.insert(p.index, kind.to_string());
    }

    assert_eq!(
        seen.len(),
        5,
        "every one of the 5 original batches must be reported exactly once"
    );
    assert_eq!(seen[&0], "confirmed");
    assert_eq!(seen[&1], "confirmed");
    assert_eq!(
        seen[&2], "confirmed",
        "the modify batch keeps its original index (2)"
    );
    assert_eq!(seen[&3], "confirmed");
    assert_eq!(seen[&4], "confirmed");
}

/// A program failure in preflight is not transient: the submitter halts after
/// one attempt instead of retrying the same failing transaction.
#[tokio::test]
async fn preflight_instruction_error_halts_without_retry() {
    let cases = [
        (
            "non-custom instruction error",
            serde_json::json!({"InstructionError": [0, "InvalidInstructionData"]}),
            "instruction error",
        ),
        (
            "unknown custom error",
            serde_json::json!({"InstructionError": [0, {"Custom": 99}]}),
            "custom program error",
        ),
        (
            "unauthorized caller",
            serde_json::json!({"InstructionError": [0, {"Custom": UNAUTHORIZED_CALLER_ERROR_CODE}]}),
            "UnauthorizedCaller",
        ),
    ];
    for (name, err, want) in cases {
        let mock = MockRpc::start(vec![SendTxBehavior::PreflightFailure(err)]);
        let config = SubmitterConfig {
            concurrency: 1,
            max_retries: 5,
            initial_retry_delay_ms: 1,
            max_retry_delay_ms: 5,
        };
        let result = submit_batches(
            Arc::new(mock.client()),
            wtt_target(),
            authority_keypair(),
            config,
            vec![modification_batch()],
        )
        .await;
        let error = format!("{:#}", result.expect_err(name));
        assert!(error.contains(want), "{name}: {error}");
        assert!(error.contains("retries: 0"), "{name}: no retry: {error}");
        assert_eq!(mock.send_transaction_calls(), 1, "{name}: one attempt");
    }
}
