//! Tests for `ga_backfill::snapshot_balance_check` — the committed-balance vs.
//! transfer-history reconciliation used by the `reconcile_balances` binary.

use ga_backfill::snapshot::{
    BalanceRow, ModificationKind, ModifyBalanceRow, SnapshotRow, TransferRow,
};
use ga_backfill::snapshot_balance_check::check_snapshot_balances;

fn amount(v: u128) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[16..].copy_from_slice(&v.to_be_bytes());
    bytes
}

fn token_address(seed: u8) -> [u8; 32] {
    let mut a = [0u8; 32];
    a[31] = seed;
    a
}

fn make_transfer(chain: u16, token_chain: u16, recipient_chain: u16, value: u128) -> TransferRow {
    TransferRow {
        chain,
        emitter: [0u8; 32],
        sequence: 1,
        digest: [0u8; 32],
        amount: amount(value),
        token_chain,
        token_address: token_address(1),
        recipient_chain,
    }
}

fn make_account(chain: u16, token_chain: u16, value: u128) -> BalanceRow {
    BalanceRow {
        chain,
        token_chain,
        token_address: token_address(1),
        balance: amount(value),
    }
}

fn make_modification(
    chain_id: u16,
    token_chain: u16,
    value: u128,
    kind: ModificationKind,
) -> ModifyBalanceRow {
    ModifyBalanceRow {
        sequence: 1,
        chain_id,
        token_chain,
        token_address: token_address(1),
        amount: amount(value),
        reason: [0u8; 32],
        modify_kind: kind,
    }
}

fn replay(
    rows: Vec<SnapshotRow>,
) -> anyhow::Result<ga_backfill::snapshot_balance_check::SnapshotBalanceReport> {
    check_snapshot_balances(rows.into_iter().map(Ok))
}

#[test]
fn native_lock_and_wrapped_mint_both_reconcile() {
    // chain == token_chain on the source (native lock, source credited);
    // recipient_chain != token_chain on the destination (wrapped mint, dest
    // credited) — a standard lock-and-mint bridge transfer.
    let rows = vec![
        SnapshotRow::Transfer(make_transfer(1, 1, 2, 1000)),
        SnapshotRow::Balance(make_account(1, 1, 1000)),
        SnapshotRow::Balance(make_account(2, 1, 1000)),
    ];
    let report = replay(rows).expect("reconcile");
    assert_eq!(report.transfers, 1);
    assert_eq!(report.accounts, 2);
    assert_eq!(report.matched, 2);
    assert!(
        report.is_clean(),
        "expected no mismatches: {:?}",
        report.mismatches
    );
}

#[test]
fn wrapped_burn_and_native_unlock_both_reconcile() {
    // chain != token_chain on the source (wrapped burn, source debited);
    // recipient_chain == token_chain on the destination (native unlock, dest
    // debited) — the reverse direction of the bridge.
    let rows = vec![
        // Fund both sides first so the debit-only records don't underflow.
        SnapshotRow::ModifyBalance(make_modification(2, 1, 1000, ModificationKind::Add)),
        SnapshotRow::ModifyBalance(make_modification(1, 1, 1000, ModificationKind::Add)),
        SnapshotRow::Transfer(make_transfer(2, 1, 1, 1000)),
        SnapshotRow::Balance(make_account(2, 1, 0)),
        SnapshotRow::Balance(make_account(1, 1, 0)),
    ];
    let report = replay(rows).expect("reconcile");
    assert_eq!(report.transfers, 1);
    assert_eq!(report.modifications, 2);
    assert_eq!(report.matched, 2);
    assert!(
        report.is_clean(),
        "expected no mismatches: {:?}",
        report.mismatches
    );
}

#[test]
fn mismatch_when_committed_balance_disagrees_with_net() {
    let rows = vec![
        SnapshotRow::Transfer(make_transfer(1, 1, 2, 1000)),
        SnapshotRow::Balance(make_account(1, 1, 500)), // wrong: net credits 1000
        SnapshotRow::Balance(make_account(2, 1, 1000)), // correct
    ];
    let report = replay(rows).expect("reconcile");
    assert_eq!(report.matched, 1);
    assert_eq!(report.mismatches.len(), 1);
    assert!(
        report.mismatches[0].starts_with("MISMATCH chain=1 "),
        "{}",
        report.mismatches[0]
    );
    assert!(!report.is_clean());
}

#[test]
fn unexpected_when_net_activity_has_no_committed_row() {
    let rows = vec![SnapshotRow::ModifyBalance(make_modification(
        1,
        1,
        1000,
        ModificationKind::Add,
    ))];
    let report = replay(rows).expect("reconcile");
    assert_eq!(report.matched, 0);
    assert_eq!(report.mismatches.len(), 1);
    assert!(
        report.mismatches[0].starts_with("UNEXPECTED"),
        "{}",
        report.mismatches[0]
    );
}

#[test]
fn modification_add_then_subtract_nets_correctly() {
    let rows = vec![
        SnapshotRow::ModifyBalance(make_modification(1, 1, 1000, ModificationKind::Add)),
        SnapshotRow::ModifyBalance(make_modification(1, 1, 400, ModificationKind::Subtract)),
        SnapshotRow::Balance(make_account(1, 1, 600)),
    ];
    let report = replay(rows).expect("reconcile");
    assert_eq!(report.modifications, 2);
    assert_eq!(report.matched, 1);
    assert!(
        report.is_clean(),
        "expected no mismatches: {:?}",
        report.mismatches
    );
}

#[test]
fn zero_net_activity_without_committed_row_is_not_flagged() {
    // Add then Subtract the same amount nets to exactly zero; a zeroed
    // account with no committed row must not be reported as unexpected.
    let rows = vec![
        SnapshotRow::ModifyBalance(make_modification(1, 1, 500, ModificationKind::Add)),
        SnapshotRow::ModifyBalance(make_modification(1, 1, 500, ModificationKind::Subtract)),
    ];
    let report = replay(rows).expect("reconcile");
    assert_eq!(report.matched, 0);
    assert!(
        report.is_clean(),
        "zero net with no committed row must not be flagged: {:?}",
        report.mismatches
    );
}

#[test]
fn credit_overflow_is_an_error() {
    let mut max_amount = [0u8; 32];
    max_amount.fill(0xff);
    let mut one = [0u8; 32];
    one[31] = 1;

    let rows = vec![
        SnapshotRow::ModifyBalance(ModifyBalanceRow {
            sequence: 1,
            chain_id: 1,
            token_chain: 1,
            token_address: token_address(1),
            amount: max_amount,
            reason: [0u8; 32],
            modify_kind: ModificationKind::Add,
        }),
        SnapshotRow::ModifyBalance(ModifyBalanceRow {
            sequence: 2,
            chain_id: 1,
            token_chain: 1,
            token_address: token_address(1),
            amount: one,
            reason: [0u8; 32],
            modify_kind: ModificationKind::Add,
        }),
    ];
    assert!(
        replay(rows).is_err(),
        "crediting past Uint256::MAX must error, not wrap"
    );
}

#[test]
fn reconciles_against_the_shared_sample_fixture() {
    use ga_backfill::snapshot::SnapshotReader;

    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sample_snapshot.jsonl");
    let reader = SnapshotReader::open(&path).expect("open fixture");
    let report = check_snapshot_balances(reader).expect("reconcile");

    // Same fixture used by tests/stats.rs and tests/snapshot.rs: 2
    // transfers, 1 modification, 1 account row.
    assert_eq!(report.transfers, 2);
    assert_eq!(report.modifications, 1);
    assert_eq!(report.accounts, 1);
}
