//! [`ChainState`]: the one shape both sides produce, the expected state from
//! the snapshot and the state read from Solana.
//!
//! Each account-backed [`ComparedState`] holds rows of match key → compared
//! bytes, both from [`ComparedAccount`]: the key is the PDA seeds after the
//! prefix, the bytes are the full account (padding included), or the 32-byte
//! balance for Balances. Replay marks are held per NoReplay bucket.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use global_accountant_definitions::{
    NoReplayBitmapAccount, NOREPLAY_BITMAP_BYTES, NOREPLAY_BITS_PER_BUCKET,
};

use crate::compared_account::ComparedAccount;

pub use ga_backfill::noreplay::BucketId;

/// Replay keys: 5,516,669 on mainnet.
pub const MAX_REPLAY_KEYS: usize = 16_000_000;
/// NoReplay buckets: about 50k on mainnet.
pub const MAX_BUCKETS: usize = 500_000;
/// Rows per account-backed state: 17,367 balances on mainnet, tens for the others.
pub const MAX_KEYED_ROWS: usize = 100_000;

impl std::fmt::Display for ComparedState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// One kind of state a check compares between wormchain and Solana, or
/// reports from Solana alone (`MalformedAccounts`, `PendingObservations`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparedState {
    Balances,
    ReplayMarks,
    ModifyBalanceRecords,
    ChainRegistrations,
    RegisterChainRecords,
    TransceiverHubs,
    TransceiverPeers,
    /// Program accounts that fit no layout: unknown tag, wrong size, non-canonical address.
    MalformedAccounts,
    /// `PendingObservations` accounts. The gate requires exactly 0.
    PendingObservations,
}

impl ComparedState {
    pub const ACCOUNT_BACKED: [ComparedState; 6] = [
        ComparedState::Balances,
        ComparedState::ModifyBalanceRecords,
        ComparedState::ChainRegistrations,
        ComparedState::RegisterChainRecords,
        ComparedState::TransceiverHubs,
        ComparedState::TransceiverPeers,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Balances => "balances",
            Self::ReplayMarks => "replay_marks",
            Self::ModifyBalanceRecords => "modify_balance_records",
            Self::ChainRegistrations => "chain_registrations",
            Self::RegisterChainRecords => "register_chain_records",
            Self::TransceiverHubs => "transceiver_hubs",
            Self::TransceiverPeers => "transceiver_peers",
            Self::MalformedAccounts => "malformed_accounts",
            Self::PendingObservations => "pending_observations",
        }
    }

    /// Match-key length of an account-backed state.
    pub fn key_len(self) -> usize {
        match self {
            Self::Balances | Self::TransceiverPeers => 36,
            Self::ModifyBalanceRecords | Self::RegisterChainRecords => 8,
            Self::ChainRegistrations => 2,
            Self::TransceiverHubs => 34,
            Self::ReplayMarks | Self::MalformedAccounts | Self::PendingObservations => {
                unreachable!("{self:?} is not keyed")
            }
        }
    }

    /// Human-readable form of a match key.
    pub fn describe_key(self, key: &[u8]) -> String {
        let u16_at = |i: usize| u16::from_be_bytes([key[i], key[i + 1]]);
        let u64_at = |i: usize| u64::from_be_bytes(key[i..i + 8].try_into().expect("8 bytes"));
        let hex_at = |i: usize| format!("0x{}", hex::encode(&key[i..i + 32]));
        match (self, key.len()) {
            (Self::Balances, 36) => format!(
                "chain={} token_chain={} token={}",
                u16_at(0),
                u16_at(2),
                hex_at(4)
            ),
            (Self::ModifyBalanceRecords | Self::RegisterChainRecords, 8) => {
                format!("sequence={}", u64_at(0))
            }
            (Self::ChainRegistrations, 2) => format!("chain={}", u16_at(0)),
            (Self::TransceiverHubs, 34) => format!("chain={} address={}", u16_at(0), hex_at(2)),
            (Self::TransceiverPeers, 36) => format!(
                "chain={} address={} dest_chain={}",
                u16_at(0),
                hex_at(2),
                u16_at(34)
            ),
            _ => format!("0x{}", hex::encode(key)),
        }
    }
}

/// `(emitter_chain, emitter_address, sequence)`.
pub type MessageId = (u16, [u8; 32], u64);

/// One 1024-bit NoReplay bucket, bit layout as `NoReplayBitmapAccount`.
pub type BucketBits = [u8; NOREPLAY_BITMAP_BYTES];

/// Replay marks as one mask per bucket. Every stored mask has at least one bit set.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReplayMarks {
    buckets: BTreeMap<BucketId, BucketBits>,
    count: usize,
}

impl ReplayMarks {
    /// Mark `key`. Fails on a duplicate or when a bound is hit.
    pub fn insert(&mut self, (chain, emitter, sequence): MessageId) -> Result<()> {
        let bucket = (
            chain,
            emitter,
            NoReplayBitmapAccount::bucket_index(sequence),
        );
        let bit = NoReplayBitmapAccount::bit_index(sequence);
        if !self.buckets.contains_key(&bucket) && self.buckets.len() == MAX_BUCKETS {
            bail!("replay set exceeds MAX_BUCKETS ({MAX_BUCKETS})");
        }
        if self.count == MAX_REPLAY_KEYS {
            bail!("replay set exceeds MAX_REPLAY_KEYS ({MAX_REPLAY_KEYS})");
        }
        let mask = self
            .buckets
            .entry(bucket)
            .or_insert([0; NOREPLAY_BITMAP_BYTES]);
        if is_set(mask, bit) {
            bail!(
                "duplicate replay mark chain={chain} emitter=0x{} sequence={sequence}",
                hex::encode(emitter)
            );
        }
        mask[bit / 8] |= 1 << (bit % 8);
        self.count += 1;
        Ok(())
    }

    /// Store a fetched bucket. An all-zero mask is dropped, so an empty bucket
    /// account equals an absent one.
    pub fn insert_bucket(&mut self, bucket: BucketId, mask: BucketBits) -> Result<()> {
        let bits = popcount(&mask);
        if bits == 0 {
            return Ok(());
        }
        if self.buckets.len() == MAX_BUCKETS {
            bail!("replay set exceeds MAX_BUCKETS ({MAX_BUCKETS})");
        }
        if self.count + bits > MAX_REPLAY_KEYS {
            bail!("replay set exceeds MAX_REPLAY_KEYS ({MAX_REPLAY_KEYS})");
        }
        if self.buckets.insert(bucket, mask).is_some() {
            bail!("bucket {bucket:?} inserted twice");
        }
        self.count += bits;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn buckets(&self) -> &BTreeMap<BucketId, BucketBits> {
        &self.buckets
    }

    /// Marked keys in `(chain, emitter, sequence)` order.
    pub fn iter(&self) -> impl Iterator<Item = MessageId> + '_ {
        self.buckets
            .iter()
            .flat_map(|(bucket, mask)| marked_messages(bucket, mask))
    }

    /// SHA-256 over `count_be ‖ (chain_be ‖ emitter ‖ sequence_be)*` in key order.
    pub fn hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update((self.count as u64).to_be_bytes());
        for (chain, emitter, sequence) in self.iter() {
            hasher.update(chain.to_be_bytes());
            hasher.update(emitter);
            hasher.update(sequence.to_be_bytes());
        }
        hasher.finalize().into()
    }
}

/// Keys whose bit is set in `mask`, ascending.
pub fn marked_messages<'a>(
    bucket: &'a BucketId,
    mask: &'a BucketBits,
) -> impl Iterator<Item = MessageId> + 'a {
    (0..NOREPLAY_BITS_PER_BUCKET).filter_map(move |bit| {
        is_set(mask, bit as usize).then_some((
            bucket.0,
            bucket.1,
            bucket.2 * NOREPLAY_BITS_PER_BUCKET + bit,
        ))
    })
}

/// Bit layout of `NoReplayBitmapAccount::is_marked`: little-endian within each byte.
fn is_set(mask: &BucketBits, bit: usize) -> bool {
    mask[bit / 8] & (1 << (bit % 8)) != 0
}

fn popcount(mask: &BucketBits) -> usize {
    mask.iter().map(|b| b.count_ones() as usize).sum()
}

/// Rows of one account-backed state: match key → compared bytes.
pub type RowsByKey = BTreeMap<Vec<u8>, Vec<u8>>;

/// One chain's accountant state, keyed and ordered deterministically: the
/// expected state from the snapshot, or the state read from Solana.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ChainState {
    pub keyed: BTreeMap<ComparedState, RowsByKey>,
    pub replay: ReplayMarks,
}

impl ChainState {
    pub fn rows(&self, kind: ComparedState) -> &RowsByKey {
        static EMPTY: RowsByKey = BTreeMap::new();
        self.keyed.get(&kind).unwrap_or(&EMPTY)
    }

    /// Insert one row. Fails on a duplicate key or when the bound is hit.
    pub fn insert(&mut self, kind: ComparedState, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        assert!(
            ComparedState::ACCOUNT_BACKED.contains(&kind),
            "{kind:?} is not keyed"
        );
        assert_eq!(key.len(), kind.key_len(), "{kind:?} key length");
        let rows = self.keyed.entry(kind).or_default();
        if rows.len() == MAX_KEYED_ROWS {
            bail!("{} exceeds MAX_KEYED_ROWS ({MAX_KEYED_ROWS})", kind.name());
        }
        let rendered = kind.describe_key(&key);
        if rows.insert(key, value).is_some() {
            bail!("two {} rows for {rendered}", kind.name());
        }
        Ok(())
    }

    /// Insert one account under its compared state's match key and compared bytes.
    pub fn insert_account<L: ComparedAccount>(&mut self, layout: &L) -> Result<()> {
        self.insert(L::STATE, layout.match_key(), layout.compared_bytes())
    }

    /// SHA-256 over `count_be ‖ (key ‖ value)*` in key order. For Balances
    /// this is `count ‖ (chain_be ‖ token_chain_be ‖ token_address ‖ balance)*`.
    pub fn state_hash(&self, kind: ComparedState) -> [u8; 32] {
        if kind == ComparedState::ReplayMarks {
            return self.replay.hash();
        }
        let rows = self.rows(kind);
        let mut hasher = Sha256::new();
        hasher.update((rows.len() as u64).to_be_bytes());
        for (key, value) in rows {
            hasher.update(key);
            hasher.update(value);
        }
        hasher.finalize().into()
    }

    pub fn state_len(&self, kind: ComparedState) -> usize {
        if kind == ComparedState::ReplayMarks {
            return self.replay.len();
        }
        self.rows(kind).len()
    }
}

pub fn hex32(bytes: &[u8; 32]) -> String {
    format!("0x{}", hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(balance: u8, sequence: u64) -> ChainState {
        let mut state = ChainState::default();
        let mut key = vec![0, 1, 0, 2];
        key.extend_from_slice(&[0xAA; 32]);
        state
            .insert(ComparedState::Balances, key, vec![balance; 32])
            .unwrap();
        state.replay.insert((2, [0xBB; 32], sequence)).unwrap();
        state
    }

    #[test]
    fn hashes_are_deterministic_and_key_sensitive() {
        let base = state_with(1, 5);
        let hash = |s: &ChainState, f| s.state_hash(f);
        assert_eq!(
            hash(&base, ComparedState::Balances),
            hash(&state_with(1, 5), ComparedState::Balances)
        );
        assert_eq!(
            hash(&base, ComparedState::ReplayMarks),
            hash(&state_with(1, 5), ComparedState::ReplayMarks)
        );

        assert_ne!(
            hash(&base, ComparedState::Balances),
            hash(&state_with(2, 5), ComparedState::Balances)
        );
        assert_ne!(
            hash(&base, ComparedState::ReplayMarks),
            hash(&state_with(1, 6), ComparedState::ReplayMarks)
        );

        let empty = ChainState::default();
        assert_ne!(
            hash(&base, ComparedState::Balances),
            hash(&empty, ComparedState::Balances)
        );
        assert_ne!(
            hash(&base, ComparedState::ReplayMarks),
            hash(&empty, ComparedState::ReplayMarks)
        );
    }

    #[test]
    fn insertion_order_does_not_change_hashes() {
        let mut forward = ReplayMarks::default();
        let mut backward = ReplayMarks::default();
        for sequence in [1u64, 2, 3] {
            forward.insert((1, [0x11; 32], sequence)).unwrap();
        }
        for sequence in [3u64, 2, 1] {
            backward.insert((1, [0x11; 32], sequence)).unwrap();
        }
        assert_eq!(forward.hash(), backward.hash());
    }
}
