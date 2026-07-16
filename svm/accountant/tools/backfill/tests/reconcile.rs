//! Unit tests for the reconciliation comparison logic.
//!
//! The fetch path (gPA) is integration-tested via Phase 9's surfpool e2e.

use std::collections::HashMap;

use ga_backfill::reconcile::{
    compare_balances, compare_maps, load_expected_balances, load_expected_relayer_registrations,
    load_expected_transceiver_hubs, load_expected_transceiver_peers, BalanceKey, BalanceValue,
};

fn key(chain: u16, token_chain: u16, addr_seed: u8) -> BalanceKey {
    let mut addr = [0u8; 32];
    addr[31] = addr_seed;
    (chain, token_chain, addr)
}

fn value(low: u8) -> BalanceValue {
    let mut v = [0u8; 32];
    v[31] = low;
    v
}

#[test]
fn empty_inputs_report_no_diffs() {
    let report = compare_balances(&HashMap::new(), &HashMap::new());
    assert!(report.is_clean());
    assert_eq!(report.matched, 0);
}

#[test]
fn all_match() {
    let mut both = HashMap::new();
    both.insert(key(1, 2, 0xAA), value(10));
    both.insert(key(1, 2, 0xBB), value(20));
    let report = compare_balances(&both, &both);
    assert!(report.is_clean());
    assert_eq!(report.matched, 2);
}

#[test]
fn missing_from_chain_flagged() {
    let mut expected = HashMap::new();
    expected.insert(key(1, 2, 0xAA), value(10));
    expected.insert(key(1, 2, 0xBB), value(20));
    let mut actual = HashMap::new();
    actual.insert(key(1, 2, 0xAA), value(10));

    let report = compare_balances(&expected, &actual);
    assert!(!report.is_clean());
    assert_eq!(report.matched, 1);
    assert_eq!(report.missing_from_chain, vec![key(1, 2, 0xBB)]);
    assert!(report.mismatched.is_empty());
    assert!(report.unexpected_on_chain.is_empty());
}

#[test]
fn unexpected_on_chain_flagged() {
    let mut expected = HashMap::new();
    expected.insert(key(1, 2, 0xAA), value(10));
    let mut actual = expected.clone();
    actual.insert(key(1, 2, 0xCC), value(99));

    let report = compare_balances(&expected, &actual);
    assert!(!report.is_clean());
    assert_eq!(report.matched, 1);
    assert_eq!(report.unexpected_on_chain, vec![key(1, 2, 0xCC)]);
}

#[test]
fn value_mismatch_flagged() {
    let mut expected = HashMap::new();
    expected.insert(key(1, 2, 0xAA), value(10));
    let mut actual = HashMap::new();
    actual.insert(key(1, 2, 0xAA), value(99));

    let report = compare_balances(&expected, &actual);
    assert!(!report.is_clean());
    assert_eq!(report.mismatched.len(), 1);
    let m = &report.mismatched[0];
    assert_eq!(m.key, key(1, 2, 0xAA));
    assert_eq!(m.catalogue_balance, value(10));
    assert_eq!(m.on_chain_balance, value(99));
}

#[test]
fn mixed_diff_types() {
    let mut expected = HashMap::new();
    expected.insert(key(1, 2, 0xAA), value(10)); // matched
    expected.insert(key(1, 2, 0xBB), value(20)); // mismatched
    expected.insert(key(1, 2, 0xCC), value(30)); // missing from chain
    let mut actual = HashMap::new();
    actual.insert(key(1, 2, 0xAA), value(10));
    actual.insert(key(1, 2, 0xBB), value(99));
    actual.insert(key(1, 2, 0xDD), value(40)); // unexpected on chain

    let report = compare_balances(&expected, &actual);
    assert_eq!(report.matched, 1);
    assert_eq!(report.mismatched.len(), 1);
    assert_eq!(report.missing_from_chain.len(), 1);
    assert_eq!(report.unexpected_on_chain.len(), 1);
    assert_eq!(report.total_diffs(), 3);
}

#[test]
fn load_expected_from_sample_fixture() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sample_catalogue.jsonl");
    let expected = load_expected_balances(&path).expect("load");
    // Sample fixture has one account record.
    assert_eq!(expected.len(), 1);
    // chain=1, token_chain=2, balance low byte = 0x00 (= 100_000_000 in BE)
    let key = (1u16, 2u16, {
        let mut a = [0u8; 32];
        // token_address: 0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48
        a[12] = 0xa0;
        a[13] = 0xb8;
        a[14] = 0x69;
        a[15] = 0x91;
        a[16] = 0xc6;
        a[17] = 0x21;
        a[18] = 0x8b;
        a[19] = 0x36;
        a[20] = 0xc1;
        a[21] = 0xd1;
        a[22] = 0x9d;
        a[23] = 0x4a;
        a[24] = 0x2e;
        a[25] = 0x9e;
        a[26] = 0xb0;
        a[27] = 0xce;
        a[28] = 0x36;
        a[29] = 0x06;
        a[30] = 0xeb;
        a[31] = 0x48;
        a
    });
    assert!(expected.contains_key(&key));
    // Balance bytes: 0x...05f5e100 (100_000_000)
    let bal = expected[&key];
    assert_eq!(bal[28], 0x05);
    assert_eq!(bal[31], 0x00);
}

// ============================================================================
// NTT-native maps
// ============================================================================

fn ntt_fixture() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sample_ntt_catalogue.jsonl")
}

#[test]
fn compare_maps_generic_verdict() {
    let mut expected = HashMap::new();
    expected.insert((2u16, [0u8; 32]), (1u16, [9u8; 32])); // matched
    expected.insert((3u16, [0u8; 32]), (1u16, [9u8; 32])); // mismatched
    expected.insert((4u16, [0u8; 32]), (1u16, [9u8; 32])); // missing from chain
    let mut actual = HashMap::new();
    actual.insert((2u16, [0u8; 32]), (1u16, [9u8; 32]));
    actual.insert((3u16, [0u8; 32]), (2u16, [9u8; 32])); // diff value
    actual.insert((5u16, [0u8; 32]), (1u16, [9u8; 32])); // unexpected on chain

    let v = compare_maps(&expected, &actual);
    assert_eq!(v.matched, 1);
    assert_eq!(v.mismatched, 1);
    assert_eq!(v.missing_from_chain, 1);
    assert_eq!(v.unexpected_on_chain, 1);
    assert!(!v.is_clean());
    assert_eq!(v.total_diffs(), 3);
}

#[test]
fn loads_ntt_maps_from_fixture() {
    let path = ntt_fixture();
    let relayers = load_expected_relayer_registrations(&path).expect("relayers");
    assert_eq!(relayers.len(), 2);
    assert_eq!(relayers[&2u16][31], 0x85);

    let hubs = load_expected_transceiver_hubs(&path).expect("hubs");
    assert_eq!(hubs.len(), 2);
    let (hub_chain, hub_address) = hubs[&(2u16, {
        let mut a = [0u8; 32];
        a[31] = 0x22;
        a
    })];
    assert_eq!(hub_chain, 1);
    assert_eq!(hub_address[31], 0x33);

    let peers = load_expected_transceiver_peers(&path).expect("peers");
    assert_eq!(peers.len(), 1);
    let peer_address = peers[&(
        2u16,
        {
            let mut a = [0u8; 32];
            a[31] = 0x66;
            a
        },
        4u16,
    )];
    assert_eq!(peer_address[31], 0x77);
}
