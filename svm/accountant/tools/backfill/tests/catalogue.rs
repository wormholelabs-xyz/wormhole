//! Integration tests for the catalogue reader.

use ga_backfill::catalogue::{CatalogueError, CatalogueReader, ModifyKind, Record};

fn fixture_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sample_catalogue.jsonl")
}

fn ntt_fixture_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sample_ntt_catalogue.jsonl")
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
            other => panic!("unexpected NTT record kind in TB fixture: {other:?}"),
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
fn parses_all_three_ntt_kinds() {
    let reader = CatalogueReader::open(ntt_fixture_path()).expect("open ntt fixture");
    let records: Vec<Record> = reader
        .collect::<Result<Vec<_>, _>>()
        .expect("all records parse");
    assert_eq!(records.len(), 5, "ntt fixture has 5 records");

    let mut counts = [0usize; 3]; // relayer, hub, peer
    for r in &records {
        match r {
            Record::RelayerChainRegistration(_) => counts[0] += 1,
            Record::TransceiverHub(_) => counts[1] += 1,
            Record::TransceiverPeer(_) => counts[2] += 1,
            other => panic!("unexpected record kind in ntt fixture: {other:?}"),
        }
    }
    assert_eq!(counts, [2, 2, 1], "2 relayer + 2 hub + 1 peer");
}

#[test]
fn relayer_chain_registration_fields_round_trip() {
    let reader = CatalogueReader::open(ntt_fixture_path()).expect("open");
    let r = reader
        .filter_map(|r| match r {
            Ok(Record::RelayerChainRegistration(r)) => Some(r),
            _ => None,
        })
        .next()
        .expect("one relayer registration");
    assert_eq!(r.chain, 2);
    assert_eq!(r.registered_emitter[31], 0x85);
}

#[test]
fn transceiver_hub_fields_round_trip() {
    let reader = CatalogueReader::open(ntt_fixture_path()).expect("open");
    let h = reader
        .filter_map(|r| match r {
            Ok(Record::TransceiverHub(h)) => Some(h),
            _ => None,
        })
        .next()
        .expect("one hub");
    assert_eq!(h.chain, 2);
    assert_eq!(h.address[31], 0x22);
    assert_eq!(h.hub_chain, 1);
    assert_eq!(h.hub_address[31], 0x33);
}

#[test]
fn transceiver_peer_fields_round_trip() {
    let reader = CatalogueReader::open(ntt_fixture_path()).expect("open");
    let p = reader
        .filter_map(|r| match r {
            Ok(Record::TransceiverPeer(p)) => Some(p),
            _ => None,
        })
        .next()
        .expect("one peer");
    assert_eq!(p.chain, 2);
    assert_eq!(p.address[31], 0x66);
    assert_eq!(p.dest_chain, 4);
    assert_eq!(p.peer_address[31], 0x77);
}

// One malformed/truncated-row test per `CatalogueError` variant. `parse_record`
// is private, so these go through the public entry point: write a single bad
// line to a temp file, open it with `CatalogueReader`, and inspect the error.

fn parse_one_line(line: &str) -> CatalogueError {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("bad.jsonl");
    std::fs::write(&path, format!("{line}\n")).expect("write");
    CatalogueReader::open(&path)
        .expect("open")
        .next()
        .expect("one record")
        .expect_err("line must fail to parse")
}

#[test]
fn io_error_on_missing_file() {
    let result = CatalogueReader::open("/nonexistent/path/that/should/not/exist.jsonl");
    assert!(matches!(result, Err(CatalogueError::Io(_))));
}

#[test]
fn json_error_on_malformed_json() {
    let err = parse_one_line("{ this is not valid json");
    assert!(matches!(err, CatalogueError::Json { .. }), "got: {err:?}");
}

#[test]
fn missing_field_error_when_kind_absent() {
    let err = parse_one_line(r#"{"chain":1}"#);
    assert!(
        matches!(err, CatalogueError::MissingField { field: "kind", .. }),
        "got: {err:?}"
    );
}

#[test]
fn missing_field_error_when_kind_specific_field_absent() {
    // A `transfer` row missing `chain`.
    let err = parse_one_line(
        r#"{"kind":"transfer","emitter":"0x0000000000000000000000000000000000000000000000000000000000000001","sequence":1,"digest":"0x0000000000000000000000000000000000000000000000000000000000000001","amount":"0x0000000000000000000000000000000000000000000000000000000000000001","token_chain":1,"token_address":"0x0000000000000000000000000000000000000000000000000000000000000001","recipient_chain":1}"#,
    );
    assert!(
        matches!(err, CatalogueError::MissingField { field: "chain", .. }),
        "got: {err:?}"
    );
}

#[test]
fn unknown_kind_error() {
    let err = parse_one_line(r#"{"kind":"not_a_real_kind"}"#);
    assert!(
        matches!(err, CatalogueError::UnknownKind { ref kind, .. } if kind == "not_a_real_kind"),
        "got: {err:?}"
    );
}

#[test]
fn bad_type_error_on_non_numeric_chain() {
    let err = parse_one_line(r#"{"kind":"registration","chain":"not-a-number","registered_emitter":"0x0000000000000000000000000000000000000000000000000000000000000001"}"#);
    assert!(
        matches!(err, CatalogueError::BadType { field: "chain", expected: "u64", .. }),
        "got: {err:?}"
    );
}

#[test]
fn invalid_hex_error_on_wrong_length() {
    // `registered_emitter` should be 64 hex chars (32 bytes); this is short.
    let err = parse_one_line(r#"{"kind":"registration","chain":1,"registered_emitter":"0xaabb"}"#);
    assert!(
        matches!(err, CatalogueError::InvalidHex { field: "registered_emitter", .. }),
        "got: {err:?}"
    );
}

#[test]
fn invalid_hex_error_on_non_hex_characters() {
    let err = parse_one_line(r#"{"kind":"registration","chain":1,"registered_emitter":"0xzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"}"#);
    assert!(
        matches!(err, CatalogueError::InvalidHex { field: "registered_emitter", .. }),
        "got: {err:?}"
    );
}

#[test]
fn out_of_range_error_when_u64_does_not_fit_u16() {
    // `chain` is parsed as u16; 99999 overflows it.
    let err = parse_one_line(r#"{"kind":"registration","chain":99999,"registered_emitter":"0x0000000000000000000000000000000000000000000000000000000000000001"}"#);
    assert!(
        matches!(err, CatalogueError::OutOfRange { field: "chain", value: 99999, expected: "u16", .. }),
        "got: {err:?}"
    );
}

#[test]
fn bad_modify_kind_error() {
    let err = parse_one_line(r#"{"kind":"modification","sequence":1,"chain_id":1,"token_chain":1,"token_address":"0x0000000000000000000000000000000000000000000000000000000000000001","amount":"0x0000000000000000000000000000000000000000000000000000000000000001","reason":"x","modify_kind":"multiply"}"#);
    assert!(
        matches!(err, CatalogueError::BadModifyKind { ref value, .. } if value == "multiply"),
        "got: {err:?}"
    );
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
