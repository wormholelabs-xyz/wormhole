//! Tests for `ga-backfill stats`.

use ga_backfill::stats::{compute, TX_FEE_LAMPORTS};

fn fixture_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sample_snapshot.jsonl")
}

#[test]
fn compute_against_sample_fixture() {
    let stats = compute(&fixture_path()).expect("compute");

    // Sample snapshot: 1 account + 1 modification + 1 registration + 2 transfers
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

    // Batcher output: 1 BackfillNoReplay (2 transfers same emitter) +
    // 1 BackfillBalance + 1 BackfillModifyBalance + 1 BackfillChainRegistration.
    assert_eq!(stats.noreplay_batches, 1);
    assert_eq!(stats.balance_batches, 1);
    assert_eq!(stats.modify_balance_batches, 1);
    assert_eq!(stats.chain_registration_batches, 1);
    assert_eq!(stats.backfill_txs(), 4);
}

#[test]
fn fees_scale_with_tx_count() {
    let stats = compute(&fixture_path()).expect("compute");
    // 4 backfill txs × 5,000 L.
    assert_eq!(stats.fees_lamports(), 4 * TX_FEE_LAMPORTS);
}

#[test]
fn rent_includes_all_pda_classes() {
    let stats = compute(&fixture_path()).expect("compute");
    // 1 bucket (129 B) + 1 balance (70 B) + 1 ChainRegistration (64 B) +
    // 1 RegisterChain (48 B) + 1 ModifyBalance (112 B), each 6,960 × (128 + len).
    let rent = stats.rent_lamports();
    let expected = 1_788_720 + 1_378_080 + 1_336_320 + 1_224_960 + 1_670_400;
    assert_eq!(rent, expected);
}

#[test]
fn total_records_matches_sum() {
    let stats = compute(&fixture_path()).expect("compute");
    assert_eq!(
        stats.total_records(),
        stats.transfers + stats.accounts + stats.modifications + stats.registrations
    );
}
