//! [`ChainState`]: the one shape both sides produce, the expected state from
//! the snapshot and the actual state read from Solana.
//!
//! Each [`KeyedKind`] holds rows of match key → compared bytes, both from
//! [`ComparedAccount`]: the key is the PDA seeds after the prefix, the bytes
//! are the full account (padding included), or the 32-byte balance for
//! Balances. Replay marks are held per NoReplay bucket.

use std::collections::BTreeMap;
use std::fmt;

use anyhow::{bail, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use global_accountant_definitions::{
    NoReplayBitmapAccount, NOREPLAY_BITMAP_BYTES, NOREPLAY_BITS_PER_BUCKET,
};

use crate::ids::{BucketId, MessageId, Sequence};
use crate::noreplay::bucket_id;
use crate::parity::compared_account::ComparedAccount;

/// Replay keys: 5,516,669 on mainnet.
pub const MAX_REPLAY_KEYS: usize = 16_000_000;
/// NoReplay buckets: about 50k on mainnet.
pub const MAX_BUCKETS: usize = 500_000;
/// Rows per keyed state: 17,367 balances on mainnet, tens for the others.
pub const MAX_KEYED_ROWS: usize = 100_000;

/// An account-backed state, compared row by row on its match key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeyedKind {
    Balances,
    ModifyBalanceRecords,
    ChainRegistrations,
    RegisterChainRecords,
    TransceiverHubs,
    TransceiverPeers,
}

/// A state a check compares between wormchain and Solana.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StateKind {
    Keyed(KeyedKind),
    ReplayMarks,
}

/// What a finding is about: a compared state, or a Solana-only check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FindingKind {
    State(StateKind),
    /// Program accounts that fit no layout: unknown tag, wrong size, non-canonical address.
    MalformedAccounts,
    /// `PendingObservations` accounts. Strict mode requires exactly 0.
    PendingObservations,
}

impl KeyedKind {
    pub const ALL: [KeyedKind; 6] = [
        KeyedKind::Balances,
        KeyedKind::ModifyBalanceRecords,
        KeyedKind::ChainRegistrations,
        KeyedKind::RegisterChainRecords,
        KeyedKind::TransceiverHubs,
        KeyedKind::TransceiverPeers,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Balances => "balances",
            Self::ModifyBalanceRecords => "modify_balance_records",
            Self::ChainRegistrations => "chain_registrations",
            Self::RegisterChainRecords => "register_chain_records",
            Self::TransceiverHubs => "transceiver_hubs",
            Self::TransceiverPeers => "transceiver_peers",
        }
    }

    /// Match-key length.
    pub fn key_len(self) -> usize {
        match self {
            Self::Balances | Self::TransceiverPeers => 36,
            Self::ModifyBalanceRecords | Self::RegisterChainRecords => 8,
            Self::ChainRegistrations => 2,
            Self::TransceiverHubs => 34,
        }
    }

    /// Human-readable form of a match key of length [`Self::key_len`].
    pub fn describe_key(self, key: &[u8]) -> String {
        assert_eq!(key.len(), self.key_len(), "{self:?} key length");
        let u16_at = |i: usize| u16::from_be_bytes([key[i], key[i + 1]]);
        let u64_at = |i: usize| u64::from_be_bytes(key[i..i + 8].try_into().expect("8 bytes"));
        let hex_at = |i: usize| format!("0x{}", hex::encode(&key[i..i + 32]));
        match self {
            Self::Balances => format!(
                "chain={} token_chain={} token={}",
                u16_at(0),
                u16_at(2),
                hex_at(4)
            ),
            Self::ModifyBalanceRecords | Self::RegisterChainRecords => {
                format!("sequence={}", u64_at(0))
            }
            Self::ChainRegistrations => format!("chain={}", u16_at(0)),
            Self::TransceiverHubs => format!("chain={} address={}", u16_at(0), hex_at(2)),
            Self::TransceiverPeers => format!(
                "chain={} address={} dest_chain={}",
                u16_at(0),
                hex_at(2),
                u16_at(34)
            ),
        }
    }
}

impl StateKind {
    pub const BALANCES: StateKind = StateKind::Keyed(KeyedKind::Balances);

    pub fn name(self) -> &'static str {
        match self {
            Self::Keyed(kind) => kind.name(),
            Self::ReplayMarks => "replay_marks",
        }
    }
}

impl FindingKind {
    /// Every finding kind, for name lookup.
    const ALL: [FindingKind; 9] = [
        FindingKind::State(StateKind::BALANCES),
        FindingKind::State(StateKind::ReplayMarks),
        FindingKind::State(StateKind::Keyed(KeyedKind::ModifyBalanceRecords)),
        FindingKind::State(StateKind::Keyed(KeyedKind::ChainRegistrations)),
        FindingKind::State(StateKind::Keyed(KeyedKind::RegisterChainRecords)),
        FindingKind::State(StateKind::Keyed(KeyedKind::TransceiverHubs)),
        FindingKind::State(StateKind::Keyed(KeyedKind::TransceiverPeers)),
        FindingKind::MalformedAccounts,
        FindingKind::PendingObservations,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::State(state) => state.name(),
            Self::MalformedAccounts => "malformed_accounts",
            Self::PendingObservations => "pending_observations",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

impl From<KeyedKind> for FindingKind {
    fn from(kind: KeyedKind) -> Self {
        Self::State(StateKind::Keyed(kind))
    }
}

macro_rules! impl_name_serde {
    ($($ty:ty => $from_name:expr),* $(,)?) => {$(
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.name())
            }
        }

        /// Serialized as its snake_case name, so it is a JSON map key.
        impl Serialize for $ty {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.name())
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let name = String::deserialize(deserializer)?;
                let from_name: fn(&str) -> Option<$ty> = $from_name;
                from_name(&name)
                    .ok_or_else(|| serde::de::Error::custom(format!("unknown kind `{name}`")))
            }
        }
    )*};
}

impl_name_serde!(
    KeyedKind => |name| KeyedKind::ALL.into_iter().find(|kind| kind.name() == name),
    StateKind => |name| match FindingKind::from_name(name)? {
        FindingKind::State(state) => Some(state),
        FindingKind::MalformedAccounts | FindingKind::PendingObservations => None,
    },
    FindingKind => FindingKind::from_name,
);

/// One 1024-bit NoReplay bucket, bit layout as `NoReplayBitmapAccount`.
pub type BucketBits = [u8; NOREPLAY_BITMAP_BYTES];

/// Replay marks as one mask per bucket. Every stored mask has at least one bit set.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReplayMarks {
    buckets: BTreeMap<BucketId, BucketBits>,
    count: usize,
}

impl ReplayMarks {
    /// Mark `message`. Fails on a duplicate or when a bound is hit.
    pub fn insert(&mut self, message: MessageId) -> Result<()> {
        let bucket = bucket_id(message.emitter, message.sequence);
        let bit = NoReplayBitmapAccount::bit_index(message.sequence.0);
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
            bail!("duplicate replay mark {message}");
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

    /// Marked messages in `(chain, emitter, sequence)` order.
    pub fn iter(&self) -> impl Iterator<Item = MessageId> + '_ {
        self.buckets
            .iter()
            .flat_map(|(bucket, mask)| marked_messages(bucket, mask))
    }

    /// SHA-256 over `count_be ‖ (chain_be ‖ emitter ‖ sequence_be)*` in key order.
    pub fn hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update((self.count as u64).to_be_bytes());
        for message in self.iter() {
            hasher.update(message.emitter.chain.0.to_be_bytes());
            hasher.update(message.emitter.emitter.0);
            hasher.update(message.sequence.0.to_be_bytes());
        }
        hasher.finalize().into()
    }
}

/// Messages whose bit is set in `mask`, ascending.
pub fn marked_messages<'a>(
    bucket: &'a BucketId,
    mask: &'a BucketBits,
) -> impl Iterator<Item = MessageId> + 'a {
    (0..NOREPLAY_BITS_PER_BUCKET).filter_map(move |bit| {
        is_set(mask, bit as usize).then_some(MessageId {
            emitter: bucket.emitter,
            sequence: Sequence(bucket.index * NOREPLAY_BITS_PER_BUCKET + bit),
        })
    })
}

/// Bit layout of `NoReplayBitmapAccount::is_marked`: little-endian within each byte.
fn is_set(mask: &BucketBits, bit: usize) -> bool {
    mask[bit / 8] & (1 << (bit % 8)) != 0
}

fn popcount(mask: &BucketBits) -> usize {
    mask.iter().map(|b| b.count_ones() as usize).sum()
}

/// Rows of one keyed state: match key → compared bytes.
pub type RowsByKey = BTreeMap<Vec<u8>, Vec<u8>>;

/// One chain's accountant state, keyed and ordered deterministically: the
/// expected state from the snapshot, or the actual state read from Solana.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ChainState {
    keyed: BTreeMap<KeyedKind, RowsByKey>,
    pub replay_marks: ReplayMarks,
}

impl ChainState {
    pub fn rows(&self, kind: KeyedKind) -> &RowsByKey {
        static EMPTY: RowsByKey = BTreeMap::new();
        self.keyed.get(&kind).unwrap_or(&EMPTY)
    }

    /// Insert one row. Fails on a duplicate key or when the bound is hit.
    pub fn insert(&mut self, kind: KeyedKind, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        assert_eq!(key.len(), kind.key_len(), "{kind:?} key length");
        assert!(!value.is_empty(), "{kind:?} value is empty");
        let rows = self.keyed.entry(kind).or_default();
        if rows.len() == MAX_KEYED_ROWS {
            bail!("{kind} exceeds MAX_KEYED_ROWS ({MAX_KEYED_ROWS})");
        }
        let rendered = kind.describe_key(&key);
        if rows.insert(key, value).is_some() {
            bail!("two {kind} rows for {rendered}");
        }
        Ok(())
    }

    /// Insert one account under its kind's match key and compared bytes.
    pub fn insert_account<L: ComparedAccount>(&mut self, layout: &L) -> Result<()> {
        self.insert(L::KIND, layout.match_key(), layout.compared_bytes())
    }

    /// SHA-256 over `count_be ‖ (key ‖ value)*` in key order. For Balances
    /// this is `count ‖ (chain_be ‖ token_chain_be ‖ token_address ‖ balance)*`.
    pub fn hash_of(&self, state: StateKind) -> [u8; 32] {
        let kind = match state {
            StateKind::ReplayMarks => return self.replay_marks.hash(),
            StateKind::Keyed(kind) => kind,
        };
        let rows = self.rows(kind);
        let mut hasher = Sha256::new();
        hasher.update((rows.len() as u64).to_be_bytes());
        for (key, value) in rows {
            hasher.update(key);
            hasher.update(value);
        }
        hasher.finalize().into()
    }

    pub fn len_of(&self, state: StateKind) -> usize {
        match state {
            StateKind::ReplayMarks => self.replay_marks.len(),
            StateKind::Keyed(kind) => self.rows(kind).len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{Address, ChainId, EmitterKey};

    fn message(chain: u16, emitter: u8, sequence: u64) -> MessageId {
        MessageId {
            emitter: EmitterKey {
                chain: ChainId(chain),
                emitter: Address([emitter; 32]),
            },
            sequence: Sequence(sequence),
        }
    }

    fn state_with(balance: u8, sequence: u64) -> ChainState {
        let mut state = ChainState::default();
        let mut key = vec![0, 1, 0, 2];
        key.extend_from_slice(&[0xAA; 32]);
        state
            .insert(KeyedKind::Balances, key, vec![balance; 32])
            .unwrap();
        state
            .replay_marks
            .insert(message(2, 0xBB, sequence))
            .unwrap();
        state
    }

    #[test]
    fn hashes_are_deterministic_and_key_sensitive() {
        let base = state_with(1, 5);
        let balances = StateKind::BALANCES;
        let replay = StateKind::ReplayMarks;
        assert_eq!(base.hash_of(balances), state_with(1, 5).hash_of(balances));
        assert_eq!(base.hash_of(replay), state_with(1, 5).hash_of(replay));

        assert_ne!(base.hash_of(balances), state_with(2, 5).hash_of(balances));
        assert_ne!(base.hash_of(replay), state_with(1, 6).hash_of(replay));

        let empty = ChainState::default();
        assert_ne!(base.hash_of(balances), empty.hash_of(balances));
        assert_ne!(base.hash_of(replay), empty.hash_of(replay));
    }

    #[test]
    fn insertion_order_does_not_change_hashes() {
        let mut forward = ReplayMarks::default();
        let mut backward = ReplayMarks::default();
        for sequence in [1u64, 2, 3] {
            forward.insert(message(1, 0x11, sequence)).unwrap();
        }
        for sequence in [3u64, 2, 1] {
            backward.insert(message(1, 0x11, sequence)).unwrap();
        }
        assert_eq!(forward.hash(), backward.hash());
    }

    #[test]
    fn local_bit_layout_matches_the_bitmap_account() {
        for bit in 0..NOREPLAY_BITS_PER_BUCKET {
            let mut account: NoReplayBitmapAccount = bytemuck::Zeroable::zeroed();
            let sequence = 3 * NOREPLAY_BITS_PER_BUCKET + bit;
            let index = NoReplayBitmapAccount::bit_index(sequence);
            account.bitmap[index / 8] |= 1 << (index % 8);
            assert!(account.is_marked(sequence), "bit {bit}");
            assert!(is_set(&account.bitmap, index), "bit {bit}");
            assert_eq!(popcount(&account.bitmap), 1, "bit {bit}");
        }
    }

    #[test]
    fn kind_names_round_trip_through_serde() {
        for kind in FindingKind::ALL {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.name()));
            assert_eq!(serde_json::from_str::<FindingKind>(&json).unwrap(), kind);
        }
        assert!(serde_json::from_str::<StateKind>("\"malformed_accounts\"").is_err());
        assert!(serde_json::from_str::<FindingKind>("\"unknown\"").is_err());
    }
}
