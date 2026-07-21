//! Unit tests for the reconciliation comparison logic.
//!
//! The on-chain fetch path (gPA) against a *real validator* is
//! integration-tested via Phase 9's surfpool e2e. The NTT decode-closure
//! tests further down exercise the same fetch path's byte-offset parsing
//! against a mock RPC instead, since surfpool e2e coverage is `#[ignore]`d
//! and heavy — these give fast, always-on coverage of the parsing itself.

mod common;

use std::collections::HashMap;

use ga_backfill::reconcile::{
    compare_balances, compare_maps, fetch_on_chain_relayer_registrations,
    fetch_on_chain_transceiver_hubs, fetch_on_chain_transceiver_peers, load_expected_balances,
    load_expected_relayer_registrations, load_expected_transceiver_hubs,
    load_expected_transceiver_peers, BalanceKey, BalanceValue,
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

// Loads a real file into `load_expected_balances`, then compares it against
// a deliberately-wrong "actual" map — proving the on-disk (chain,
// token_chain, token_address) key doesn't get mis-assembled or collided
// when it flows through to a comparison.

#[test]
fn load_expected_balances_from_real_file_does_not_miskey_or_collide_similar_records() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("catalogue.jsonl");
    // Three rows sharing components in a way that WOULD collide under a
    // mis-keyed loader: rows 1 and 2 share the same token_address with
    // (chain, token_chain) swapped; row 3 shares (chain, token_chain) with
    // row 1 but a different token_address.
    let jsonl = concat!(
        "{\"kind\":\"account\",\"chain\":1,\"token_chain\":2,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"balance\":\"0x0000000000000000000000000000000000000000000000000000000000000064\"}\n",
        "{\"kind\":\"account\",\"chain\":2,\"token_chain\":1,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000aa\",\"balance\":\"0x00000000000000000000000000000000000000000000000000000000000000c8\"}\n",
        "{\"kind\":\"account\",\"chain\":1,\"token_chain\":2,\"token_address\":\"0x00000000000000000000000000000000000000000000000000000000000000bb\",\"balance\":\"0x00000000000000000000000000000000000000000000000000000000000001f4\"}\n",
    );
    std::fs::write(&path, jsonl).expect("write catalogue");

    let expected = load_expected_balances(&path).expect("load");
    assert_eq!(
        expected.len(),
        3,
        "3 distinct (chain, token_chain, token_address) keys must not collide \
         into fewer entries"
    );

    let addr = |seed: u8| {
        let mut a = [0u8; 32];
        a[31] = seed;
        a
    };
    assert_eq!(expected[&(1u16, 2u16, addr(0xaa))][31], 0x64);
    assert_eq!(expected[&(2u16, 1u16, addr(0xaa))][31], 0xc8);
    assert_eq!(expected[&(1u16, 2u16, addr(0xbb))][31], 0xf4);

    // Deliberately-wrong "actual" (on-chain) map: correct keys but one
    // wrong value, and one key entirely missing.
    let mut actual = HashMap::new();
    actual.insert((1u16, 2u16, addr(0xaa)), value(0x64)); // matches
    actual.insert((2u16, 1u16, addr(0xaa)), value(0xFF)); // wrong value
    // (1, 2, 0xbb) missing entirely from "on-chain" state.

    let report = compare_balances(&expected, &actual);
    assert_eq!(report.matched, 1);
    assert_eq!(report.mismatched.len(), 1);
    assert_eq!(report.mismatched[0].key, (2u16, 1u16, addr(0xaa)));
    assert_eq!(report.missing_from_chain, vec![(1u16, 2u16, addr(0xbb))]);
    assert!(report.unexpected_on_chain.is_empty());
}

// NTT decode-closure byte-offset parsing, via a mock `getProgramAccounts`.
// `fetch_on_chain_relayer_registrations` / `_transceiver_hubs` / `_peers`
// each embed their decode logic as an inline closure with no standalone
// testable function, so these drive the real public fetch functions
// against `tests/common/mock_rpc.rs` rather than reimplementing the
// byte-offset parsing a second time in the test.

fn le16(v: u16) -> [u8; 2] {
    v.to_le_bytes()
}

#[tokio::test]
async fn fetch_relayer_registrations_decodes_real_byte_layout() {
    // Layout (64B): tag@0, _pad0@1, chain@2 (LE u16), _padding@4 (28B),
    // emitter_address@32 (32B).
    let mut data = vec![0u8; 64];
    data[0] = 5; // AccountTag::RelayerChainRegistration
    data[2..4].copy_from_slice(&le16(42));
    data[32..64].fill(0x9); // recognisable emitter pattern
    data[63] = 0x77; // distinguishable tail byte

    let program_id = solana_pubkey::Pubkey::new_unique();
    let pda = solana_pubkey::Pubkey::new_unique();
    let mock = common::mock_rpc::MockRpc::start_with_program_accounts(vec![
        common::mock_rpc::keyed_account_json(&pda, &program_id, &data),
    ]);
    let rpc = solana_client::nonblocking::rpc_client::RpcClient::new(mock.url());

    let map = fetch_on_chain_relayer_registrations(&rpc, &program_id)
        .await
        .expect("fetch");
    assert_eq!(map.len(), 1);
    let emitter = map.get(&42u16).expect("chain 42 present");
    assert_eq!(emitter[0], 0x9);
    assert_eq!(emitter[31], 0x77);
}

#[tokio::test]
async fn fetch_transceiver_hubs_decodes_real_byte_layout() {
    // Layout (70B): tag@0, _pad0@1, chain@2 (LE u16), hub_chain@4 (LE u16),
    // address@6 (32B), hub_address@38 (32B).
    let mut data = vec![0u8; 70];
    data[0] = 6; // AccountTag::TransceiverHub
    data[2..4].copy_from_slice(&le16(11));
    data[4..6].copy_from_slice(&le16(22));
    data[6..38].fill(0xAB);
    data[6] = 0x01; // distinguish first byte of address from fill pattern
    data[38..70].fill(0xCD);
    data[69] = 0x02;

    let program_id = solana_pubkey::Pubkey::new_unique();
    let pda = solana_pubkey::Pubkey::new_unique();
    let mock = common::mock_rpc::MockRpc::start_with_program_accounts(vec![
        common::mock_rpc::keyed_account_json(&pda, &program_id, &data),
    ]);
    let rpc = solana_client::nonblocking::rpc_client::RpcClient::new(mock.url());

    let map = fetch_on_chain_transceiver_hubs(&rpc, &program_id)
        .await
        .expect("fetch");
    assert_eq!(map.len(), 1);
    let mut expected_address = [0xABu8; 32];
    expected_address[0] = 0x01;
    let (hub_chain, hub_address) = map
        .get(&(11u16, expected_address))
        .expect("(chain=11, address) present");
    assert_eq!(*hub_chain, 22);
    assert_eq!(hub_address[0], 0xCD);
    assert_eq!(hub_address[31], 0x02);
}

#[tokio::test]
async fn fetch_transceiver_peers_decodes_real_byte_layout() {
    // Layout (70B): tag@0, _pad0@1, chain@2 (LE u16), dest_chain@4 (LE u16),
    // address@6 (32B), peer_address@38 (32B).
    let mut data = vec![0u8; 70];
    data[0] = 7; // AccountTag::TransceiverPeer
    data[2..4].copy_from_slice(&le16(3));
    data[4..6].copy_from_slice(&le16(4));
    data[6..38].fill(0xEE);
    data[38..70].fill(0xFA);
    data[69] = 0x55;

    let program_id = solana_pubkey::Pubkey::new_unique();
    let pda = solana_pubkey::Pubkey::new_unique();
    let mock = common::mock_rpc::MockRpc::start_with_program_accounts(vec![
        common::mock_rpc::keyed_account_json(&pda, &program_id, &data),
    ]);
    let rpc = solana_client::nonblocking::rpc_client::RpcClient::new(mock.url());

    let map = fetch_on_chain_transceiver_peers(&rpc, &program_id)
        .await
        .expect("fetch");
    assert_eq!(map.len(), 1);
    let peer_address = map
        .get(&(3u16, [0xEEu8; 32], 4u16))
        .expect("(chain=3, address, dest_chain=4) present");
    assert_eq!(peer_address[0], 0xFA);
    assert_eq!(peer_address[31], 0x55);
}
