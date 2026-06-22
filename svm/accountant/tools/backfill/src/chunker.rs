//! Group catalogue records into tx-sized batches for the backfill program.
//!
//! ## Wire budget primer
//!
//! Solana's 1232-byte tx wire limit caps how many entries fit in one ix.
//! Empirically validated at 100k-transfer scale via
//! `programs/global-accountant-backfill/tests/surfpool_e2e_cost_probe_at_scale.rs`:
//!
//! - **BackfillNoReplay**: 18 same-emitter entries per tx, safe across the 2-3
//!   bucket-span cases. Chunks larger than that risk wire overflow when the
//!   batch straddles a 1024-sequence bucket boundary.
//! - **BackfillBalance**: 8 entries per tx. Each entry adds one writable
//!   account meta to the account list — the binding constraint here is the
//!   account-list size, not the data.
//!
//! ## Chunking strategy
//!
//! For a sorted catalogue (which workstream A guarantees), the chunker:
//!
//! 1. Walks records in source order.
//! 2. Closes the current chunk on any of three transitions:
//!    a. Kind changes (account → transfer etc.)
//!    b. Emitter changes (within a transfer run)
//!    c. Chunk hits its kind-specific size cap
//! 3. Yields `Modification` / `Registration` records as single-record
//!    `Deferred*` chunks — they require the operational program (Phase 7) and
//!    are passed through here without packing.

use std::iter::Peekable;

use crate::catalogue::{
    AccountRecord, ModificationRecord, Record, RegistrationRecord, RelayerChainRegistrationRecord,
    TransceiverHubRecord, TransceiverPeerRecord, TransferRecord,
};

/// Maximum `BackfillNoReplay` entries per tx. Empirical limit from the
/// at-scale probe — see module docs.
pub const MAX_NOREPLAY_ENTRIES_PER_CHUNK: usize = 18;

/// Maximum `BackfillBalance` entries per tx. Empirical limit from the cost
/// probe — see module docs.
pub const MAX_BALANCE_ENTRIES_PER_CHUNK: usize = 8;

/// Per-tx entry budget, in bytes, for the account-list-bound NTT map
/// instructions. Each map entry adds one writable PDA account-meta (32 B of
/// address-table space) plus its ix-data payload; the budget below is the
/// portion of the 1232-byte raw-tx limit left for entries after the fixed
/// overhead (signature, message header, blockhash, the payer/system/program
/// metas, and the compact-array lengths).
///
/// Anchored to the WTT cost probe: `BackfillBalance` proved safe at 8 entries
/// of `32 (meta) + 68 (data) = 100 B` each, i.e. an entry budget of `8 * 100 =
/// 800 B`. The NTT map caps below are derived from the same 800-byte budget
/// divided by each instruction's per-entry byte cost, so they inherit the
/// probe's headroom rather than guessing fresh limits.
const NTT_ENTRY_BUDGET_BYTES: usize = MAX_BALANCE_ENTRIES_PER_CHUNK * (32 + 68);

/// Account-meta cost of one map entry: a single writable PDA key in the tx
/// address table.
const ENTRY_META_BYTES: usize = 32;

/// `BackfillRelayerRegistration` ix-data per entry: `chain(2 BE) ‖
/// emitter_address(32)` = 34 B. Mirrors the program handler's `ENTRY_BYTES`.
pub const RELAYER_REGISTRATION_ENTRY_DATA_BYTES: usize = 2 + 32;

/// `BackfillTransceiverHub` ix-data per entry: `chain(2 BE) ‖ address(32) ‖
/// hub_chain(2 BE) ‖ hub_address(32)` = 68 B.
pub const TRANSCEIVER_HUB_ENTRY_DATA_BYTES: usize = 2 + 32 + 2 + 32;

/// `BackfillTransceiverPeer` ix-data per entry: `chain(2 BE) ‖ address(32) ‖
/// dest_chain(2 BE) ‖ peer_address(32)` = 68 B.
pub const TRANSCEIVER_PEER_ENTRY_DATA_BYTES: usize = 2 + 32 + 2 + 32;

const fn cap_from_entry_bytes(per_entry_data_bytes: usize) -> usize {
    NTT_ENTRY_BUDGET_BYTES / (ENTRY_META_BYTES + per_entry_data_bytes)
}

/// Maximum `BackfillRelayerRegistration` entries per tx. `800 / (32 + 34) = 12`.
pub const MAX_RELAYER_REGISTRATION_ENTRIES_PER_CHUNK: usize =
    cap_from_entry_bytes(RELAYER_REGISTRATION_ENTRY_DATA_BYTES);

/// Maximum `BackfillTransceiverHub` entries per tx. `800 / (32 + 68) = 8`.
pub const MAX_TRANSCEIVER_HUB_ENTRIES_PER_CHUNK: usize =
    cap_from_entry_bytes(TRANSCEIVER_HUB_ENTRY_DATA_BYTES);

/// Maximum `BackfillTransceiverPeer` entries per tx. `800 / (32 + 68) = 8`.
pub const MAX_TRANSCEIVER_PEER_ENTRIES_PER_CHUNK: usize =
    cap_from_entry_bytes(TRANSCEIVER_PEER_ENTRY_DATA_BYTES);

/// One tx-shaped unit of work. The submitter (Phase 4) maps each plan to a
/// single Solana transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkPlan {
    /// 1..=18 same-`(chain, emitter)` transfers. Built into a single
    /// emitter-grouped `BackfillNoReplay` ix.
    BackfillNoReplay(Vec<TransferRecord>),
    /// 1..=8 balance entries. Built into a single `BackfillBalance` ix.
    BackfillBalance(Vec<AccountRecord>),
    /// Pass-through; replayed via the operational program's `ModifyBalance`
    /// ix post-upgrade (Phase 7).
    DeferredModification(ModificationRecord),
    /// Pass-through; replayed via the operational program's `RegisterChain`
    /// ix post-upgrade (Phase 7).
    DeferredRegistration(RegistrationRecord),
    /// 1..=12 relayer chain registrations, strictly ascending by `chain`.
    /// Built into a single `BackfillRelayerRegistration` ix.
    BackfillRelayerRegistration(Vec<RelayerChainRegistrationRecord>),
    /// 1..=8 transceiver-hub mappings, strictly ascending by `(chain,
    /// address)`. Built into a single `BackfillTransceiverHub` ix.
    BackfillTransceiverHub(Vec<TransceiverHubRecord>),
    /// 1..=8 transceiver-peer mappings, strictly ascending by `(chain,
    /// address, dest_chain)`. Built into a single `BackfillTransceiverPeer` ix.
    BackfillTransceiverPeer(Vec<TransceiverPeerRecord>),
}

/// Streaming chunker. Wraps any `Iterator<Item = Record>` and yields
/// [`ChunkPlan`]s lazily — bounded memory, never materialises the full
/// catalogue.
pub struct Chunker<I: Iterator<Item = Record>> {
    source: Peekable<I>,
}

impl<I: Iterator<Item = Record>> Chunker<I> {
    pub fn new(source: I) -> Self {
        Self {
            source: source.peekable(),
        }
    }
}

impl<I: Iterator<Item = Record>> Iterator for Chunker<I> {
    type Item = ChunkPlan;

    fn next(&mut self) -> Option<ChunkPlan> {
        let first = self.source.next()?;
        match first {
            Record::Transfer(t) => Some(ChunkPlan::BackfillNoReplay(self.collect_transfers(t))),
            Record::Account(a) => Some(ChunkPlan::BackfillBalance(self.collect_accounts(a))),
            Record::Modification(m) => Some(ChunkPlan::DeferredModification(m)),
            Record::Registration(r) => Some(ChunkPlan::DeferredRegistration(r)),
            Record::RelayerChainRegistration(r) => Some(ChunkPlan::BackfillRelayerRegistration(
                self.collect_relayer_registrations(r),
            )),
            Record::TransceiverHub(h) => Some(ChunkPlan::BackfillTransceiverHub(
                self.collect_transceiver_hubs(h),
            )),
            Record::TransceiverPeer(p) => Some(ChunkPlan::BackfillTransceiverPeer(
                self.collect_transceiver_peers(p),
            )),
        }
    }
}

impl<I: Iterator<Item = Record>> Chunker<I> {
    /// Greedy-pack consecutive same-`(chain, emitter)` transfers up to the
    /// `BackfillNoReplay` cap.
    fn collect_transfers(&mut self, first: TransferRecord) -> Vec<TransferRecord> {
        let key = (first.chain, first.emitter);
        let mut chunk = Vec::with_capacity(MAX_NOREPLAY_ENTRIES_PER_CHUNK);
        chunk.push(first);
        while chunk.len() < MAX_NOREPLAY_ENTRIES_PER_CHUNK {
            let take = matches!(
                self.source.peek(),
                Some(Record::Transfer(next)) if (next.chain, next.emitter) == key
            );
            if !take {
                break;
            }
            match self.source.next() {
                Some(Record::Transfer(t)) => chunk.push(t),
                _ => unreachable!("peek confirmed Transfer"),
            }
        }
        chunk
    }

    /// Greedy-pack consecutive `Account` records up to the `BackfillBalance`
    /// cap. No further grouping constraint — each balance PDA is unique.
    fn collect_accounts(&mut self, first: AccountRecord) -> Vec<AccountRecord> {
        let mut chunk = Vec::with_capacity(MAX_BALANCE_ENTRIES_PER_CHUNK);
        chunk.push(first);
        while chunk.len() < MAX_BALANCE_ENTRIES_PER_CHUNK {
            let take = matches!(self.source.peek(), Some(Record::Account(_)));
            if !take {
                break;
            }
            match self.source.next() {
                Some(Record::Account(a)) => chunk.push(a),
                _ => unreachable!("peek confirmed Account"),
            }
        }
        chunk
    }

    /// Greedy-pack consecutive relayer-chain-registration records up to the
    /// cap. The batch must be strictly ascending by `chain` (the program
    /// rejects equal/descending keys), so a non-increasing successor closes
    /// the current chunk rather than being packed.
    fn collect_relayer_registrations(
        &mut self,
        first: RelayerChainRegistrationRecord,
    ) -> Vec<RelayerChainRegistrationRecord> {
        let mut chunk = Vec::with_capacity(MAX_RELAYER_REGISTRATION_ENTRIES_PER_CHUNK);
        let mut last_key = first.chain;
        chunk.push(first);
        while chunk.len() < MAX_RELAYER_REGISTRATION_ENTRIES_PER_CHUNK {
            let take = matches!(
                self.source.peek(),
                Some(Record::RelayerChainRegistration(next)) if next.chain > last_key
            );
            if !take {
                break;
            }
            match self.source.next() {
                Some(Record::RelayerChainRegistration(r)) => {
                    last_key = r.chain;
                    chunk.push(r);
                }
                _ => unreachable!("peek confirmed RelayerChainRegistration"),
            }
        }
        chunk
    }

    /// Greedy-pack consecutive transceiver-hub records up to the cap, requiring
    /// strict-ascending `(chain, address)` order.
    fn collect_transceiver_hubs(
        &mut self,
        first: TransceiverHubRecord,
    ) -> Vec<TransceiverHubRecord> {
        let mut chunk = Vec::with_capacity(MAX_TRANSCEIVER_HUB_ENTRIES_PER_CHUNK);
        let mut last_key = (first.chain, first.address);
        chunk.push(first);
        while chunk.len() < MAX_TRANSCEIVER_HUB_ENTRIES_PER_CHUNK {
            let take = matches!(
                self.source.peek(),
                Some(Record::TransceiverHub(next)) if (next.chain, next.address) > last_key
            );
            if !take {
                break;
            }
            match self.source.next() {
                Some(Record::TransceiverHub(h)) => {
                    last_key = (h.chain, h.address);
                    chunk.push(h);
                }
                _ => unreachable!("peek confirmed TransceiverHub"),
            }
        }
        chunk
    }

    /// Greedy-pack consecutive transceiver-peer records up to the cap,
    /// requiring strict-ascending `(chain, address, dest_chain)` order.
    fn collect_transceiver_peers(
        &mut self,
        first: TransceiverPeerRecord,
    ) -> Vec<TransceiverPeerRecord> {
        let mut chunk = Vec::with_capacity(MAX_TRANSCEIVER_PEER_ENTRIES_PER_CHUNK);
        let mut last_key = (first.chain, first.address, first.dest_chain);
        chunk.push(first);
        while chunk.len() < MAX_TRANSCEIVER_PEER_ENTRIES_PER_CHUNK {
            let take = matches!(
                self.source.peek(),
                Some(Record::TransceiverPeer(next))
                    if (next.chain, next.address, next.dest_chain) > last_key
            );
            if !take {
                break;
            }
            match self.source.next() {
                Some(Record::TransceiverPeer(p)) => {
                    last_key = (p.chain, p.address, p.dest_chain);
                    chunk.push(p);
                }
                _ => unreachable!("peek confirmed TransceiverPeer"),
            }
        }
        chunk
    }
}
