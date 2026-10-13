//! Group snapshot rows into tx-sized batches for the backfill program.
//!
//! ## Wire budget primer
//!
//! Solana's [`PACKET_DATA_SIZE`]-byte (1232) tx wire limit caps how many
//! entries fit in one ix.
//!
//! - **BackfillNoReplay**: bucket-aware. A flat entry-count cap alone is NOT
//!   a safe bound — an 18-entry same-emitter batch that happens to straddle
//!   18 distinct 1024-sequence buckets serializes to 1621 bytes, 389 over
//!   the limit (each additional unique bucket costs one more 32-byte account
//!   meta). `collect_transfers` therefore tracks the running unique-bucket
//!   count as it packs and, before accepting each candidate entry, measures
//!   the EXACT wire size the resulting tx would have (see
//!   [`noreplay_wire_size`]) — entries are packed up to
//!   [`MAX_NOREPLAY_ENTRIES_PER_BATCH`] as a secondary cap, but never past
//!   the point where the real serialized tx would exceed
//!   [`PACKET_DATA_SIZE`].
//! - **BackfillBalance**: 8 entries per tx. Each entry adds one writable
//!   account meta to the account list — the binding constraint here is the
//!   account-list size, not the data.
//!
//! ## Batching strategy
//!
//! For a snapshot sorted by `(kind, chain, emitter, sequence)` (the snapshot
//! tool's output order), the batcher:
//!
//! 1. Walks rows in source order.
//! 2. Streams transfers, closing a batch when the `(chain, emitter)` changes
//!    or the batch hits its entry cap or wire size.
//! 3. Buffers each contiguous run of every other row kind (balances,
//!    modifications, registrations, transceiver hubs and peers), sorts it into
//!    handler order, and emits it in capped batches.
//! 4. Skips `governance` rows: the backfill leaves governance sequences unmarked.
//!
//! [`check_batch_order`] then asserts the key order across batch boundaries.

use std::collections::{HashSet, VecDeque};
use std::iter::Peekable;
use std::sync::OnceLock;

use anyhow::{bail, Result};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use global_accountant_definitions::{
    BackfillChainRegistrationEntry, BackfillModifyBalanceEntry, BackfillTransceiverHubEntry,
    BackfillTransceiverPeerEntry, NoReplayBitmapAccount, MAX_BATCH_ENTRIES,
    MAX_CHAIN_REGISTRATION_ENTRIES,
};

use svm_accountant_tools_common::ids::{Address, BalanceKey, ChainId, EmitterKey, Sequence};
use svm_accountant_tools_common::snapshot::{
    BalanceRow, ChainRegistrationRow, ModifyBalanceRow, SnapshotRow, TransceiverHubRow,
    TransceiverPeerRow, TransferRow,
};

use crate::wire::{legacy_wire_size, PACKET_DATA_SIZE};

/// Backstop cap on `BackfillNoReplay` entries per tx, from the at-scale
/// probe's common case (few buckets per emitter). The binding bound is the
/// exact wire size, [`noreplay_wire_size`] against [`PACKET_DATA_SIZE`].
pub const MAX_NOREPLAY_ENTRIES_PER_BATCH: usize = 18;

/// Fixed accounts every `BackfillNoReplay` ix carries regardless of entry or
/// bucket count: payer, noreplay program, noreplay-authority PDA, system
/// program. Mirrors `tx_builder::build_backfill_noreplay_ix`'s account list.
const NOREPLAY_FIXED_ACCOUNTS: usize = 4;

/// Top-level fixed wire-data head: `disc(1) + group_count(1)`.
const NOREPLAY_FIXED_DATA_HEADER_BYTES: usize = 1 + 1;

/// Group header: `chain(2) + emitter(32) + entry_count(1)`. `collect_transfers`
/// only ever packs entries sharing one `(chain, emitter)` key, so a batch's
/// data is always exactly one group — see `tx_builder`'s wire format doc.
const NOREPLAY_GROUP_HEADER_BYTES: usize = 2 + 32 + 1;

/// Per-entry payload inside a group: `sequence(8) + digest(32)`.
const NOREPLAY_ENTRY_DATA_BYTES: usize = 8 + 32;

/// Exact `BackfillNoReplay` ix-data length for `entry_count` entries packed
/// into a single group (see [`NOREPLAY_GROUP_HEADER_BYTES`]).
const fn noreplay_data_bytes(entry_count: usize) -> usize {
    NOREPLAY_FIXED_DATA_HEADER_BYTES
        + NOREPLAY_GROUP_HEADER_BYTES
        + entry_count * NOREPLAY_ENTRY_DATA_BYTES
}

/// Exact serialized wire size of a `BackfillNoReplay` tx carrying
/// `bucket_count` unique noreplay buckets and `entry_count` transfer
/// entries (single-group, per `collect_transfers`'s packing invariant).
///
/// Measured once per `(bucket_count, entry_count)` through the real
/// `Message`/`Transaction` serializer on a throwaway instruction of the same
/// shape. The dummy account keys are distinct because `Message` deduplicates
/// equal keys.
fn noreplay_wire_size(bucket_count: usize, entry_count: usize) -> usize {
    const N: usize = MAX_NOREPLAY_ENTRIES_PER_BATCH + 1;
    static SIZES: OnceLock<[[usize; N]; N]> = OnceLock::new();
    assert!(
        bucket_count < N,
        "bucket count {bucket_count} over the entry cap"
    );
    assert!(
        entry_count < N,
        "entry count {entry_count} over the entry cap"
    );
    SIZES.get_or_init(|| {
        let mut sizes = [[0; N]; N];
        for (buckets, row) in sizes.iter_mut().enumerate() {
            for (entries, size) in row.iter_mut().enumerate() {
                *size = measure_noreplay_wire_size(buckets, entries);
            }
        }
        sizes
    })[bucket_count][entry_count]
}

fn measure_noreplay_wire_size(bucket_count: usize, entry_count: usize) -> usize {
    let payer = Pubkey::new_from_array([0u8; 32]);
    let dummy = |i: usize| {
        let mut key = [0u8; 32];
        key[..8].copy_from_slice(&(i as u64).to_le_bytes());
        AccountMeta::new_readonly(Pubkey::new_from_array(key), false)
    };
    let accounts = std::iter::once(AccountMeta::new(payer, true))
        .chain((1..NOREPLAY_FIXED_ACCOUNTS + bucket_count).map(dummy))
        .collect();
    let ix = Instruction {
        program_id: Pubkey::new_from_array([0xFFu8; 32]),
        accounts,
        data: vec![0u8; noreplay_data_bytes(entry_count)],
    };
    legacy_wire_size(&ix, &payer)
}

/// Maximum `BackfillBalance` entries per tx. Empirical limit from the cost
/// probe — see module docs.
pub const MAX_BALANCE_ENTRIES_PER_BATCH: usize = 8;

/// Per-tx entry budget, in bytes, for the account-list-bound instructions
/// (every kind except `BackfillNoReplay`). Each entry adds one writable PDA account-meta (32 B of
/// address-table space) plus its ix-data payload; the budget below is the
/// portion of the 1232-byte raw-tx limit left for entries after the fixed
/// overhead (signature, message header, blockhash, the payer/system/program
/// metas, and the compact-array lengths).
///
/// Anchored to the WTT cost probe: `BackfillBalance` proved safe at 8 entries
/// of `32 (meta) + 68 (data) = 100 B` each, i.e. an entry budget of `8 * 100 =
/// 800 B`. The other caps below are derived from the same 800-byte budget
/// divided by each instruction's per-entry byte cost, so they inherit the
/// probe's headroom rather than guessing fresh limits.
const ENTRY_BUDGET_BYTES: usize = MAX_BALANCE_ENTRIES_PER_BATCH * (32 + 68);

/// Account-meta cost of one PDA: a single writable key in the tx
/// address table.
const ENTRY_META_BYTES: usize = 32;

/// `BackfillModifyBalance` ix-data per entry: `BackfillModifyBalanceEntry::LEN`.
pub const MODIFY_BALANCE_ENTRY_DATA_BYTES: usize = BackfillModifyBalanceEntry::LEN;

/// `BackfillChainRegistration` ix-data per entry: `chain(2 BE) ‖ sequence(8 BE)
/// ‖ emitter(32)` = 42 B. Each entry also carries a second PDA meta.
pub const CHAIN_REGISTRATION_ENTRY_DATA_BYTES: usize = BackfillChainRegistrationEntry::LEN;

/// `BackfillTransceiverHub` ix-data per entry: `chain(2 BE) ‖ address(32) ‖
/// hub_chain(2 BE) ‖ hub_address(32)` = 68 B.
pub const TRANSCEIVER_HUB_ENTRY_DATA_BYTES: usize = BackfillTransceiverHubEntry::LEN;

/// `BackfillTransceiverPeer` ix-data per entry: `chain(2 BE) ‖ address(32) ‖
/// dest_chain(2 BE) ‖ peer_address(32)` = 68 B.
pub const TRANSCEIVER_PEER_ENTRY_DATA_BYTES: usize = BackfillTransceiverPeerEntry::LEN;

const fn cap_from_entry_bytes(per_entry_data_bytes: usize) -> usize {
    ENTRY_BUDGET_BYTES / (ENTRY_META_BYTES + per_entry_data_bytes)
}

/// Maximum `BackfillModifyBalance` entries per tx. `800 / (32 + 109) = 5`.
pub const MAX_MODIFY_BALANCE_ENTRIES_PER_BATCH: usize =
    cap_from_entry_bytes(MODIFY_BALANCE_ENTRY_DATA_BYTES);

/// Maximum `BackfillChainRegistration` entries per tx. Two PDA metas per entry:
/// `800 / (2 * 32 + 42) = 7`.
pub const MAX_CHAIN_REGISTRATION_ENTRIES_PER_BATCH: usize =
    ENTRY_BUDGET_BYTES / (2 * ENTRY_META_BYTES + CHAIN_REGISTRATION_ENTRY_DATA_BYTES);

// Program-side ceilings: 63 PDA creates per batch, 31 for two creates per entry.
const _: () = assert!(MAX_BALANCE_ENTRIES_PER_BATCH <= MAX_BATCH_ENTRIES as usize);
const _: () = assert!(MAX_MODIFY_BALANCE_ENTRIES_PER_BATCH <= MAX_BATCH_ENTRIES as usize);
const _: () =
    assert!(MAX_CHAIN_REGISTRATION_ENTRIES_PER_BATCH <= MAX_CHAIN_REGISTRATION_ENTRIES as usize);
const _: () = assert!(MAX_MODIFY_BALANCE_ENTRIES_PER_BATCH == 5);
const _: () = assert!(MAX_CHAIN_REGISTRATION_ENTRIES_PER_BATCH == 7);

/// Maximum `BackfillTransceiverHub` entries per tx. `800 / (32 + 68) = 8`.
pub const MAX_TRANSCEIVER_HUB_ENTRIES_PER_BATCH: usize =
    cap_from_entry_bytes(TRANSCEIVER_HUB_ENTRY_DATA_BYTES);

/// Maximum `BackfillTransceiverPeer` entries per tx. `800 / (32 + 68) = 8`.
pub const MAX_TRANSCEIVER_PEER_ENTRIES_PER_BATCH: usize =
    cap_from_entry_bytes(TRANSCEIVER_PEER_ENTRY_DATA_BYTES);

/// The rows of one transaction. The submitter sends each batch as a single
/// Solana transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxBatch {
    /// 1..=18 same-`(chain, emitter)` transfers. Built into a single
    /// emitter-grouped `BackfillNoReplay` ix.
    BackfillNoReplay(Vec<TransferRow>),
    /// 1..=8 balance entries. Built into a single `BackfillBalance` ix.
    BackfillBalance(Vec<BalanceRow>),
    /// 1..=5 modification records, strictly ascending by `sequence`. Built
    /// into a single `BackfillModifyBalance` ix.
    BackfillModifyBalance(Vec<ModifyBalanceRow>),
    /// 1..=7 chain registrations (Token Bridge on WTT, Standard Relayer on
    /// NTT), strictly ascending by `chain`. Built into a single
    /// `BackfillChainRegistration` / `BackfillRelayerChainRegistration` ix.
    BackfillChainRegistration(Vec<ChainRegistrationRow>),
    /// 1..=8 transceiver-hub mappings, strictly ascending by `(chain,
    /// address)`. Built into a single `BackfillTransceiverHub` ix.
    BackfillTransceiverHub(Vec<TransceiverHubRow>),
    /// 1..=8 transceiver-peer mappings, strictly ascending by `(chain,
    /// address, dest_chain)`. Built into a single `BackfillTransceiverPeer` ix.
    BackfillTransceiverPeer(Vec<TransceiverPeerRow>),
}

impl TxBatch {
    /// Entry count.
    pub fn len(&self) -> usize {
        match self {
            Self::BackfillNoReplay(e) => e.len(),
            Self::BackfillBalance(e) => e.len(),
            Self::BackfillModifyBalance(e) => e.len(),
            Self::BackfillChainRegistration(e) => e.len(),
            Self::BackfillTransceiverHub(e) => e.len(),
            Self::BackfillTransceiverPeer(e) => e.len(),
        }
    }

    /// Never true for a batch the [`Batcher`] emits.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn ix_name(&self) -> &'static str {
        match self {
            Self::BackfillNoReplay(_) => "BackfillNoReplay",
            Self::BackfillBalance(_) => "BackfillBalance",
            Self::BackfillModifyBalance(_) => "BackfillModifyBalance",
            Self::BackfillChainRegistration(_) => "BackfillChainRegistration",
            Self::BackfillTransceiverHub(_) => "BackfillTransceiverHub",
            Self::BackfillTransceiverPeer(_) => "BackfillTransceiverPeer",
        }
    }
}

/// Longest buffered run (one row kind). Mainnet: 17,367 `account` rows.
pub const MAX_BUFFERED_RUN_ROWS: usize = 100_000;

/// Per-kind key order across the whole plan. Each handler requires strictly
/// ascending keys within one batch; this extends the rule across batch
/// boundaries, so a duplicate or out-of-order row fails before any send.
/// `RegisterChain` sequences must also be unique.
pub fn check_batch_order(batches: &[TxBatch]) -> Result<()> {
    #[derive(Default)]
    struct Last {
        noreplay: Option<(EmitterKey, Sequence)>,
        balance: Option<BalanceKey>,
        modify: Option<Sequence>,
        registration: Option<ChainId>,
        hub: Option<(ChainId, Address)>,
        peer: Option<(ChainId, Address, ChainId)>,
    }
    fn ascend<K: Ord + Copy + std::fmt::Debug>(
        last: &mut Option<K>,
        key: K,
        batch: usize,
        kind: &str,
    ) -> Result<()> {
        if let Some(previous) = *last {
            if key <= previous {
                bail!("batch {batch} ({kind}): key {key:?} is not above {previous:?}");
            }
        }
        *last = Some(key);
        Ok(())
    }
    let mut last = Last::default();
    let mut register_chain_sequences = HashSet::new();
    for (i, batch) in batches.iter().enumerate() {
        let kind = batch.ix_name();
        match batch {
            TxBatch::BackfillNoReplay(e) => e.iter().try_for_each(|t| {
                ascend(&mut last.noreplay, (t.emitter_key(), t.sequence), i, kind)
            })?,
            TxBatch::BackfillBalance(e) => e
                .iter()
                .try_for_each(|a| ascend(&mut last.balance, a.key(), i, kind))?,
            TxBatch::BackfillModifyBalance(e) => e
                .iter()
                .try_for_each(|m| ascend(&mut last.modify, m.sequence, i, kind))?,
            TxBatch::BackfillChainRegistration(e) => {
                for r in e {
                    ascend(&mut last.registration, r.chain, i, kind)?;
                    if !register_chain_sequences.insert(r.sequence) {
                        bail!(
                            "batch {i} ({kind}): RegisterChain sequence {} repeats",
                            r.sequence
                        );
                    }
                }
            }
            TxBatch::BackfillTransceiverHub(e) => e
                .iter()
                .try_for_each(|h| ascend(&mut last.hub, (h.chain, h.address), i, kind))?,
            TxBatch::BackfillTransceiverPeer(e) => e.iter().try_for_each(|p| {
                ascend(&mut last.peer, (p.chain, p.address, p.dest_chain), i, kind)
            })?,
        }
    }
    Ok(())
}

/// Streaming batcher. Wraps any `Iterator<Item = SnapshotRow>` and yields
/// [`TxBatch`]s lazily. Transfers stream; the small sorted kinds buffer one run.
pub struct Batcher<I: Iterator<Item = SnapshotRow>> {
    source: Peekable<I>,
    /// Sorted, not-yet-emitted batches of the current buffered run. Every
    /// handler except `BackfillNoReplay` requires a strictly ascending batch,
    /// and the snapshot order is not always handler order (`account` rows sort
    /// `token_address` before `token_chain`).
    /// Bounded by the run length: 17,367 accounts, tens of the others.
    buffered: VecDeque<TxBatch>,
}

impl<I: Iterator<Item = SnapshotRow>> Batcher<I> {
    pub fn new(source: I) -> Self {
        Self {
            source: source.peekable(),
            buffered: VecDeque::new(),
        }
    }
}

impl<I: Iterator<Item = SnapshotRow>> Iterator for Batcher<I> {
    type Item = TxBatch;

    fn next(&mut self) -> Option<TxBatch> {
        // Bounded: each pass consumes one row or returns.
        loop {
            if let Some(batch) = self.buffered.pop_front() {
                return Some(batch);
            }
            match self.source.next()? {
                SnapshotRow::Transfer(t) => {
                    return Some(TxBatch::BackfillNoReplay(self.collect_transfers(t)))
                }
                SnapshotRow::Governance(_) => continue,
                SnapshotRow::Balance(first) => {
                    let run = drain_run!(self, first, SnapshotRow::Balance);
                    self.buffer_sorted(
                        run,
                        BalanceRow::key,
                        MAX_BALANCE_ENTRIES_PER_BATCH,
                        TxBatch::BackfillBalance,
                    );
                }
                SnapshotRow::ModifyBalance(first) => {
                    let run = drain_run!(self, first, SnapshotRow::ModifyBalance);
                    self.buffer_sorted(
                        run,
                        |m| m.sequence,
                        MAX_MODIFY_BALANCE_ENTRIES_PER_BATCH,
                        TxBatch::BackfillModifyBalance,
                    );
                }
                SnapshotRow::TokenBridgeRegistration(first) => {
                    let run = drain_run!(self, first, SnapshotRow::TokenBridgeRegistration);
                    self.buffer_registrations(run);
                }
                SnapshotRow::RelayerChainRegistration(first) => {
                    let run = drain_run!(self, first, SnapshotRow::RelayerChainRegistration);
                    self.buffer_registrations(run);
                }
                SnapshotRow::TransceiverHub(first) => {
                    let run = drain_run!(self, first, SnapshotRow::TransceiverHub);
                    self.buffer_sorted(
                        run,
                        |h| (h.chain, h.address),
                        MAX_TRANSCEIVER_HUB_ENTRIES_PER_BATCH,
                        TxBatch::BackfillTransceiverHub,
                    );
                }
                SnapshotRow::TransceiverPeer(first) => {
                    let run = drain_run!(self, first, SnapshotRow::TransceiverPeer);
                    self.buffer_sorted(
                        run,
                        |p| (p.chain, p.address, p.dest_chain),
                        MAX_TRANSCEIVER_PEER_ENTRIES_PER_BATCH,
                        TxBatch::BackfillTransceiverPeer,
                    );
                }
            }
        }
    }
}

/// `first` plus every directly following `$variant` record. Panics past
/// [`MAX_BUFFERED_RUN_ROWS`].
macro_rules! drain_run {
    ($self:ident, $first:ident, $variant:path) => {{
        let mut run = vec![$first];
        while let Some($variant(next)) = $self.source.next_if(|r| matches!(r, $variant(_))) {
            assert!(
                run.len() < MAX_BUFFERED_RUN_ROWS,
                "sorted run exceeds MAX_BUFFERED_RUN_ROWS ({MAX_BUFFERED_RUN_ROWS})"
            );
            run.push(next);
        }
        run
    }};
}
use drain_run;

impl<I: Iterator<Item = SnapshotRow>> Batcher<I> {
    /// Greedy-pack consecutive same-`(chain, emitter)` transfers, bounded by
    /// BOTH the `BackfillNoReplay` entry-count cap AND — the primary,
    /// correctness-critical bound — the real Solana packet-size limit.
    ///
    /// Entries from one emitter can land in different 1024-sequence buckets,
    /// and each bucket adds a 32-byte account meta. An entry is packed only
    /// if the batch including it still serializes within [`PACKET_DATA_SIZE`]
    /// ([`noreplay_wire_size`]). Sequences ascend within a run, so a new
    /// bucket is a change of bucket index.
    fn collect_transfers(&mut self, first: TransferRow) -> Vec<TransferRow> {
        let key = first.emitter_key();
        let mut last_bucket = NoReplayBitmapAccount::bucket_index(first.sequence.0);
        let mut bucket_count = 1;
        let mut batch = Vec::with_capacity(MAX_NOREPLAY_ENTRIES_PER_BATCH);
        batch.push(first);
        while batch.len() < MAX_NOREPLAY_ENTRIES_PER_BATCH {
            let entries = batch.len() + 1;
            let fits = |t: &TransferRow| {
                let bucket = NoReplayBitmapAccount::bucket_index(t.sequence.0);
                let buckets = bucket_count + usize::from(bucket != last_bucket);
                t.emitter_key() == key && noreplay_wire_size(buckets, entries) <= PACKET_DATA_SIZE
            };
            let Some(SnapshotRow::Transfer(next)) = self
                .source
                .next_if(|r| matches!(r, SnapshotRow::Transfer(t) if fits(t)))
            else {
                break;
            };
            let bucket = NoReplayBitmapAccount::bucket_index(next.sequence.0);
            if bucket != last_bucket {
                bucket_count += 1;
                last_bucket = bucket;
            }
            batch.push(next);
        }
        batch
    }

    /// Sort a run into handler order and queue it in batches of at most `cap`.
    fn buffer_sorted<T, K: Ord>(
        &mut self,
        mut run: Vec<T>,
        key: impl Fn(&T) -> K,
        cap: usize,
        into_batch: fn(Vec<T>) -> TxBatch,
    ) {
        assert!(cap > 0, "batch cap must be positive");
        run.sort_by_key(key);
        let mut run = run.into_iter().peekable();
        while run.peek().is_some() {
            self.buffered
                .push_back(into_batch(run.by_ref().take(cap).collect()));
        }
    }

    fn buffer_registrations(&mut self, run: Vec<ChainRegistrationRow>) {
        self.buffer_sorted(
            run,
            |r| r.chain,
            MAX_CHAIN_REGISTRATION_ENTRIES_PER_BATCH,
            TxBatch::BackfillChainRegistration,
        );
    }
}
