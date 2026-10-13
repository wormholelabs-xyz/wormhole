//! Tests for `svm_accountant_backfill::snapshot_balance_check`: the committed balances vs.
//! the net transfer history, used by `svm-accountant-backfill check-snapshot-balances`.

use global_accountant_definitions::Uint256;
use svm_accountant_backfill::snapshot_balance_check::check_snapshot_balances;
use svm_accountant_tools_common::ids::{Address, ChainId, Digest, Sequence};
use svm_accountant_tools_common::snapshot::{
    BalanceRow, ModificationKind, ModifyBalanceRow, SnapshotRow, TransferRow,
};
use svm_accountant_tools_common::test_support::fixture_path;

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
        chain: ChainId(chain),
        emitter: Address([0u8; 32]),
        sequence: Sequence(1),
        digest: Digest([0u8; 32]),
        amount: Uint256(amount(value)),
        token_chain: ChainId(token_chain),
        token_address: Address(token_address(1)),
        recipient_chain: ChainId(recipient_chain),
    }
}

fn make_balance(chain: u16, token_chain: u16, value: u128) -> BalanceRow {
    BalanceRow {
        chain: ChainId(chain),
        token_chain: ChainId(token_chain),
        token_address: Address(token_address(1)),
        balance: Uint256(amount(value)),
    }
}

fn make_modification(
    chain_id: u16,
    token_chain: u16,
    value: u128,
    kind: ModificationKind,
) -> ModifyBalanceRow {
    ModifyBalanceRow {
        sequence: Sequence(1),
        chain_id: ChainId(chain_id),
        token_chain: ChainId(token_chain),
        token_address: Address(token_address(1)),
        amount: Uint256(amount(value)),
        reason: [0u8; 32],
        modify_kind: kind,
    }
}

fn check(
    rows: Vec<SnapshotRow>,
) -> anyhow::Result<svm_accountant_backfill::snapshot_balance_check::SnapshotBalanceReport> {
    check_snapshot_balances(rows.into_iter().map(Ok))
}

#[test]
fn native_lock_and_wrapped_mint_both_net_to_balance() {
    // chain == token_chain on the source (native lock, source credited);
    // recipient_chain != token_chain on the destination (wrapped mint, dest
    // credited) — a standard lock-and-mint bridge transfer.
    let rows = vec![
        SnapshotRow::Transfer(make_transfer(1, 1, 2, 1000)),
        SnapshotRow::Balance(make_balance(1, 1, 1000)),
        SnapshotRow::Balance(make_balance(2, 1, 1000)),
    ];
    let report = check(rows).expect("check");
    assert_eq!(report.transfers, 1);
    assert_eq!(report.balances, 2);
    assert_eq!(report.matched, 2);
    assert!(
        report.is_clean(),
        "expected no mismatches: {:?}",
        report.discrepancies
    );
}

#[test]
fn wrapped_burn_and_native_unlock_both_net_to_balance() {
    // chain != token_chain on the source (wrapped burn, source debited);
    // recipient_chain == token_chain on the destination (native unlock, dest
    // debited) — the reverse direction of the bridge.
    let rows = vec![
        // Fund both sides first so the debit-only records don't underflow.
        SnapshotRow::ModifyBalance(make_modification(2, 1, 1000, ModificationKind::Add)),
        SnapshotRow::ModifyBalance(make_modification(1, 1, 1000, ModificationKind::Add)),
        SnapshotRow::Transfer(make_transfer(2, 1, 1, 1000)),
        SnapshotRow::Balance(make_balance(2, 1, 0)),
        SnapshotRow::Balance(make_balance(1, 1, 0)),
    ];
    let report = check(rows).expect("check");
    assert_eq!(report.transfers, 1);
    assert_eq!(report.modifications, 2);
    assert_eq!(report.matched, 2);
    assert!(
        report.is_clean(),
        "expected no mismatches: {:?}",
        report.discrepancies
    );
}

#[test]
fn mismatch_when_committed_balance_disagrees_with_net() {
    let rows = vec![
        SnapshotRow::Transfer(make_transfer(1, 1, 2, 1000)),
        SnapshotRow::Balance(make_balance(1, 1, 500)), // wrong: net credits 1000
        SnapshotRow::Balance(make_balance(2, 1, 1000)), // correct
    ];
    let report = check(rows).expect("check");
    assert_eq!(report.matched, 1);
    assert_eq!(report.discrepancies.len(), 1);
    assert!(
        report.discrepancies[0].starts_with("MISMATCH chain=1 "),
        "{}",
        report.discrepancies[0]
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
    let report = check(rows).expect("check");
    assert_eq!(report.matched, 0);
    assert_eq!(report.discrepancies.len(), 1);
    assert!(
        report.discrepancies[0].starts_with("UNEXPECTED"),
        "{}",
        report.discrepancies[0]
    );
}

#[test]
fn modification_add_then_subtract_nets_correctly() {
    let rows = vec![
        SnapshotRow::ModifyBalance(make_modification(1, 1, 1000, ModificationKind::Add)),
        SnapshotRow::ModifyBalance(make_modification(1, 1, 400, ModificationKind::Subtract)),
        SnapshotRow::Balance(make_balance(1, 1, 600)),
    ];
    let report = check(rows).expect("check");
    assert_eq!(report.modifications, 2);
    assert_eq!(report.matched, 1);
    assert!(
        report.is_clean(),
        "expected no mismatches: {:?}",
        report.discrepancies
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
    let report = check(rows).expect("check");
    assert_eq!(report.matched, 0);
    assert!(
        report.is_clean(),
        "zero net with no committed row must not be flagged: {:?}",
        report.discrepancies
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
            sequence: Sequence(1),
            chain_id: ChainId(1),
            token_chain: ChainId(1),
            token_address: Address(token_address(1)),
            amount: Uint256(max_amount),
            reason: [0u8; 32],
            modify_kind: ModificationKind::Add,
        }),
        SnapshotRow::ModifyBalance(ModifyBalanceRow {
            sequence: Sequence(2),
            chain_id: ChainId(1),
            token_chain: ChainId(1),
            token_address: Address(token_address(1)),
            amount: Uint256(one),
            reason: [0u8; 32],
            modify_kind: ModificationKind::Add,
        }),
    ];
    assert!(
        check(rows).is_err(),
        "crediting past Uint256::MAX must error, not wrap"
    );
}

#[test]
fn sample_fixture_nets_to_committed_balances() {
    use svm_accountant_tools_common::snapshot::SnapshotReader;

    let path = fixture_path("wtt_sample_snapshot.jsonl");
    let reader = SnapshotReader::open(&path).expect("open fixture");
    let report = check_snapshot_balances(reader).expect("check");

    // Same fixture used by tests/stats.rs and tests/snapshot.rs: 2
    // transfers, 1 modification, 1 balance row.
    assert_eq!(report.transfers, 2);
    assert_eq!(report.modifications, 1);
    assert_eq!(report.balances, 1);
}
