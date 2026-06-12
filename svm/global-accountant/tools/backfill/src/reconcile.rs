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
use global_accountant_definitions::{AccountTag, BalanceAccountLayout};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::RpcProgramAccountsConfig;
use solana_client::rpc_filter::{Memcmp, RpcFilterType};
use solana_pubkey::Pubkey;

use crate::catalogue::{CatalogueReader, Record};

/// Balance layout size on disk. Sourced from the shared definitions crate so it
/// can never drift from the on-chain layout (currently 70 bytes: tag (1) +
/// _pad0 (1) + chain (2) + token_chain (2) + token_address (32) + balance (32)).
pub const BALANCE_LAYOUT_LEN: u64 = BalanceAccountLayout::LEN as u64;

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

/// Pure comparison — no I/O. The two HashMaps come from independent sources
/// (catalogue file, on-chain gPA fetch).
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

/// Fetch every Balance PDA from the backfill program via `getProgramAccounts`.
/// Discriminated by the offset-0 account tag (`AccountTag::Balance`) — the
/// canonical filter every consumer uses — plus a `dataSize` belt-and-suspenders.
pub async fn fetch_on_chain_balances(
    rpc: &RpcClient,
    program_id: &Pubkey,
) -> Result<HashMap<BalanceKey, BalanceValue>> {
    let config = RpcProgramAccountsConfig {
        filters: Some(vec![
            RpcFilterType::Memcmp(Memcmp::new_raw_bytes(0, vec![AccountTag::Balance as u8])),
            RpcFilterType::DataSize(BALANCE_LAYOUT_LEN),
        ]),
        ..Default::default()
    };
    #[allow(deprecated)] // get_program_ui_accounts returns UiAccount which is harder to byte-parse
    let accounts = rpc
        .get_program_accounts_with_config(program_id, config)
        .await
        .context("getProgramAccounts(BalanceLayout)")?;
    let mut out = HashMap::with_capacity(accounts.len());
    for (_pubkey, acc) in accounts {
        if acc.data.len() != BALANCE_LAYOUT_LEN as usize {
            return Err(anyhow!(
                "unexpected account data length {} (want {})",
                acc.data.len(),
                BALANCE_LAYOUT_LEN
            ));
        }
        // Layout: tag@0, _pad0@1, chain@2, token_chain@4, token_address@6, balance@38.
        let chain = u16::from_le_bytes([acc.data[2], acc.data[3]]);
        let token_chain = u16::from_le_bytes([acc.data[4], acc.data[5]]);
        let mut token_address = [0u8; 32];
        token_address.copy_from_slice(&acc.data[6..38]);
        let mut balance = [0u8; 32];
        balance.copy_from_slice(&acc.data[38..70]);
        out.insert((chain, token_chain, token_address), balance);
    }
    Ok(out)
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
