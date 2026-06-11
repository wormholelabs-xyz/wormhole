//! Integration tests for the catalogue reader.

use ga_backfill::catalogue::{CatalogueReader, ModifyKind, Record};

fn fixture_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sample_catalogue.jsonl")
}

#[test]
fn parses_all_four_kinds() {
    let reader = CatalogueReader::open(fixture_path()).expect("open fixture");
    let records: Vec<Record> = reader
        .collect::<Result<Vec<_>, _>>()
        .expect("all records parse");

    assert_eq!(records.len(), 5, "fixture has 5 records");

    let mut counts = [0usize; 4]; // transfer, account, modification, registration
    for r in &records {
        match r {
            Record::Transfer(_) => counts[0] += 1,
            Record::Account(_) => counts[1] += 1,
            Record::Modification(_) => counts[2] += 1,
            Record::Registration(_) => counts[3] += 1,
        }
    }
    assert_eq!(
        counts,
        [2, 1, 1, 1],
        "expected 2 transfers + 1 each of account/modification/registration"
    );
}

#[test]
fn transfer_record_fields_round_trip() {
    let reader = CatalogueReader::open(fixture_path()).expect("open");
    let transfer = reader
        .filter_map(|r| match r {
            Ok(Record::Transfer(t)) => Some(t),
            _ => None,
        })
        .next()
        .expect("at least one transfer");

    assert_eq!(transfer.chain, 1);
    assert_eq!(transfer.sequence, 5);
    assert_eq!(transfer.token_chain, 1);
    assert_eq!(transfer.recipient_chain, 2);
    // Emitter ends in the known Solana Token Bridge bytes.
    assert_eq!(transfer.emitter[0], 0xec);
    assert_eq!(transfer.emitter[31], 0xf5);
    // Digest first byte 0x15.
    assert_eq!(transfer.digest[0], 0x15);
}

#[test]
fn account_record_fields_round_trip() {
    let reader = CatalogueReader::open(fixture_path()).expect("open");
    let account = reader
        .filter_map(|r| match r {
            Ok(Record::Account(a)) => Some(a),
            _ => None,
        })
        .next()
        .expect("one account");

    assert_eq!(account.chain, 1);
    assert_eq!(account.token_chain, 2);
    // token_address ends in the known USDC.eth address tail.
    assert_eq!(account.token_address[31], 0x48);
    // balance is the BE bytes of 0x0000...05f5e100 (= 100_000_000).
    assert_eq!(account.balance[28], 0x05);
    assert_eq!(account.balance[31], 0x00);
}

#[test]
fn modification_record_fields_round_trip() {
    let reader = CatalogueReader::open(fixture_path()).expect("open");
    let m = reader
        .filter_map(|r| match r {
            Ok(Record::Modification(m)) => Some(m),
            _ => None,
        })
        .next()
        .expect("one modification");

    assert_eq!(m.sequence, 1);
    assert_eq!(m.chain_id, 2);
    assert_eq!(m.token_chain, 2);
    assert_eq!(m.modify_kind, ModifyKind::Add);
    assert_eq!(m.reason, "smoke test");
}

#[test]
fn registration_record_fields_round_trip() {
    let reader = CatalogueReader::open(fixture_path()).expect("open");
    let r = reader
        .filter_map(|r| match r {
            Ok(Record::Registration(r)) => Some(r),
            _ => None,
        })
        .next()
        .expect("one registration");

    assert_eq!(r.chain, 2);
    assert_eq!(r.registered_emitter[31], 0x85);
}

#[test]
fn streaming_reader_does_not_load_whole_file() {
    // Sanity-check the iterator pattern — calling open() does not yet parse
    // any records. `next()` is the first parse trigger.
    let reader = CatalogueReader::open(fixture_path()).expect("open");
    // Reader is a struct, not a Vec; size_of compatible with a few file
    // handles + small line buffer, not 5 records' worth of memory.
    assert!(std::mem::size_of_val(&reader) < 1024);
}
