//! Tests for `ga-backfill index-stats`.

use ga_backfill::stats::{compute, TX_FEE_LAMPORTS};

fn fixture_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sample_catalogue.jsonl")
}

#[test]
fn compute_against_sample_fixture() {
    let stats = compute(&fixture_path()).expect("compute");

    // Sample catalogue: 1 account + 1 modification + 1 registration + 2 transfers
    // = 5 records total.
    assert_eq!(stats.transfers, 2);
    assert_eq!(stats.accounts, 1);
    assert_eq!(stats.modifications, 1);
    assert_eq!(stats.registrations, 1);
    assert_eq!(stats.total_records(), 5);

    // Both transfers from chain=1, same emitter, sequences 5 and 6 — same
    // bucket (sequence / 1024 = 0) and same emitter group.
    assert_eq!(stats.unique_emitters, 1);
    assert_eq!(stats.unique_buckets, 1);

    // Chunker output: 1 BackfillNoReplay (2 transfers same emitter) +
    // 1 BackfillBalance (1 account) + 1 DeferredModification + 1
    // DeferredRegistration = 4 chunks.
    assert_eq!(stats.noreplay_chunks, 1);
    assert_eq!(stats.balance_chunks, 1);
    assert_eq!(stats.deferred_modifications, 1);
    assert_eq!(stats.deferred_registrations, 1);
    assert_eq!(stats.backfill_txs(), 2);
    assert_eq!(stats.operational_txs(), 4); // 2 deferred × 2 txs each
}

#[test]
fn fees_scale_with_tx_count() {
    let stats = compute(&fixture_path()).expect("compute");
    // 2 backfill txs + 4 operational txs = 6 × 5,000 L = 30,000 L
    assert_eq!(stats.fees_lamports(), 6 * TX_FEE_LAMPORTS);
}

#[test]
fn rent_includes_all_pda_classes() {
    let stats = compute(&fixture_path()).expect("compute");
    // 1 bucket + 1 account + 1 registration + 1 modification — at least
    // each class contributes. Just sanity-check it's a four-summand non-zero.
    let rent = stats.rent_lamports();
    assert!(rent > 0);
    // Lower bound: roughly the sum of the four per-PDA constants.
    let lower = 1_788_720 + 1_364_160 + 1_113_600 + 1_670_400;
    assert_eq!(rent, lower);
}

#[test]
fn total_records_matches_sum() {
    let stats = compute(&fixture_path()).expect("compute");
    assert_eq!(
        stats.total_records(),
        stats.transfers + stats.accounts + stats.modifications + stats.registrations
    );
}
