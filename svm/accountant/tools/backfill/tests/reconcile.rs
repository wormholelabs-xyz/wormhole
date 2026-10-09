//! Unit tests for the reconciliation comparison logic.
//!
//! The on-chain fetch path (gPA) against a *real validator* is covered by the
//! surfpool e2e suites. The layout fetch tests further down drive the same
//! path against a mock RPC for fast, always-on coverage of the parsing.

mod common;

use std::collections::HashMap;

use ga_backfill::expected_accounts::derive_pda;
use ga_backfill::expected_accounts::{
    chain_registration_account, transceiver_hub_account, transceiver_peer_account,
};
use ga_backfill::reconcile::{
    compare_balances, compare_maps, fetch_layouts, load_expected_balances, load_expected_layouts,
    BalanceKey, BalanceValue,
};
use ga_backfill::snapshot::SnapshotRow;
use global_accountant_definitions::{
    BelongsToHub, ChainRegistrationKey, ChainRegistrationLayout, TransceiverHubKey,
    TransceiverHubLayout, TransceiverPeerKey, TransceiverPeerLayout,
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
    assert_eq!(m.snapshot_balance, value(10));
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
        .join("tests/fixtures/sample_snapshot.jsonl");
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
        .join("tests/fixtures/sample_ntt_snapshot.jsonl")
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
    let program_id = solana_pubkey::Pubkey::new_unique();
    let addr = |seed: u8| {
        let mut a = [0u8; 32];
        a[31] = seed;
        a
    };

    let registrations = load_expected_layouts(&path, &program_id, |r| match r {
        SnapshotRow::RelayerChainRegistration(x) => vec![chain_registration_account(x)],
        _ => vec![],
    })
    .expect("registrations");
    assert_eq!(registrations.len(), 2);
    let chain_2 = &registrations[&derive_pda(&program_id, &ChainRegistrationKey::new(2))];
    assert_eq!(chain_2.emitter_address[31], 0x85);
    assert_eq!(chain_2.governance_sequence(), 3_710_971_193_224_645_800);

    let hubs = load_expected_layouts(&path, &program_id, |r| match r {
        SnapshotRow::TransceiverHub(h) => vec![transceiver_hub_account(h)],
        _ => vec![],
    })
    .expect("hubs");
    assert_eq!(hubs.len(), 2);
    let hub = &hubs[&derive_pda(&program_id, &TransceiverHubKey::new(2, addr(0x22)))];
    assert_eq!(hub.hub_chain, 1);
    assert_eq!(hub.hub_address[31], 0x33);

    let peers = load_expected_layouts(&path, &program_id, |r| match r {
        SnapshotRow::TransceiverPeer(p) => vec![transceiver_peer_account(p)],
        _ => vec![],
    })
    .expect("peers");
    assert_eq!(peers.len(), 1);
    let peer = &peers[&derive_pda(&program_id, &TransceiverPeerKey::new(2, addr(0x66), 4))];
    assert_eq!(peer.peer_address[31], 0x77);
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
    std::fs::write(&path, jsonl).expect("write snapshot");

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

// Layout fetch via a mock `getProgramAccounts`. Each account sits at the PDA
// its layout key derives, as `fetch_layouts` requires.

async fn fetch_one<L>(
    layout: L,
    key: &impl global_accountant_definitions::PdaSeeds,
) -> HashMap<solana_pubkey::Pubkey, L>
where
    L: global_accountant_definitions::AccountLayout + ga_backfill::expected_accounts::PdaAccount,
{
    let program_id = solana_pubkey::Pubkey::new_unique();
    let pda = derive_pda(&program_id, key);
    let mock = common::mock_rpc::MockRpc::start_with_program_accounts(vec![
        common::mock_rpc::keyed_account_json(&pda, &program_id, bytemuck::bytes_of(&layout)),
    ]);
    let rpc = solana_client::nonblocking::rpc_client::RpcClient::new(mock.url());
    let map = fetch_layouts::<L>(&rpc, &program_id).await.expect("fetch");
    assert_eq!(map.len(), 1);
    assert!(map.contains_key(&pda), "keyed by the canonical PDA");
    map
}

#[tokio::test]
async fn fetch_chain_registrations_decodes_real_byte_layout() {
    let mut emitter = [0x9u8; 32];
    emitter[31] = 0x77;
    let layout = ChainRegistrationLayout::new(42, emitter, 7);
    let map = fetch_one(layout, &ChainRegistrationKey::new(42)).await;
    let got = map.values().next().expect("one entry");
    assert_eq!(got.chain, 42);
    assert_eq!(got.emitter_address, emitter);
    assert_eq!(got.governance_sequence(), 7);
}

#[tokio::test]
async fn fetch_transceiver_hubs_decodes_real_byte_layout() {
    let mut address = [0xABu8; 32];
    address[0] = 0x01;
    let mut hub_address = [0xCDu8; 32];
    hub_address[31] = 0x02;
    let key = TransceiverHubKey::new(11, address);
    let layout =
        TransceiverHubLayout::new(key, BelongsToHub(TransceiverHubKey::new(22, hub_address)));
    let map = fetch_one(layout, &key).await;
    let got = map.values().next().expect("one entry");
    assert_eq!(got.chain, 11);
    assert_eq!(got.address, address);
    assert_eq!(got.hub_chain, 22);
    assert_eq!(got.hub_address, hub_address);
}

#[tokio::test]
async fn fetch_transceiver_peers_decodes_real_byte_layout() {
    let mut peer_address = [0xFAu8; 32];
    peer_address[31] = 0x55;
    let key = TransceiverPeerKey::new(3, [0xEEu8; 32], 4);
    let layout = TransceiverPeerLayout::new(key, peer_address);
    let map = fetch_one(layout, &key).await;
    let got = map.values().next().expect("one entry");
    assert_eq!((got.chain, got.dest_chain), (3, 4));
    assert_eq!(got.address, [0xEEu8; 32]);
    assert_eq!(got.peer_address, peer_address);
}
