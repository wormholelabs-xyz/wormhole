//! Post-backfill reconciliation.
//!
//! Walks the program's accounts per class (`getProgramAccounts` filtered by
//! the offset-0 tag and the exact layout size) and compares them against the
//! snapshot. Balances keep a per-key report; every other class compares whole
//! layouts keyed by PDA address, so a field, padding or tag difference is a
//! mismatch and an account at a non-canonical address is an error.
//!
//! The fetch + parse are split from the comparison so the comparison logic
//! is unit-testable without a real RPC.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use global_accountant_definitions::{AccountLayout, BalanceAccountLayout};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::RpcProgramAccountsConfig;
use solana_client::rpc_filter::{Memcmp, RpcFilterType};
use solana_pubkey::Pubkey;

use crate::expected_accounts::PdaAccount;
use crate::noreplay::{fetch_buckets, group_by_bucket, BucketAccount};
use crate::snapshot::{SnapshotReader, SnapshotRow};
use crate::tx_builder::BackfillTarget;

/// Rows per account class, both sides. Mainnet: 17,367 balances, tens of the others.
pub const MAX_RECONCILE_ROWS: usize = 100_000;

/// Snapshot transfers checked for NoReplay marks. Mainnet: 5,516,669.
pub const MAX_RECONCILE_TRANSFERS: usize = 16_000_000;

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
    pub snapshot_balance: BalanceValue,
    pub on_chain_balance: BalanceValue,
}

/// Schema-neutral map comparison verdict — counts of matches and the three
/// diff classes, without binding to a concrete key/value type. Used for every
/// non-balance account class.
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

/// Pure comparison over any keyed map. The two HashMaps come from
/// independent sources (snapshot file, on-chain gPA fetch).
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

/// Pure comparison. The two HashMaps come from independent sources
/// (snapshot file, on-chain gPA fetch). Retains the Balance-specific report
/// (with per-key mismatch detail) that `run` prints.
pub fn compare_balances(
    expected: &HashMap<BalanceKey, BalanceValue>,
    actual: &HashMap<BalanceKey, BalanceValue>,
) -> ReconciliationReport {
    let mut report = ReconciliationReport::default();
    for (key, expected_value) in expected {
        match actual.get(key) {
            Some(actual_value) if actual_value == expected_value => report.matched += 1,
            Some(actual_value) => report.mismatched.push(Mismatch {
                key: *key,
                snapshot_balance: *expected_value,
                on_chain_balance: *actual_value,
            }),
            None => report.missing_from_chain.push(*key),
        }
    }
    report
        .unexpected_on_chain
        .extend(actual.keys().filter(|k| !expected.contains_key(*k)));
    report
}

/// Walk the snapshot once and build the expected balance map.
pub fn load_expected_balances(path: &Path) -> Result<HashMap<BalanceKey, BalanceValue>> {
    let mut out = HashMap::new();
    for row in SnapshotReader::open(path).context("open snapshot")? {
        if let SnapshotRow::Balance(a) = row? {
            if out.len() == MAX_RECONCILE_ROWS {
                bail!("account rows exceed MAX_RECONCILE_ROWS ({MAX_RECONCILE_ROWS})");
            }
            let key = (a.chain, a.token_chain, a.token_address);
            if out.insert(key, a.balance).is_some() {
                bail!("snapshot holds two account rows for {key:?}");
            }
        }
    }
    Ok(out)
}

/// Every program account of layout `L`, keyed by address. Each account is
/// parsed through the definitions layout, its tag is checked, and its address
/// must be the PDA its own key derives; any other address is an error.
pub async fn fetch_layouts<L>(rpc: &RpcClient, program_id: &Pubkey) -> Result<HashMap<Pubkey, L>>
where
    L: AccountLayout + PdaAccount,
{
    let config = RpcProgramAccountsConfig {
        filters: Some(vec![
            RpcFilterType::Memcmp(Memcmp::new_raw_bytes(0, vec![<L as AccountLayout>::TAG])),
            RpcFilterType::DataSize(<L as AccountLayout>::LEN as u64),
        ]),
        ..Default::default()
    };
    #[allow(deprecated)] // get_program_ui_accounts returns UiAccount which is harder to byte-parse
    let accounts = rpc
        .get_program_accounts_with_config(program_id, config)
        .await
        .with_context(|| format!("getProgramAccounts(tag={})", <L as AccountLayout>::TAG))?;
    if accounts.len() > MAX_RECONCILE_ROWS {
        bail!(
            "{} accounts with tag {} exceed MAX_RECONCILE_ROWS",
            accounts.len(),
            <L as AccountLayout>::TAG
        );
    }
    let mut out = HashMap::with_capacity(accounts.len());
    for (address, account) in accounts {
        if account.data.len() != <L as AccountLayout>::LEN {
            bail!(
                "account {address}: {} bytes under the size filter",
                account.data.len()
            );
        }
        let layout: L = bytemuck::pod_read_unaligned(&account.data);
        if layout.tag() != <L as AccountLayout>::TAG {
            bail!(
                "account {address}: tag {} under filter {}",
                layout.tag(),
                <L as AccountLayout>::TAG
            );
        }
        let canonical = layout.address(program_id);
        if canonical != address {
            bail!("account {address}: layout key derives {canonical}");
        }
        if out.insert(address, layout).is_some() {
            bail!("getProgramAccounts returned {address} twice");
        }
    }
    Ok(out)
}

/// Snapshot rows mapped to the layouts the backfill writes, keyed by PDA address.
pub fn load_expected_layouts<L>(
    path: &Path,
    program_id: &Pubkey,
    layouts: impl Fn(&SnapshotRow) -> Vec<L>,
) -> Result<HashMap<Pubkey, L>>
where
    L: PdaAccount,
{
    let mut out = HashMap::new();
    for row in SnapshotReader::open(path).context("open snapshot")? {
        for layout in layouts(&row?) {
            if out.len() == MAX_RECONCILE_ROWS {
                bail!("snapshot rows exceed MAX_RECONCILE_ROWS ({MAX_RECONCILE_ROWS})");
            }
            let address = layout.address(program_id);
            if out.insert(address, layout).is_some() {
                bail!("snapshot maps two rows to PDA {address}");
            }
        }
    }
    Ok(out)
}

/// Every Balance PDA, keyed by `(chain, token_chain, token_address)`.
pub async fn fetch_on_chain_balances(
    rpc: &RpcClient,
    program_id: &Pubkey,
) -> Result<HashMap<BalanceKey, BalanceValue>> {
    let layouts = fetch_layouts::<BalanceAccountLayout>(rpc, program_id).await?;
    Ok(layouts
        .into_values()
        .map(|l| ((l.chain, l.token_chain, l.token_address), l.balance.0))
        .collect())
}

/// End-to-end balance reconciliation: load snapshot, fetch on-chain state,
/// compare.
pub async fn reconcile_balances(
    rpc: &RpcClient,
    program_id: &Pubkey,
    snapshot_path: &Path,
) -> Result<ReconciliationReport> {
    let expected = load_expected_balances(snapshot_path)?;
    let actual = fetch_on_chain_balances(rpc, program_id).await?;
    Ok(compare_balances(&expected, &actual))
}

/// End-to-end reconciliation of one non-balance account class.
pub async fn reconcile_layouts<L>(
    rpc: &RpcClient,
    program_id: &Pubkey,
    snapshot_path: &Path,
    layouts: impl Fn(&SnapshotRow) -> Vec<L>,
) -> Result<MapVerdict>
where
    L: AccountLayout + PdaAccount + PartialEq,
{
    let expected = load_expected_layouts(snapshot_path, program_id, layouts)?;
    let actual = fetch_layouts::<L>(rpc, program_id).await?;
    Ok(compare_maps(&expected, &actual))
}

/// NoReplay marks for the snapshot transfers. Extra marks are ga-parity's job.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct NoReplayVerdict {
    pub marked: usize,
    pub unmarked: usize,
    /// Buckets that do not exist, or are not a NoReplay bitmap account.
    pub missing_buckets: usize,
}

impl NoReplayVerdict {
    pub fn is_clean(&self) -> bool {
        self.unmarked == 0 && self.missing_buckets == 0
    }
}

/// Check that every snapshot transfer's NoReplay bit is set. Buckets are
/// owned by the NoReplay program, so they are fetched by derived address.
pub async fn reconcile_noreplay(
    rpc: &RpcClient,
    target: &BackfillTarget,
    snapshot_path: &Path,
) -> Result<NoReplayVerdict> {
    let mut transfers = Vec::new();
    for row in SnapshotReader::open(snapshot_path).context("open snapshot")? {
        if let SnapshotRow::Transfer(t) = row? {
            if transfers.len() == MAX_RECONCILE_TRANSFERS {
                bail!("transfers exceed MAX_RECONCILE_TRANSFERS ({MAX_RECONCILE_TRANSFERS})");
            }
            transfers.push(t);
        }
    }
    let grouped = group_by_bucket(&transfers);
    let buckets: Vec<_> = grouped.keys().copied().collect();
    let fetched = fetch_buckets(
        rpc,
        &target.noreplay_program,
        &target.noreplay_authority,
        &buckets,
    )
    .await?;

    let mut verdict = NoReplayVerdict::default();
    for (sequences, (_, state)) in grouped.values().zip(fetched) {
        let BucketAccount::Bitmap(bitmap) = state else {
            verdict.missing_buckets += 1;
            verdict.unmarked += sequences.len();
            continue;
        };
        let marked = sequences.iter().filter(|&&s| bitmap.is_marked(s)).count();
        verdict.marked += marked;
        verdict.unmarked += sequences.len() - marked;
    }
    let transfers = transfers.len();
    assert_eq!(
        verdict.marked + verdict.unmarked,
        transfers,
        "every transfer counted once"
    );
    Ok(verdict)
}
