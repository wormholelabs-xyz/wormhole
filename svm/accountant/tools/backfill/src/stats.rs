//! `ga-backfill stats` — walk the snapshot and report row counts,
//! batching projection, and cost estimate.
//!
//! Fees: 5,000 lamports per tx. Rent: `rent_exempt_lamports` over each
//! definitions layout size.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};

use global_accountant_definitions::{
    BalanceAccountLayout, ChainRegistrationLayout, ModifyBalanceLayout, NoReplayBitmapAccount,
    RegisterChainLayout, TransceiverHubLayout, TransceiverPeerLayout,
};

use crate::batcher::{
    Batcher, TxBatch, MAX_BALANCE_ENTRIES_PER_BATCH, MAX_CHAIN_REGISTRATION_ENTRIES_PER_BATCH,
    MAX_MODIFY_BALANCE_ENTRIES_PER_BATCH, MAX_TRANSCEIVER_HUB_ENTRIES_PER_BATCH,
    MAX_TRANSCEIVER_PEER_ENTRIES_PER_BATCH,
};
use crate::snapshot::{SnapshotReader, SnapshotRow};

/// Base Solana tx fee for a single-signature tx. Deterministic on mainnet.
pub const TX_FEE_LAMPORTS: u64 = 5_000;

/// Rent-exempt minimum for `len` data bytes: `3480 × (128 + len) × 2`.
pub const fn rent_exempt_lamports(len: usize) -> u64 {
    3_480 * (128 + len as u64) * 2
}

/// Per-NoReplay-bucket PDA rent (129-byte account): 1,788,720.
pub const NOREPLAY_BUCKET_RENT_LAMPORTS: u64 = rent_exempt_lamports(NoReplayBitmapAccount::LEN);

/// Per-BalanceAccount PDA rent (70-byte account).
pub const BALANCE_ACCOUNT_RENT_LAMPORTS: u64 = rent_exempt_lamports(BalanceAccountLayout::LEN);

/// Per-registration rent: the `ChainRegistration` PDA (64 bytes) plus the
/// `RegisterChain` record PDA (48 bytes).
pub const REGISTRATION_RENT_LAMPORTS: u64 = rent_exempt_lamports(ChainRegistrationLayout::LEN)
    + rent_exempt_lamports(RegisterChainLayout::LEN);

/// Per-`ModifyBalance` record PDA rent (112-byte account).
pub const MODIFY_BALANCE_RENT_LAMPORTS: u64 = rent_exempt_lamports(ModifyBalanceLayout::LEN);

/// Per-TransceiverHub PDA rent (70-byte account).
pub const TRANSCEIVER_HUB_RENT_LAMPORTS: u64 = rent_exempt_lamports(TransceiverHubLayout::LEN);

/// Per-TransceiverPeer PDA rent (70-byte account).
pub const TRANSCEIVER_PEER_RENT_LAMPORTS: u64 = rent_exempt_lamports(TransceiverPeerLayout::LEN);

const _: () = assert!(NOREPLAY_BUCKET_RENT_LAMPORTS == 1_788_720);

/// Lamports per SOL.
pub const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

#[derive(Debug, Default, Clone)]
pub struct SnapshotStats {
    pub transfers: u64,
    pub accounts: u64,
    pub modifications: u64,
    pub registrations: u64,
    pub governance: u64,
    pub relayer_registrations: u64,
    pub transceiver_hubs: u64,
    pub transceiver_peers: u64,
    pub unique_emitters: usize,
    pub unique_buckets: usize,
    pub noreplay_batches: usize,
    pub balance_batches: usize,
    pub modify_balance_batches: usize,
    pub chain_registration_batches: usize,
    pub transceiver_hub_batches: usize,
    pub transceiver_peer_batches: usize,
}

impl SnapshotStats {
    pub fn total_records(&self) -> u64 {
        self.transfers
            + self.accounts
            + self.modifications
            + self.registrations
            + self.governance
            + self.relayer_registrations
            + self.transceiver_hubs
            + self.transceiver_peers
    }

    /// One transaction per batch.
    pub fn backfill_txs(&self) -> u64 {
        (self.noreplay_batches
            + self.balance_batches
            + self.modify_balance_batches
            + self.chain_registration_batches
            + self.transceiver_hub_batches
            + self.transceiver_peer_batches) as u64
    }

    pub fn fees_lamports(&self) -> u64 {
        self.backfill_txs() * TX_FEE_LAMPORTS
    }

    /// Total rent in lamports across all PDA classes.
    pub fn rent_lamports(&self) -> u64 {
        self.unique_buckets as u64 * NOREPLAY_BUCKET_RENT_LAMPORTS
            + self.accounts * BALANCE_ACCOUNT_RENT_LAMPORTS
            + self.modifications * MODIFY_BALANCE_RENT_LAMPORTS
            + (self.registrations + self.relayer_registrations) * REGISTRATION_RENT_LAMPORTS
            + self.transceiver_hubs * TRANSCEIVER_HUB_RENT_LAMPORTS
            + self.transceiver_peers * TRANSCEIVER_PEER_RENT_LAMPORTS
    }

    pub fn total_lamports(&self) -> u64 {
        self.fees_lamports() + self.rent_lamports()
    }
}

#[derive(Default)]
struct Counters {
    transfers: u64,
    accounts: u64,
    modifications: u64,
    registrations: u64,
    governance: u64,
    relayer_registrations: u64,
    transceiver_hubs: u64,
    transceiver_peers: u64,
    buckets: HashSet<(u16, [u8; 32], u64)>,
    emitters: HashSet<(u16, [u8; 32])>,
}

impl Counters {
    fn observe(&mut self, rec: &SnapshotRow) {
        match rec {
            SnapshotRow::Transfer(t) => {
                self.transfers += 1;
                self.buckets.insert((
                    t.chain,
                    t.emitter,
                    NoReplayBitmapAccount::bucket_index(t.sequence),
                ));
                self.emitters.insert((t.chain, t.emitter));
            }
            SnapshotRow::Balance(_) => self.accounts += 1,
            SnapshotRow::ModifyBalance(_) => self.modifications += 1,
            SnapshotRow::TokenBridgeRegistration(_) => self.registrations += 1,
            SnapshotRow::Governance(_) => self.governance += 1,
            SnapshotRow::RelayerChainRegistration(_) => self.relayer_registrations += 1,
            SnapshotRow::TransceiverHub(_) => self.transceiver_hubs += 1,
            SnapshotRow::TransceiverPeer(_) => self.transceiver_peers += 1,
        }
    }
}

/// Walk the snapshot and compute stats. Streams the file — bounded memory
/// modulo the bucket/emitter HashSets (~80 bytes per entry, expected ~5,500
/// entries = ~440 KB peak).
pub fn compute(path: &Path) -> Result<SnapshotStats> {
    let counters = RefCell::new(Counters::default());
    let reader = SnapshotReader::open(path).context("open snapshot")?;

    let iter = reader.filter_map(|r| match r {
        Ok(rec) => {
            counters.borrow_mut().observe(&rec);
            Some(rec)
        }
        Err(e) => {
            eprintln!("warning: skipping malformed line: {e}");
            None
        }
    });

    let mut noreplay_batches = 0usize;
    let mut balance_batches = 0usize;
    let mut modify_balance_batches = 0usize;
    let mut chain_registration_batches = 0usize;
    let mut transceiver_hub_batches = 0usize;
    let mut transceiver_peer_batches = 0usize;
    for batch in Batcher::new(iter) {
        match batch {
            TxBatch::BackfillNoReplay(_) => noreplay_batches += 1,
            TxBatch::BackfillBalance(_) => balance_batches += 1,
            TxBatch::BackfillModifyBalance(_) => modify_balance_batches += 1,
            TxBatch::BackfillChainRegistration(_) => chain_registration_batches += 1,
            TxBatch::BackfillTransceiverHub(_) => transceiver_hub_batches += 1,
            TxBatch::BackfillTransceiverPeer(_) => transceiver_peer_batches += 1,
        }
    }

    let c = counters.into_inner();
    Ok(SnapshotStats {
        transfers: c.transfers,
        accounts: c.accounts,
        modifications: c.modifications,
        registrations: c.registrations,
        governance: c.governance,
        relayer_registrations: c.relayer_registrations,
        transceiver_hubs: c.transceiver_hubs,
        transceiver_peers: c.transceiver_peers,
        unique_emitters: c.emitters.len(),
        unique_buckets: c.buckets.len(),
        noreplay_batches,
        balance_batches,
        modify_balance_batches,
        chain_registration_batches,
        transceiver_hub_batches,
        transceiver_peer_batches,
    })
}

/// CLI entry point: compute stats and print a human-readable report.
pub fn run(path: &Path, sol_usd: f64) -> Result<()> {
    let stats = compute(path)?;
    print_report(path, &stats, sol_usd);
    Ok(())
}

fn lamports_to_sol(l: u64) -> f64 {
    l as f64 / LAMPORTS_PER_SOL as f64
}

fn lamports_to_usd(l: u64, sol_usd: f64) -> f64 {
    lamports_to_sol(l) * sol_usd
}

fn print_report(path: &Path, s: &SnapshotStats, sol_usd: f64) {
    println!("snapshot:               {}", path.display());
    println!("total records:           {}", s.total_records());
    println!("  transfer:              {}", s.transfers);
    println!("  account:               {}", s.accounts);
    println!("  modification:          {}", s.modifications);
    println!("  registration:          {}", s.registrations);
    println!("  governance:            {} (not backfilled)", s.governance);
    println!("  relayer registration:  {}", s.relayer_registrations);
    println!("  transceiver hub:       {}", s.transceiver_hubs);
    println!("  transceiver peer:      {}", s.transceiver_peers);
    println!();
    println!("unique (chain, emitter): {}", s.unique_emitters);
    println!("unique noreplay buckets: {}", s.unique_buckets);
    if s.unique_buckets > 0 {
        println!(
            "avg transfers / bucket:  {:.1}",
            s.transfers as f64 / s.unique_buckets as f64
        );
    }
    println!();
    println!("batching projection:");
    println!(
        "  BackfillNoReplay txs:  {:>8} (≤18 entries each)",
        s.noreplay_batches
    );
    println!(
        "  BackfillBalance txs:   {:>8} (≤{MAX_BALANCE_ENTRIES_PER_BATCH} entries each)",
        s.balance_batches
    );
    println!(
        "  ModifyBalance txs:     {:>8} (≤{MAX_MODIFY_BALANCE_ENTRIES_PER_BATCH} entries each)",
        s.modify_balance_batches
    );
    println!(
        "  ChainRegistration txs: {:>8} (≤{MAX_CHAIN_REGISTRATION_ENTRIES_PER_BATCH} entries each)",
        s.chain_registration_batches
    );
    println!(
        "  TransceiverHub txs:    {:>8} (≤{MAX_TRANSCEIVER_HUB_ENTRIES_PER_BATCH} entries each)",
        s.transceiver_hub_batches
    );
    println!(
        "  TransceiverPeer txs:   {:>8} (≤{MAX_TRANSCEIVER_PEER_ENTRIES_PER_BATCH} entries each)",
        s.transceiver_peer_batches
    );
    println!("  backfill phase txs:    {:>8}", s.backfill_txs());
    println!();
    println!("cost projection (at ${sol_usd:.2}/SOL):");
    let fees = s.fees_lamports();
    let rent = s.rent_lamports();
    let total = s.total_lamports();
    println!(
        "  fees:   {:>10.4} SOL   ${:>10.2}   [burned]",
        lamports_to_sol(fees),
        lamports_to_usd(fees, sol_usd)
    );
    println!(
        "  rent:   {:>10.4} SOL   ${:>10.2}   [locked in PDAs]",
        lamports_to_sol(rent),
        lamports_to_usd(rent, sol_usd)
    );
    println!(
        "  TOTAL:  {:>10.4} SOL   ${:>10.2}",
        lamports_to_sol(total),
        lamports_to_usd(total, sol_usd)
    );
}
