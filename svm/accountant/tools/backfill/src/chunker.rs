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
    AccountRecord, ModificationRecord, Record, RegistrationRecord, TransferRecord,
};

/// Maximum `BackfillNoReplay` entries per tx. Empirical limit from the
/// at-scale probe — see module docs.
pub const MAX_NOREPLAY_ENTRIES_PER_CHUNK: usize = 18;

/// Maximum `BackfillBalance` entries per tx. Empirical limit from the cost
/// probe — see module docs.
pub const MAX_BALANCE_ENTRIES_PER_CHUNK: usize = 8;

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
}
