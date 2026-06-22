//! Post-backfill reconciliation.
//!
//! Walks the on-chain Balance PDAs (via `getProgramAccounts` filtered by data
//! size) and compares them against the catalogue's Account records. Mismatches
//! get logged for human review; missing/extra entries are flagged separately.
//!
//! The fetch + parse are split from the comparison so the comparison logic
//! is unit-testable without a real RPC. Phase 9's surfpool e2e exercises the
//! full path.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use global_accountant_definitions::{
    AccountTag, BalanceAccountLayout, RelayerChainRegistrationLayout, TransceiverHubLayout,
    TransceiverPeerLayout,
};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::RpcProgramAccountsConfig;
use solana_client::rpc_filter::{Memcmp, RpcFilterType};
use solana_pubkey::Pubkey;

use crate::catalogue::{CatalogueReader, Record};

/// Balance layout size on disk. Sourced from the shared definitions crate so it
/// can never drift from the on-chain layout (currently 70 bytes: tag (1) +
/// _pad0 (1) + chain (2) + token_chain (2) + token_address (32) + balance (32)).
pub const BALANCE_LAYOUT_LEN: u64 = BalanceAccountLayout::LEN as u64;

/// RelayerChainRegistration layout size on disk (64 bytes).
pub const RELAYER_REGISTRATION_LAYOUT_LEN: u64 = RelayerChainRegistrationLayout::LEN as u64;

/// TransceiverHub layout size on disk (70 bytes).
pub const TRANSCEIVER_HUB_LAYOUT_LEN: u64 = TransceiverHubLayout::LEN as u64;

/// TransceiverPeer layout size on disk (70 bytes).
pub const TRANSCEIVER_PEER_LAYOUT_LEN: u64 = TransceiverPeerLayout::LEN as u64;

/// Identity of one Balance entry — `(chain, token_chain, token_address)`.
pub type BalanceKey = (u16, u16, [u8; 32]);

/// On-disk value — the 32-byte big-endian balance.
pub type BalanceValue = [u8; 32];

#[derive(Debug, Default)]
pub struct ReconciliationReport {
    pub matched: usize,
    pub mismatched: Vec<Mismatch>,
    pub missing_from_chain: Vec<BalanceKey>,
    pub unexpected_on_chain: Vec<BalanceKey>,
}

impl ReconciliationReport {
    pub fn is_clean(&self) -> bool {
        self.mismatched.is_empty()
            && self.missing_from_chain.is_empty()
            && self.unexpected_on_chain.is_empty()
    }

    pub fn total_diffs(&self) -> usize {
        self.mismatched.len() + self.missing_from_chain.len() + self.unexpected_on_chain.len()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub key: BalanceKey,
    pub catalogue_balance: BalanceValue,
    pub on_chain_balance: BalanceValue,
}

/// Schema-neutral map comparison verdict — counts of matches and the three
/// diff classes, without binding to a concrete key/value type. Used to
/// reconcile the NTT-native maps, whose key/value shapes differ from Balance.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MapVerdict {
    pub matched: usize,
    pub mismatched: usize,
    pub missing_from_chain: usize,
    pub unexpected_on_chain: usize,
}

impl MapVerdict {
    pub fn is_clean(&self) -> bool {
        self.mismatched == 0 && self.missing_from_chain == 0 && self.unexpected_on_chain == 0
    }

    pub fn total_diffs(&self) -> usize {
        self.mismatched + self.missing_from_chain + self.unexpected_on_chain
    }
}

/// Pure comparison over any keyed map — no I/O. The two HashMaps come from
/// independent sources (catalogue file, on-chain gPA fetch).
pub fn compare_maps<K, V>(expected: &HashMap<K, V>, actual: &HashMap<K, V>) -> MapVerdict
where
    K: std::cmp::Eq + std::hash::Hash,
    V: PartialEq,
{
    let mut verdict = MapVerdict::default();
    for (key, expected_value) in expected {
        match actual.get(key) {
            Some(actual_value) if actual_value == expected_value => verdict.matched += 1,
            Some(_) => verdict.mismatched += 1,
            None => verdict.missing_from_chain += 1,
        }
    }
    for key in actual.keys() {
        if !expected.contains_key(key) {
            verdict.unexpected_on_chain += 1;
        }
    }
    verdict
}

/// Pure comparison — no I/O. The two HashMaps come from independent sources
/// (catalogue file, on-chain gPA fetch). Retains the Balance-specific report
/// (with per-key mismatch detail) that the WTT reconcile path consumes.
pub fn compare_balances(
    expected: &HashMap<BalanceKey, BalanceValue>,
    actual: &HashMap<BalanceKey, BalanceValue>,
) -> ReconciliationReport {
    let mut report = ReconciliationReport::default();
    let actual_keys: HashSet<&BalanceKey> = actual.keys().collect();
    for (key, expected_value) in expected {
        match actual.get(key) {
            Some(actual_value) if actual_value == expected_value => report.matched += 1,
            Some(actual_value) => report.mismatched.push(Mismatch {
                key: *key,
                catalogue_balance: *expected_value,
                on_chain_balance: *actual_value,
            }),
            None => report.missing_from_chain.push(*key),
        }
    }
    for actual_key in actual_keys {
        if !expected.contains_key(actual_key) {
            report.unexpected_on_chain.push(*actual_key);
        }
    }
    report
}

/// Walk the catalogue once and build the expected balance map.
pub fn load_expected_balances(path: &Path) -> Result<HashMap<BalanceKey, BalanceValue>> {
    let mut out = HashMap::new();
    for record in CatalogueReader::open(path).context("open catalogue")? {
        if let Record::Account(a) = record? {
            out.insert((a.chain, a.token_chain, a.token_address), a.balance);
        }
    }
    Ok(out)
}

/// Fetch every PDA of one account class from the backfill program via
/// `getProgramAccounts`, discriminated by the offset-0 account `tag` — the
/// canonical filter every consumer uses — plus a `dataSize` belt-and-suspenders.
///
/// The `(tag, layout_len)` pair parameterizes the filter so the same fetch
/// drives every map: Balance (`AccountTag::Balance`, 70), and the three
/// NTT-native maps (`RelayerChainRegistration`/5/64, `TransceiverHub`/6/70,
/// `TransceiverPeer`/7/70). Each returned account is validated to the exact
/// `layout_len` before its raw bytes are handed to `decode`.
pub async fn fetch_tagged_accounts<K, V, F>(
    rpc: &RpcClient,
    program_id: &Pubkey,
    tag: AccountTag,
    layout_len: u64,
    decode: F,
) -> Result<HashMap<K, V>>
where
    K: std::cmp::Eq + std::hash::Hash,
    F: Fn(&[u8]) -> (K, V),
{
    let config = RpcProgramAccountsConfig {
        filters: Some(vec![
            RpcFilterType::Memcmp(Memcmp::new_raw_bytes(0, vec![tag as u8])),
            RpcFilterType::DataSize(layout_len),
        ]),
        ..Default::default()
    };
    #[allow(deprecated)] // get_program_ui_accounts returns UiAccount which is harder to byte-parse
    let accounts = rpc
        .get_program_accounts_with_config(program_id, config)
        .await
        .with_context(|| format!("getProgramAccounts(tag={}, len={layout_len})", tag as u8))?;
    let mut out = HashMap::with_capacity(accounts.len());
    for (_pubkey, acc) in accounts {
        if acc.data.len() != layout_len as usize {
            return Err(anyhow!(
                "unexpected account data length {} (want {layout_len})",
                acc.data.len(),
            ));
        }
        let (k, v) = decode(&acc.data);
        out.insert(k, v);
    }
    Ok(out)
}

/// Fetch every Balance PDA from the backfill program. Thin wrapper over
/// [`fetch_tagged_accounts`] with the Balance tag/size and decoder.
pub async fn fetch_on_chain_balances(
    rpc: &RpcClient,
    program_id: &Pubkey,
) -> Result<HashMap<BalanceKey, BalanceValue>> {
    fetch_tagged_accounts(
        rpc,
        program_id,
        AccountTag::Balance,
        BALANCE_LAYOUT_LEN,
        |data| {
            // Layout: tag@0, _pad0@1, chain@2, token_chain@4, token_address@6, balance@38.
            let chain = u16::from_le_bytes([data[2], data[3]]);
            let token_chain = u16::from_le_bytes([data[4], data[5]]);
            let mut token_address = [0u8; 32];
            token_address.copy_from_slice(&data[6..38]);
            let mut balance = [0u8; 32];
            balance.copy_from_slice(&data[38..70]);
            ((chain, token_chain, token_address), balance)
        },
    )
    .await
}

/// End-to-end balance reconciliation: load catalogue, fetch on-chain state,
/// compare.
pub async fn reconcile_balances(
    rpc: &RpcClient,
    program_id: &Pubkey,
    catalogue_path: &Path,
) -> Result<ReconciliationReport> {
    let expected = load_expected_balances(catalogue_path)?;
    let actual = fetch_on_chain_balances(rpc, program_id).await?;
    Ok(compare_balances(&expected, &actual))
}

// ============================================================================
// NTT-native maps
// ============================================================================
//
// Each map reuses the schema-neutral `compare_balances` comparison over a
// `(key, value)` HashMap — the report's "balance" field names read as generic
// "value" here. Keys mirror the program's PDA seed tuples; values are the
// remaining stored fields.

/// Identity of one RelayerChainRegistration entry — `chain`.
pub type RelayerRegistrationKey = u16;
/// Stored value — the 32-byte registered emitter address.
pub type RelayerRegistrationValue = [u8; 32];

/// Identity of one TransceiverHub entry — `(chain, address)`.
pub type TransceiverHubKey = (u16, [u8; 32]);
/// Stored value — `(hub_chain, hub_address)`.
pub type TransceiverHubValue = (u16, [u8; 32]);

/// Identity of one TransceiverPeer entry — `(chain, address, dest_chain)`.
pub type TransceiverPeerKey = (u16, [u8; 32], u16);
/// Stored value — the 32-byte peer address.
pub type TransceiverPeerValue = [u8; 32];

/// Walk the catalogue once and build the expected relayer-registration map.
pub fn load_expected_relayer_registrations(
    path: &Path,
) -> Result<HashMap<RelayerRegistrationKey, RelayerRegistrationValue>> {
    let mut out = HashMap::new();
    for record in CatalogueReader::open(path).context("open catalogue")? {
        if let Record::RelayerChainRegistration(r) = record? {
            out.insert(r.chain, r.registered_emitter);
        }
    }
    Ok(out)
}

/// Walk the catalogue once and build the expected transceiver-hub map.
pub fn load_expected_transceiver_hubs(
    path: &Path,
) -> Result<HashMap<TransceiverHubKey, TransceiverHubValue>> {
    let mut out = HashMap::new();
    for record in CatalogueReader::open(path).context("open catalogue")? {
        if let Record::TransceiverHub(h) = record? {
            out.insert((h.chain, h.address), (h.hub_chain, h.hub_address));
        }
    }
    Ok(out)
}

/// Walk the catalogue once and build the expected transceiver-peer map.
pub fn load_expected_transceiver_peers(
    path: &Path,
) -> Result<HashMap<TransceiverPeerKey, TransceiverPeerValue>> {
    let mut out = HashMap::new();
    for record in CatalogueReader::open(path).context("open catalogue")? {
        if let Record::TransceiverPeer(p) = record? {
            out.insert((p.chain, p.address, p.dest_chain), p.peer_address);
        }
    }
    Ok(out)
}

/// Fetch every RelayerChainRegistration PDA (tag 5, 64 bytes).
pub async fn fetch_on_chain_relayer_registrations(
    rpc: &RpcClient,
    program_id: &Pubkey,
) -> Result<HashMap<RelayerRegistrationKey, RelayerRegistrationValue>> {
    fetch_tagged_accounts(
        rpc,
        program_id,
        AccountTag::RelayerChainRegistration,
        RELAYER_REGISTRATION_LAYOUT_LEN,
        |data| {
            // Layout: tag@0, _pad0@1, chain@2, _padding@4 (28), emitter_address@32.
            let chain = u16::from_le_bytes([data[2], data[3]]);
            let mut emitter = [0u8; 32];
            emitter.copy_from_slice(&data[32..64]);
            (chain, emitter)
        },
    )
    .await
}

/// Fetch every TransceiverHub PDA (tag 6, 70 bytes).
pub async fn fetch_on_chain_transceiver_hubs(
    rpc: &RpcClient,
    program_id: &Pubkey,
) -> Result<HashMap<TransceiverHubKey, TransceiverHubValue>> {
    fetch_tagged_accounts(
        rpc,
        program_id,
        AccountTag::TransceiverHub,
        TRANSCEIVER_HUB_LAYOUT_LEN,
        |data| {
            // Layout: tag@0, _pad0@1, chain@2, hub_chain@4, address@6, hub_address@38.
            let chain = u16::from_le_bytes([data[2], data[3]]);
            let hub_chain = u16::from_le_bytes([data[4], data[5]]);
            let mut address = [0u8; 32];
            address.copy_from_slice(&data[6..38]);
            let mut hub_address = [0u8; 32];
            hub_address.copy_from_slice(&data[38..70]);
            ((chain, address), (hub_chain, hub_address))
        },
    )
    .await
}

/// Fetch every TransceiverPeer PDA (tag 7, 70 bytes).
pub async fn fetch_on_chain_transceiver_peers(
    rpc: &RpcClient,
    program_id: &Pubkey,
) -> Result<HashMap<TransceiverPeerKey, TransceiverPeerValue>> {
    fetch_tagged_accounts(
        rpc,
        program_id,
        AccountTag::TransceiverPeer,
        TRANSCEIVER_PEER_LAYOUT_LEN,
        |data| {
            // Layout: tag@0, _pad0@1, chain@2, dest_chain@4, address@6, peer_address@38.
            let chain = u16::from_le_bytes([data[2], data[3]]);
            let dest_chain = u16::from_le_bytes([data[4], data[5]]);
            let mut address = [0u8; 32];
            address.copy_from_slice(&data[6..38]);
            let mut peer_address = [0u8; 32];
            peer_address.copy_from_slice(&data[38..70]);
            ((chain, address, dest_chain), peer_address)
        },
    )
    .await
}
