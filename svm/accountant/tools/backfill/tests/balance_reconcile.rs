//! Tests for `ga_backfill::balance_reconcile` — the committed-balance vs.
//! transfer-history reconciliation used by the `reconcile_balances` binary.

use ga_backfill::balance_reconcile::reconcile;
use ga_backfill::catalogue::{AccountRecord, ModificationRecord, ModifyKind, Record, TransferRecord};

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

fn make_transfer(chain: u16, token_chain: u16, recipient_chain: u16, value: u128) -> TransferRecord {
    TransferRecord {
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

fn make_account(chain: u16, token_chain: u16, value: u128) -> AccountRecord {
    AccountRecord {
        chain,
        token_chain,
        token_address: token_address(1),
        balance: amount(value),
    }
}

fn make_modification(chain_id: u16, token_chain: u16, value: u128, kind: ModifyKind) -> ModificationRecord {
    ModificationRecord {
        sequence: 1,
        chain_id,
        token_chain,
        token_address: token_address(1),
        amount: amount(value),
        reason: "test".to_string(),
        modify_kind: kind,
    }
}

fn replay(records: Vec<Record>) -> anyhow::Result<ga_backfill::balance_reconcile::ReconcileReport> {
    reconcile(records.into_iter().map(Ok))
}

#[test]
fn native_lock_and_wrapped_mint_both_reconcile() {
    // chain == token_chain on the source (native lock, source credited);
    // recipient_chain != token_chain on the destination (wrapped mint, dest
    // credited) — a standard lock-and-mint bridge transfer.
    let records = vec![
        Record::Transfer(make_transfer(1, 1, 2, 1000)),
        Record::Account(make_account(1, 1, 1000)),
        Record::Account(make_account(2, 1, 1000)),
    ];
    let report = replay(records).expect("reconcile");
    assert_eq!(report.transfers, 1);
    assert_eq!(report.accounts, 2);
    assert_eq!(report.matched, 2);
    assert!(report.is_clean(), "expected no mismatches: {:?}", report.mismatches);
}

#[test]
fn wrapped_burn_and_native_unlock_both_reconcile() {
    // chain != token_chain on the source (wrapped burn, source debited);
    // recipient_chain == token_chain on the destination (native unlock, dest
    // debited) — the reverse direction of the bridge.
    let records = vec![
        // Fund both sides first so the debit-only records don't underflow.
        Record::Modification(make_modification(2, 1, 1000, ModifyKind::Add)),
        Record::Modification(make_modification(1, 1, 1000, ModifyKind::Add)),
        Record::Transfer(make_transfer(2, 1, 1, 1000)),
        Record::Account(make_account(2, 1, 0)),
        Record::Account(make_account(1, 1, 0)),
    ];
    let report = replay(records).expect("reconcile");
    assert_eq!(report.transfers, 1);
    assert_eq!(report.modifications, 2);
    assert_eq!(report.matched, 2);
    assert!(report.is_clean(), "expected no mismatches: {:?}", report.mismatches);
}

#[test]
fn mismatch_when_committed_balance_disagrees_with_net() {
    let records = vec![
        Record::Transfer(make_transfer(1, 1, 2, 1000)),
        Record::Account(make_account(1, 1, 500)), // wrong: net credits 1000
        Record::Account(make_account(2, 1, 1000)), // correct
    ];
    let report = replay(records).expect("reconcile");
    assert_eq!(report.matched, 1);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0].starts_with("MISMATCH chain=1 "), "{}", report.mismatches[0]);
    assert!(!report.is_clean());
}

#[test]
fn unexpected_when_net_activity_has_no_committed_row() {
    let records = vec![Record::Modification(make_modification(1, 1, 1000, ModifyKind::Add))];
    let report = replay(records).expect("reconcile");
    assert_eq!(report.matched, 0);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0].starts_with("UNEXPECTED"), "{}", report.mismatches[0]);
}

#[test]
fn modification_add_then_subtract_nets_correctly() {
    let records = vec![
        Record::Modification(make_modification(1, 1, 1000, ModifyKind::Add)),
        Record::Modification(make_modification(1, 1, 400, ModifyKind::Subtract)),
        Record::Account(make_account(1, 1, 600)),
    ];
    let report = replay(records).expect("reconcile");
    assert_eq!(report.modifications, 2);
    assert_eq!(report.matched, 1);
    assert!(report.is_clean(), "expected no mismatches: {:?}", report.mismatches);
}

#[test]
fn zero_net_activity_without_committed_row_is_not_flagged() {
    // Add then Subtract the same amount nets to exactly zero; a zeroed
    // account with no committed row must not be reported as unexpected.
    let records = vec![
        Record::Modification(make_modification(1, 1, 500, ModifyKind::Add)),
        Record::Modification(make_modification(1, 1, 500, ModifyKind::Subtract)),
    ];
    let report = replay(records).expect("reconcile");
    assert_eq!(report.matched, 0);
    assert!(report.is_clean(), "zero net with no committed row must not be flagged: {:?}", report.mismatches);
}

#[test]
fn credit_overflow_is_an_error() {
    let mut max_amount = [0u8; 32];
    max_amount.fill(0xff);
    let mut one = [0u8; 32];
    one[31] = 1;

    let records = vec![
        Record::Modification(ModificationRecord {
            sequence: 1,
            chain_id: 1,
            token_chain: 1,
            token_address: token_address(1),
            amount: max_amount,
            reason: "test".to_string(),
            modify_kind: ModifyKind::Add,
        }),
        Record::Modification(ModificationRecord {
            sequence: 2,
            chain_id: 1,
            token_chain: 1,
            token_address: token_address(1),
            amount: one,
            reason: "test".to_string(),
            modify_kind: ModifyKind::Add,
        }),
    ];
    assert!(replay(records).is_err(), "crediting past Uint256::MAX must error, not wrap");
}

#[test]
fn reconciles_against_the_shared_sample_fixture() {
    use ga_backfill::catalogue::CatalogueReader;

    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sample_catalogue.jsonl");
    let reader = CatalogueReader::open(&path).expect("open fixture");
    let report = reconcile(reader).expect("reconcile");

    // Same fixture used by tests/stats.rs and tests/catalogue.rs: 2
    // transfers, 1 modification, 1 account row.
    assert_eq!(report.transfers, 2);
    assert_eq!(report.modifications, 1);
    assert_eq!(report.accounts, 1);
}
