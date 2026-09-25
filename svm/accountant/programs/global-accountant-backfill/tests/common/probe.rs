//! Cost measurement for the operator cost probes: per-transaction fee, compute
//! units and rent read back from `getTransaction`, plus the catalogue reader the
//! probes sample their entries from.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::thread;
use std::time::{Duration, Instant};

use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::{RpcTransactionConfig, UiTransactionEncoding};
use solana_commitment_config::CommitmentConfig;
use solana_signature::Signature;

/// Snapshot catalogue the probes sample. Produced by the snapshot tool outside
/// this workspace; the probes skip when it is absent.
pub const CATALOGUE_PATH: &str = "/tmp/wormchain-mainnet-snapshot/catalogue.jsonl";

const TX_INDEX_TIMEOUT: Duration = Duration::from_secs(10);
const TX_INDEX_POLL_INTERVAL: Duration = Duration::from_millis(150);

/// Headline conversion for the printed figures.
pub const SOL_USD: f64 = 230.0;
pub const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

/// Line reader over the catalogue. A missing catalogue prints a skip message and
/// yields `None`, which every probe treats as "stop here".
pub fn catalogue_lines() -> Option<impl Iterator<Item = String>> {
    match File::open(CATALOGUE_PATH) {
        Ok(file) => Some(BufReader::new(file).lines().map_while(Result::ok)),
        Err(e) => {
            eprintln!(
                "[cost-probe] SKIPPED: no catalogue at {CATALOGUE_PATH} ({e}). \
                 Run the wormchain snapshot tool first."
            );
            None
        }
    }
}

/// 32 bytes from a hex string, with or without the `0x` prefix.
pub fn parse_hex32(s: &str) -> [u8; 32] {
    let s = s.strip_prefix("0x").unwrap_or(s);
    assert_eq!(s.len(), 64, "expected 32-byte hex, got {} chars", s.len());
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).expect("hex digit");
    }
    out
}

/// One transaction's measured cost.
#[derive(Default, Clone, Copy, Debug)]
pub struct TxCost {
    pub fee_lamports: u64,
    pub cu_consumed: u64,
    /// Lamports the payer lost beyond the fee: the rent of the PDAs this
    /// transaction created.
    pub rent_lamports: u64,
    pub entry_count: usize,
}

/// Fee, compute units and rent of transaction `sig`, from `meta`.
///
/// Polls `getTransaction` until surfpool indexes the transaction, then reads
/// `meta.fee`, `meta.compute_units_consumed` and the payer's balance delta. The
/// payer is account index 0: sole signer and fee payer in every probe
/// transaction. Returns `None` when the transaction stays unindexed.
pub fn measure_tx(rpc: &RpcClient, sig: &Signature) -> Option<TxCost> {
    let config = RpcTransactionConfig {
        encoding: Some(UiTransactionEncoding::Json),
        commitment: Some(CommitmentConfig::confirmed()),
        max_supported_transaction_version: Some(0),
    };
    let deadline = Instant::now() + TX_INDEX_TIMEOUT;
    let confirmed = loop {
        match rpc.get_transaction_with_config(sig, config) {
            Ok(tx) => break tx,
            Err(_) if Instant::now() < deadline => thread::sleep(TX_INDEX_POLL_INTERVAL),
            Err(e) => {
                eprintln!("[cost-probe] getTransaction {sig}: {e}");
                return None;
            }
        }
    };
    let meta = confirmed.transaction.meta?;
    let cu: Option<u64> = meta.compute_units_consumed.into();
    let pre = *meta.pre_balances.first()?;
    let post = *meta.post_balances.first()?;
    Some(TxCost {
        fee_lamports: meta.fee,
        cu_consumed: cu.unwrap_or(0),
        rent_lamports: pre.saturating_sub(post + meta.fee),
        entry_count: 0,
    })
}

/// Running totals over the transactions of one instruction kind.
#[derive(Default, Debug)]
pub struct Aggregate {
    pub txs: u64,
    pub entries: u64,
    pub fee_lamports: u64,
    pub cu_consumed: u64,
    pub rent_lamports: u64,
}

impl Aggregate {
    pub fn add(&mut self, cost: TxCost) {
        self.txs += 1;
        self.entries += cost.entry_count as u64;
        self.fee_lamports += cost.fee_lamports;
        self.cu_consumed += cost.cu_consumed;
        self.rent_lamports += cost.rent_lamports;
    }

    /// Multiplier that scales this sample's totals to `full_entries` entries.
    pub fn scale_to(&self, full_entries: u64) -> f64 {
        assert!(self.entries > 0, "cannot extrapolate from an empty sample");
        full_entries as f64 / self.entries as f64
    }

    pub fn per_tx(&self, total: u64) -> u64 {
        total / self.txs.max(1)
    }
}

pub fn lamports_to_usd(lamports: u64) -> f64 {
    (lamports as f64 / LAMPORTS_PER_SOL) * SOL_USD
}

pub fn lamports_to_sol(lamports: u64) -> f64 {
    lamports as f64 / LAMPORTS_PER_SOL
}

/// `L / SOL / USD` on one line, the shape every probe total prints.
pub fn cost_line(label: &str, lamports: u64) -> String {
    format!(
        "{label}{:>15} L  ({:>8.2} SOL  ${:>10.2})",
        lamports,
        lamports_to_sol(lamports),
        lamports_to_usd(lamports)
    )
}
