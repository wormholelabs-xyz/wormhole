//! `ga-backfill index-stats` — walk the catalogue and report record counts,
//! chunking projection, and cost estimate.
//!
//! Replaces the ad-hoc `python3 -c '...'` analyses we ran during the cost
//! probes. All numeric constants (per-tx fee, per-PDA rent) are pinned to the
//! same empirical/formula-derived values used by the at-scale probe.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};

use crate::catalogue::{CatalogueReader, Record};
use crate::chunker::{ChunkPlan, Chunker};

/// Base Solana tx fee for a single-signature tx. Deterministic on mainnet.
pub const TX_FEE_LAMPORTS: u64 = 5_000;

/// Per-NoReplay-bucket PDA rent (129-byte account). Matches Solana's textbook
/// formula `3480 × (128 + 129) × 2 = 1,788,720`, empirically validated by
/// `programs/global-accountant-backfill/tests/surfpool_e2e_cost_probe_at_scale.rs`.
pub const NOREPLAY_BUCKET_RENT_LAMPORTS: u64 = 1_788_720;

/// Per-BalanceAccount PDA rent (68-byte account after the `_reserved` drop).
/// `3480 × (128 + 68) × 2 = 1,364,160`.
pub const BALANCE_ACCOUNT_RENT_LAMPORTS: u64 = 1_364_160;

/// Per-ChainRegistration PDA rent (64-byte account, paid by the operational
/// program's `RegisterChain` ix post-upgrade).
pub const CHAIN_REGISTRATION_RENT_LAMPORTS: u64 = 1_113_600;

/// Per-Modification-log PDA rent (112-byte account, paid by the operational
/// program's `ModifyBalance` ix post-upgrade).
pub const MODIFICATION_LOG_RENT_LAMPORTS: u64 = 1_670_400;

/// Per-RelayerChainRegistration PDA rent (64-byte account). Same layout size as
/// ChainRegistration → same rent.
pub const RELAYER_REGISTRATION_RENT_LAMPORTS: u64 = CHAIN_REGISTRATION_RENT_LAMPORTS;

/// Per-TransceiverHub PDA rent (70-byte account). `3480 × (128 + 70) × 2 =
/// 1,378,080`.
pub const TRANSCEIVER_HUB_RENT_LAMPORTS: u64 = 1_378_080;

/// Per-TransceiverPeer PDA rent (70-byte account). Same layout size as
/// TransceiverHub → same rent.
pub const TRANSCEIVER_PEER_RENT_LAMPORTS: u64 = TRANSCEIVER_HUB_RENT_LAMPORTS;

/// Lamports per SOL.
pub const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

#[derive(Debug, Default, Clone)]
pub struct CatalogueStats {
    pub transfers: u64,
    pub accounts: u64,
    pub modifications: u64,
    pub registrations: u64,
    pub unique_emitters: usize,
    pub unique_buckets: usize,
    pub noreplay_chunks: usize,
    pub balance_chunks: usize,
    pub deferred_modifications: usize,
    pub deferred_registrations: usize,
    pub relayer_registrations: u64,
    pub transceiver_hubs: u64,
    pub transceiver_peers: u64,
    pub relayer_registration_chunks: usize,
    pub transceiver_hub_chunks: usize,
    pub transceiver_peer_chunks: usize,
}

impl CatalogueStats {
    pub fn total_records(&self) -> u64 {
        self.transfers
            + self.accounts
            + self.modifications
            + self.registrations
            + self.relayer_registrations
            + self.transceiver_hubs
            + self.transceiver_peers
    }

    /// Transactions submitted during the backfill phase (the NoReplay/Balance
    /// pair plus the three NTT-native map instructions). Deferred mods + regs
    /// are NOT counted here — they go through the operational program
    /// post-upgrade.
    pub fn backfill_txs(&self) -> u64 {
        (self.noreplay_chunks
            + self.balance_chunks
            + self.relayer_registration_chunks
            + self.transceiver_hub_chunks
            + self.transceiver_peer_chunks) as u64
    }

    /// Operational-program txs (Phase 7). Each governance VAA replay needs a
    /// `PostSignatures` prelude + the program ix, so we count 2× the deferred
    /// records.
    pub fn operational_txs(&self) -> u64 {
        (self.deferred_modifications + self.deferred_registrations) as u64 * 2
    }

    /// Total tx fees in lamports — backfill txs + operational txs at 5,000 L each.
    pub fn fees_lamports(&self) -> u64 {
        (self.backfill_txs() + self.operational_txs()) * TX_FEE_LAMPORTS
    }

    /// Total rent in lamports across all PDA classes.
    pub fn rent_lamports(&self) -> u64 {
        self.unique_buckets as u64 * NOREPLAY_BUCKET_RENT_LAMPORTS
            + self.accounts * BALANCE_ACCOUNT_RENT_LAMPORTS
            + self.deferred_registrations as u64 * CHAIN_REGISTRATION_RENT_LAMPORTS
            + self.deferred_modifications as u64 * MODIFICATION_LOG_RENT_LAMPORTS
            + self.relayer_registrations * RELAYER_REGISTRATION_RENT_LAMPORTS
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
    relayer_registrations: u64,
    transceiver_hubs: u64,
    transceiver_peers: u64,
    buckets: HashSet<(u16, [u8; 32], u64)>,
    emitters: HashSet<(u16, [u8; 32])>,
}

impl Counters {
    fn observe(&mut self, rec: &Record) {
        match rec {
            Record::Transfer(t) => {
                self.transfers += 1;
                self.buckets.insert((t.chain, t.emitter, t.sequence / 1024));
                self.emitters.insert((t.chain, t.emitter));
            }
            Record::Account(_) => self.accounts += 1,
            Record::Modification(_) => self.modifications += 1,
            Record::Registration(_) => self.registrations += 1,
            Record::RelayerChainRegistration(_) => self.relayer_registrations += 1,
            Record::TransceiverHub(_) => self.transceiver_hubs += 1,
            Record::TransceiverPeer(_) => self.transceiver_peers += 1,
        }
    }
}

/// Walk the catalogue and compute stats. Streams the file — bounded memory
/// modulo the bucket/emitter HashSets (~80 bytes per entry, expected ~5,500
/// entries = ~440 KB peak).
pub fn compute(path: &Path) -> Result<CatalogueStats> {
    let counters = RefCell::new(Counters::default());
    let reader = CatalogueReader::open(path).context("open catalogue")?;

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

    let mut noreplay_chunks = 0usize;
    let mut balance_chunks = 0usize;
    let mut deferred_modifications = 0usize;
    let mut deferred_registrations = 0usize;
    let mut relayer_registration_chunks = 0usize;
    let mut transceiver_hub_chunks = 0usize;
    let mut transceiver_peer_chunks = 0usize;
    for chunk in Chunker::new(iter) {
        match chunk {
            ChunkPlan::BackfillNoReplay(_) => noreplay_chunks += 1,
            ChunkPlan::BackfillBalance(_) => balance_chunks += 1,
            ChunkPlan::DeferredModification(_) => deferred_modifications += 1,
            ChunkPlan::DeferredRegistration(_) => deferred_registrations += 1,
            ChunkPlan::BackfillRelayerRegistration(_) => relayer_registration_chunks += 1,
            ChunkPlan::BackfillTransceiverHub(_) => transceiver_hub_chunks += 1,
            ChunkPlan::BackfillTransceiverPeer(_) => transceiver_peer_chunks += 1,
        }
    }

    let c = counters.into_inner();
    Ok(CatalogueStats {
        transfers: c.transfers,
        accounts: c.accounts,
        modifications: c.modifications,
        registrations: c.registrations,
        unique_emitters: c.emitters.len(),
        unique_buckets: c.buckets.len(),
        noreplay_chunks,
        balance_chunks,
        deferred_modifications,
        deferred_registrations,
        relayer_registrations: c.relayer_registrations,
        transceiver_hubs: c.transceiver_hubs,
        transceiver_peers: c.transceiver_peers,
        relayer_registration_chunks,
        transceiver_hub_chunks,
        transceiver_peer_chunks,
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

fn print_report(path: &Path, s: &CatalogueStats, sol_usd: f64) {
    println!("catalogue:               {}", path.display());
    println!("total records:           {}", s.total_records());
    println!("  transfer:              {}", s.transfers);
    println!("  account:               {}", s.accounts);
    println!("  modification:          {}", s.modifications);
    println!("  registration:          {}", s.registrations);
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
    println!("chunking projection:");
    println!(
        "  BackfillNoReplay txs:  {:>8} (≤18 entries each)",
        s.noreplay_chunks
    );
    println!(
        "  BackfillBalance txs:   {:>8} (≤8 entries each)",
        s.balance_chunks
    );
    println!(
        "  RelayerRegistration:   {:>8} (≤12 entries each)",
        s.relayer_registration_chunks
    );
    println!(
        "  TransceiverHub txs:    {:>8} (≤8 entries each)",
        s.transceiver_hub_chunks
    );
    println!(
        "  TransceiverPeer txs:   {:>8} (≤8 entries each)",
        s.transceiver_peer_chunks
    );
    println!(
        "  Deferred records:      {:>8} ({} mods + {} regs — Phase 7, operational program)",
        s.deferred_modifications + s.deferred_registrations,
        s.deferred_modifications,
        s.deferred_registrations
    );
    println!("  backfill phase txs:    {:>8}", s.backfill_txs());
    println!(
        "  operational phase txs: {:>8} (2× deferred records: PostSig + ix)",
        s.operational_txs()
    );
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
