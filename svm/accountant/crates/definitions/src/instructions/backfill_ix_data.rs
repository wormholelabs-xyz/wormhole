//! Backfill wire formats, after the 1-byte instruction discriminator. Offsets are
//! pinned by the `const _` block below; these tables are orientation only.
//!
//! `BackfillBalanceEntry` (68 bytes):
//!
//! | offset | size | field         |
//! |--------|------|---------------|
//! | 0      | 2    | chain (BE)    |
//! | 2      | 2    | token_chain (BE) |
//! | 4      | 32   | token_address |
//! | 36     | 32   | balance (BE)  |
//!
//! `BackfillBalance` wire: `count (u8) ‖ count × entry`, entries strictly ascending
//! by `(chain, token_chain, token_address)`.
//!
//! `BackfillNoReplayGroupHeader` (35 bytes) / `BackfillNoReplayEntry` (40 bytes):
//!
//! | offset | size | field            |
//! |--------|------|------------------|
//! | 0      | 2    | chain (BE)       |
//! | 2      | 32   | emitter          |
//! | 34     | 1    | entry_count      |
//! | +0     | 8    | sequence (BE)    |
//! | +8     | 32   | digest           |
//!
//! `BackfillNoReplay` wire: `group_count (u8) ‖ group_count × (header ‖ entry_count ×
//! entry)`. Groups strictly ascending by `(chain, emitter)`; sequences strictly
//! ascending within a group.
//!
//! `BackfillModifyBalanceEntry` (109 bytes):
//!
//! | offset | size | field         |
//! |--------|------|---------------|
//! | 0      | 1    | kind          |
//! | 1      | 2    | chain_id (BE) |
//! | 3      | 2    | token_chain (BE) |
//! | 5      | 8    | sequence (BE) |
//! | 13     | 32   | token_address |
//! | 45     | 32   | amount (BE)   |
//! | 77     | 32   | reason        |
//!
//! `ModifyBalanceBatch` wire: `count (u8) ‖ count × entry`, entries strictly ascending
//! by `sequence`.
//!
//! `BackfillChainRegistrationEntry` (42 bytes):
//!
//! | offset | size | field         |
//! |--------|------|---------------|
//! | 0      | 2    | chain (BE)    |
//! | 2      | 8    | sequence (BE) |
//! | 10     | 32   | emitter       |
//!
//! `ChainRegistrationBatch` wire: `count (u8) ‖ count × entry`, entries strictly ascending
//! by `chain`.
//!
//! `BackfillTransceiverHubEntry` (68 bytes):
//!
//! | offset | size | field          |
//! |--------|------|----------------|
//! | 0      | 2    | chain (BE)     |
//! | 2      | 32   | address        |
//! | 34     | 2    | hub_chain (BE) |
//! | 36     | 32   | hub_address    |
//!
//! `TransceiverHubBatch` wire: `count (u8) ‖ count × entry`, entries strictly ascending
//! by `(chain, address)`.
//!
//! `BackfillTransceiverPeerEntry` (68 bytes):
//!
//! | offset | size | field           |
//! |--------|------|-----------------|
//! | 0      | 2    | chain (BE)      |
//! | 2      | 32   | address         |
//! | 34     | 2    | dest_chain (BE) |
//! | 36     | 32   | peer_address    |
//!
//! `TransceiverPeerBatch` wire: `count (u8) ‖ count × entry`, entries strictly ascending
//! by `(chain, address, dest_chain)`.

use bytemuck::{Pod, Zeroable};

use crate::error::GlobalAccountantError;
use crate::pda::{TransceiverHubKey, TransceiverPeerKey};
use crate::primitives::Uint256;
use crate::state::{TransceiverHubLayout, TransceiverPeerLayout};

/// `BackfillBalance` entry (68 bytes). Big-endian, as in the wormchain snapshot row.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct BackfillBalanceEntry {
    pub chain: [u8; 2],
    pub token_chain: [u8; 2],
    pub token_address: [u8; 32],
    pub balance: [u8; 32],
}

impl BackfillBalanceEntry {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn new(chain: u16, token_chain: u16, token_address: [u8; 32], balance: Uint256) -> Self {
        Self {
            chain: chain.to_be_bytes(),
            token_chain: token_chain.to_be_bytes(),
            token_address,
            balance: balance.0,
        }
    }

    pub fn chain(&self) -> u16 {
        u16::from_be_bytes(self.chain)
    }

    pub fn token_chain(&self) -> u16 {
        u16::from_be_bytes(self.token_chain)
    }

    pub fn balance(&self) -> Uint256 {
        Uint256::from_be_bytes(self.balance)
    }

    /// PDA seed key, and the batch sort key.
    pub fn key(&self) -> (u16, u16, [u8; 32]) {
        (self.chain(), self.token_chain(), self.token_address)
    }
}

/// `BackfillNoReplay` group header (35 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct BackfillNoReplayGroupHeader {
    pub chain: [u8; 2],
    pub emitter: [u8; 32],
    pub entry_count: u8,
}

impl BackfillNoReplayGroupHeader {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn new(chain: u16, emitter: [u8; 32], entry_count: u8) -> Self {
        Self {
            chain: chain.to_be_bytes(),
            emitter,
            entry_count,
        }
    }

    pub fn chain(&self) -> u16 {
        u16::from_be_bytes(self.chain)
    }
}

/// `BackfillNoReplay` entry inside a group (40 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct BackfillNoReplayEntry {
    pub sequence: [u8; 8],
    pub digest: [u8; 32],
}

impl BackfillNoReplayEntry {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn new(sequence: u64, digest: [u8; 32]) -> Self {
        Self {
            sequence: sequence.to_be_bytes(),
            digest,
        }
    }

    pub fn sequence(&self) -> u64 {
        u64::from_be_bytes(self.sequence)
    }
}

/// `BackfillModifyBalance` entry (109 bytes). Big-endian, as in the wormchain row.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct BackfillModifyBalanceEntry {
    pub kind: u8,
    pub chain_id: [u8; 2],
    pub token_chain: [u8; 2],
    pub sequence: [u8; 8],
    pub token_address: [u8; 32],
    pub amount: [u8; 32],
    pub reason: [u8; 32],
}

impl BackfillModifyBalanceEntry {
    pub const LEN: usize = core::mem::size_of::<Self>();

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        kind: u8,
        chain_id: u16,
        token_chain: u16,
        sequence: u64,
        token_address: [u8; 32],
        amount: Uint256,
        reason: [u8; 32],
    ) -> Self {
        Self {
            kind,
            chain_id: chain_id.to_be_bytes(),
            token_chain: token_chain.to_be_bytes(),
            sequence: sequence.to_be_bytes(),
            token_address,
            amount: amount.0,
            reason,
        }
    }

    pub fn chain_id(&self) -> u16 {
        u16::from_be_bytes(self.chain_id)
    }

    pub fn token_chain(&self) -> u16 {
        u16::from_be_bytes(self.token_chain)
    }

    pub fn sequence(&self) -> u64 {
        u64::from_be_bytes(self.sequence)
    }

    pub fn amount(&self) -> Uint256 {
        Uint256::from_be_bytes(self.amount)
    }
}

/// `BackfillChainRegistration` entry (42 bytes). Big-endian, as in the wormchain row.
/// `sequence` is the governance VAA that installed the registration; it keys the
/// `RegisterChain` record PDA and is stored in `ChainRegistrationLayout`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct BackfillChainRegistrationEntry {
    pub chain: [u8; 2],
    pub sequence: [u8; 8],
    pub emitter: [u8; 32],
}

impl BackfillChainRegistrationEntry {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn new(chain: u16, sequence: u64, emitter: [u8; 32]) -> Self {
        Self {
            chain: chain.to_be_bytes(),
            sequence: sequence.to_be_bytes(),
            emitter,
        }
    }

    pub fn chain(&self) -> u16 {
        u16::from_be_bytes(self.chain)
    }

    pub fn sequence(&self) -> u64 {
        u64::from_be_bytes(self.sequence)
    }
}

/// `BackfillTransceiverHub` entry (68 bytes). Big-endian, as in the wormchain
/// `transceiver_to_hub` row. A hub names itself; a spoke names the hub that
/// `register_peer`'s adoption arm put it under.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct BackfillTransceiverHubEntry {
    pub chain: [u8; 2],
    pub address: [u8; 32],
    pub hub_chain: [u8; 2],
    pub hub_address: [u8; 32],
}

impl BackfillTransceiverHubEntry {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn new(chain: u16, address: [u8; 32], hub_chain: u16, hub_address: [u8; 32]) -> Self {
        Self {
            chain: chain.to_be_bytes(),
            address,
            hub_chain: hub_chain.to_be_bytes(),
            hub_address,
        }
    }

    pub fn chain(&self) -> u16 {
        u16::from_be_bytes(self.chain)
    }

    pub fn hub_chain(&self) -> u16 {
        u16::from_be_bytes(self.hub_chain)
    }

    /// PDA key of the transceiver this row is stored under.
    pub fn key(&self) -> TransceiverHubKey {
        TransceiverHubKey::new(self.chain(), self.address)
    }

    /// The hub this transceiver belongs to.
    pub fn hub(&self) -> TransceiverHubKey {
        TransceiverHubKey::new(self.hub_chain(), self.hub_address)
    }

    /// Account bytes, from the constructor `register_hub` and `register_peer` use.
    pub fn layout(&self) -> TransceiverHubLayout {
        TransceiverHubLayout::new(self.key(), self.hub())
    }
}

/// `BackfillTransceiverPeer` entry (68 bytes). Big-endian, as in the wormchain
/// `transceiver_peers` row: `address` on `chain` sends to `peer_address` on `dest_chain`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct BackfillTransceiverPeerEntry {
    pub chain: [u8; 2],
    pub address: [u8; 32],
    pub dest_chain: [u8; 2],
    pub peer_address: [u8; 32],
}

impl BackfillTransceiverPeerEntry {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn new(chain: u16, address: [u8; 32], dest_chain: u16, peer_address: [u8; 32]) -> Self {
        Self {
            chain: chain.to_be_bytes(),
            address,
            dest_chain: dest_chain.to_be_bytes(),
            peer_address,
        }
    }

    pub fn chain(&self) -> u16 {
        u16::from_be_bytes(self.chain)
    }

    pub fn dest_chain(&self) -> u16 {
        u16::from_be_bytes(self.dest_chain)
    }

    /// PDA key of the peer entry, and the batch sort key.
    pub fn key(&self) -> TransceiverPeerKey {
        TransceiverPeerKey::new(self.chain(), self.address, self.dest_chain())
    }

    /// Account bytes, from the constructor `register_peer` uses.
    pub fn layout(&self) -> TransceiverPeerLayout {
        TransceiverPeerLayout::new(self.key(), self.peer_address)
    }
}

const _: () = {
    use core::mem::offset_of;
    assert!(BackfillTransceiverHubEntry::LEN == 68);
    assert!(BackfillTransceiverPeerEntry::LEN == 68);
    assert!(offset_of!(BackfillTransceiverPeerEntry, address) == 2);
    assert!(offset_of!(BackfillTransceiverPeerEntry, dest_chain) == 34);
    assert!(offset_of!(BackfillTransceiverPeerEntry, peer_address) == 36);
    assert!(offset_of!(BackfillTransceiverHubEntry, address) == 2);
    assert!(offset_of!(BackfillTransceiverHubEntry, hub_chain) == 34);
    assert!(offset_of!(BackfillTransceiverHubEntry, hub_address) == 36);
    assert!(BackfillChainRegistrationEntry::LEN == 42);
    assert!(offset_of!(BackfillChainRegistrationEntry, sequence) == 2);
    assert!(offset_of!(BackfillChainRegistrationEntry, emitter) == 10);
    assert!(BackfillBalanceEntry::LEN == 68);
    assert!(offset_of!(BackfillBalanceEntry, token_chain) == 2);
    assert!(offset_of!(BackfillBalanceEntry, token_address) == 4);
    assert!(offset_of!(BackfillBalanceEntry, balance) == 36);
    assert!(BackfillNoReplayGroupHeader::LEN == 35);
    assert!(offset_of!(BackfillNoReplayGroupHeader, emitter) == 2);
    assert!(offset_of!(BackfillNoReplayGroupHeader, entry_count) == 34);
    assert!(BackfillNoReplayEntry::LEN == 40);
    assert!(offset_of!(BackfillNoReplayEntry, digest) == 8);
    assert!(BackfillModifyBalanceEntry::LEN == 109);
    assert!(offset_of!(BackfillModifyBalanceEntry, chain_id) == 1);
    assert!(offset_of!(BackfillModifyBalanceEntry, token_chain) == 3);
    assert!(offset_of!(BackfillModifyBalanceEntry, sequence) == 5);
    assert!(offset_of!(BackfillModifyBalanceEntry, token_address) == 13);
    assert!(offset_of!(BackfillModifyBalanceEntry, amount) == 45);
    assert!(offset_of!(BackfillModifyBalanceEntry, reason) == 77);
};

/// Balance entries, strictly ascending by `(chain, token_chain, token_address)`.
/// `parse` is the only constructor.
pub struct BalanceBatch<'a>(&'a [BackfillBalanceEntry]);

impl<'a> BalanceBatch<'a> {
    /// Wire: `count (u8) ‖ count × BackfillBalanceEntry`.
    pub fn parse(data: &'a [u8]) -> Result<Self, GlobalAccountantError> {
        let (&count, rest) = data
            .split_first()
            .ok_or(GlobalAccountantError::InvalidInstructionData)?;
        if count == 0 {
            return Err(GlobalAccountantError::InvalidInstructionData);
        }
        let expected_len = count as usize * BackfillBalanceEntry::LEN;
        if rest.len() != expected_len {
            return Err(GlobalAccountantError::InvalidInstructionData);
        }
        let entries: &[BackfillBalanceEntry] = bytemuck::try_cast_slice(rest)
            .map_err(|_| GlobalAccountantError::InvalidInstructionData)?;
        if entries.len() != count as usize {
            return Err(GlobalAccountantError::InvalidInstructionData);
        }

        let mut prev_key: Option<(u16, u16, [u8; 32])> = None;
        for entry in entries {
            let cur_key = entry.key();
            if let Some(prev) = prev_key {
                if cur_key <= prev {
                    return Err(GlobalAccountantError::InvalidInstructionData);
                }
            }
            prev_key = Some(cur_key);
        }

        Ok(Self(entries))
    }

    pub fn entries(&self) -> &'a [BackfillBalanceEntry] {
        self.0
    }
}

/// Modification records, strictly ascending by `sequence`. `parse` is the only constructor.
pub struct ModifyBalanceBatch<'a>(&'a [BackfillModifyBalanceEntry]);

impl<'a> ModifyBalanceBatch<'a> {
    /// Wire: `count (u8) ‖ count × BackfillModifyBalanceEntry`.
    pub fn parse(data: &'a [u8]) -> Result<Self, GlobalAccountantError> {
        let (&count, rest) = data
            .split_first()
            .ok_or(GlobalAccountantError::InvalidInstructionData)?;
        if count == 0 {
            return Err(GlobalAccountantError::InvalidInstructionData);
        }
        let expected_len = count as usize * BackfillModifyBalanceEntry::LEN;
        if rest.len() != expected_len {
            return Err(GlobalAccountantError::InvalidInstructionData);
        }
        let entries: &[BackfillModifyBalanceEntry] = bytemuck::try_cast_slice(rest)
            .map_err(|_| GlobalAccountantError::InvalidInstructionData)?;
        if entries.len() != count as usize {
            return Err(GlobalAccountantError::InvalidInstructionData);
        }

        let mut prev_sequence: Option<u64> = None;
        for entry in entries {
            let sequence = entry.sequence();
            if let Some(prev) = prev_sequence {
                if sequence <= prev {
                    return Err(GlobalAccountantError::InvalidInstructionData);
                }
            }
            prev_sequence = Some(sequence);
        }

        Ok(Self(entries))
    }

    pub fn entries(&self) -> &'a [BackfillModifyBalanceEntry] {
        self.0
    }
}

/// Chain registrations, strictly ascending by `chain`. `parse` is the only constructor.
pub struct ChainRegistrationBatch<'a>(&'a [BackfillChainRegistrationEntry]);

impl<'a> ChainRegistrationBatch<'a> {
    /// Wire: `count (u8) ‖ count × BackfillChainRegistrationEntry`.
    pub fn parse(data: &'a [u8]) -> Result<Self, GlobalAccountantError> {
        let (&count, rest) = data
            .split_first()
            .ok_or(GlobalAccountantError::InvalidInstructionData)?;
        if count == 0 {
            return Err(GlobalAccountantError::InvalidInstructionData);
        }
        let expected_len = count as usize * BackfillChainRegistrationEntry::LEN;
        if rest.len() != expected_len {
            return Err(GlobalAccountantError::InvalidInstructionData);
        }
        let entries: &[BackfillChainRegistrationEntry] = bytemuck::try_cast_slice(rest)
            .map_err(|_| GlobalAccountantError::InvalidInstructionData)?;
        if entries.len() != count as usize {
            return Err(GlobalAccountantError::InvalidInstructionData);
        }

        let mut prev_chain: Option<u16> = None;
        for entry in entries {
            let chain = entry.chain();
            if let Some(prev) = prev_chain {
                if chain <= prev {
                    return Err(GlobalAccountantError::InvalidInstructionData);
                }
            }
            prev_chain = Some(chain);
        }

        Ok(Self(entries))
    }

    pub fn entries(&self) -> &'a [BackfillChainRegistrationEntry] {
        self.0
    }
}

/// Transceiver-to-hub rows, strictly ascending by `(chain, address)`. `parse` is the only
/// constructor.
pub struct TransceiverHubBatch<'a>(&'a [BackfillTransceiverHubEntry]);

impl<'a> TransceiverHubBatch<'a> {
    /// Wire: `count (u8) ‖ count × BackfillTransceiverHubEntry`.
    pub fn parse(data: &'a [u8]) -> Result<Self, GlobalAccountantError> {
        let (&count, rest) = data
            .split_first()
            .ok_or(GlobalAccountantError::InvalidInstructionData)?;
        if count == 0 {
            return Err(GlobalAccountantError::InvalidInstructionData);
        }
        let expected_len = count as usize * BackfillTransceiverHubEntry::LEN;
        if rest.len() != expected_len {
            return Err(GlobalAccountantError::InvalidInstructionData);
        }
        let entries: &[BackfillTransceiverHubEntry] = bytemuck::try_cast_slice(rest)
            .map_err(|_| GlobalAccountantError::InvalidInstructionData)?;
        if entries.len() != count as usize {
            return Err(GlobalAccountantError::InvalidInstructionData);
        }

        let mut prev_key: Option<(u16, [u8; 32])> = None;
        for entry in entries {
            let key = (entry.chain(), entry.address);
            if let Some(prev) = prev_key {
                if key <= prev {
                    return Err(GlobalAccountantError::InvalidInstructionData);
                }
            }
            prev_key = Some(key);
        }

        Ok(Self(entries))
    }

    pub fn entries(&self) -> &'a [BackfillTransceiverHubEntry] {
        self.0
    }
}

/// Transceiver peer rows, strictly ascending by `(chain, address, dest_chain)`. `parse` is
/// the only constructor.
pub struct TransceiverPeerBatch<'a>(&'a [BackfillTransceiverPeerEntry]);

impl<'a> TransceiverPeerBatch<'a> {
    /// Wire: `count (u8) ‖ count × BackfillTransceiverPeerEntry`.
    pub fn parse(data: &'a [u8]) -> Result<Self, GlobalAccountantError> {
        let (&count, rest) = data
            .split_first()
            .ok_or(GlobalAccountantError::InvalidInstructionData)?;
        if count == 0 {
            return Err(GlobalAccountantError::InvalidInstructionData);
        }
        let expected_len = count as usize * BackfillTransceiverPeerEntry::LEN;
        if rest.len() != expected_len {
            return Err(GlobalAccountantError::InvalidInstructionData);
        }
        let entries: &[BackfillTransceiverPeerEntry] = bytemuck::try_cast_slice(rest)
            .map_err(|_| GlobalAccountantError::InvalidInstructionData)?;
        if entries.len() != count as usize {
            return Err(GlobalAccountantError::InvalidInstructionData);
        }

        let mut prev_key: Option<(u16, [u8; 32], u16)> = None;
        for entry in entries {
            // `register_peer` rejects a peer on the sender's own chain.
            if entry.chain() == entry.dest_chain() {
                return Err(GlobalAccountantError::SameChainPeer);
            }
            let key = (entry.chain(), entry.address, entry.dest_chain());
            if let Some(prev) = prev_key {
                if key <= prev {
                    return Err(GlobalAccountantError::InvalidInstructionData);
                }
            }
            prev_key = Some(key);
        }

        Ok(Self(entries))
    }

    pub fn entries(&self) -> &'a [BackfillTransceiverPeerEntry] {
        self.0
    }
}

/// NoReplay groups, strictly ascending by `(chain, emitter)`, each group non-empty,
/// each group's sequences strictly ascending. `parse` is the only constructor.
pub struct NoReplayBatch<'a> {
    body: &'a [u8],
    group_count: u8,
}

impl<'a> NoReplayBatch<'a> {
    /// Wire: `group_count (u8) ‖ group_count × (header ‖ entry_count × entry)`.
    pub fn parse(data: &'a [u8]) -> Result<Self, GlobalAccountantError> {
        let (&group_count, mut rest) = data
            .split_first()
            .ok_or(GlobalAccountantError::InvalidInstructionData)?;
        if group_count == 0 {
            return Err(GlobalAccountantError::InvalidInstructionData);
        }
        let body = rest;

        let mut prev_group: Option<(u16, [u8; 32])> = None;
        let mut prev_full: Option<(u16, [u8; 32], u64)> = None;

        for _ in 0..group_count {
            let (header_bytes, after_header) = rest
                .split_at_checked(BackfillNoReplayGroupHeader::LEN)
                .ok_or(GlobalAccountantError::InvalidInstructionData)?;
            let header: &BackfillNoReplayGroupHeader = bytemuck::try_from_bytes(header_bytes)
                .map_err(|_| GlobalAccountantError::InvalidInstructionData)?;
            if header.entry_count == 0 {
                return Err(GlobalAccountantError::InvalidInstructionData);
            }

            let group_key = (header.chain(), header.emitter);
            if let Some(prev) = prev_group {
                if group_key <= prev {
                    return Err(GlobalAccountantError::InvalidInstructionData);
                }
            }
            prev_group = Some(group_key);

            let entries_len = header.entry_count as usize * BackfillNoReplayEntry::LEN;
            let (entries_bytes, after_entries) = after_header
                .split_at_checked(entries_len)
                .ok_or(GlobalAccountantError::InvalidInstructionData)?;
            let entries: &[BackfillNoReplayEntry] = bytemuck::try_cast_slice(entries_bytes)
                .map_err(|_| GlobalAccountantError::InvalidInstructionData)?;
            if entries.len() != header.entry_count as usize {
                return Err(GlobalAccountantError::InvalidInstructionData);
            }

            let mut prev_seq: Option<u64> = None;
            for entry in entries {
                let sequence = entry.sequence();
                if let Some(prev) = prev_seq {
                    if sequence <= prev {
                        return Err(GlobalAccountantError::InvalidInstructionData);
                    }
                }
                prev_seq = Some(sequence);

                let cur_full = (group_key.0, group_key.1, sequence);
                if let Some(prev) = prev_full {
                    if cur_full <= prev {
                        return Err(GlobalAccountantError::InvalidInstructionData);
                    }
                }
                prev_full = Some(cur_full);
            }

            rest = after_entries;
        }

        if !rest.is_empty() {
            return Err(GlobalAccountantError::InvalidInstructionData);
        }

        Ok(Self { body, group_count })
    }

    /// Infallible; `parse` proved the framing.
    pub fn groups(&self) -> impl Iterator<Item = NoReplayGroup<'a>> {
        NoReplayGroupIter {
            rest: self.body,
            remaining: self.group_count,
        }
    }
}

pub struct NoReplayGroup<'a> {
    pub header: &'a BackfillNoReplayGroupHeader,
    pub entries: &'a [BackfillNoReplayEntry],
}

struct NoReplayGroupIter<'a> {
    rest: &'a [u8],
    remaining: u8,
}

impl<'a> Iterator for NoReplayGroupIter<'a> {
    type Item = NoReplayGroup<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;

        // `parse` validated framing and counts; each split and cast below is
        // proven to succeed.
        let (header_bytes, after_header) = self
            .rest
            .split_at_checked(BackfillNoReplayGroupHeader::LEN)
            .expect("validated by parse");
        let header: &'a BackfillNoReplayGroupHeader =
            bytemuck::try_from_bytes(header_bytes).expect("validated by parse");
        let entries_len = header.entry_count as usize * BackfillNoReplayEntry::LEN;
        let (entries_bytes, after_entries) = after_header
            .split_at_checked(entries_len)
            .expect("validated by parse");
        let entries: &'a [BackfillNoReplayEntry] =
            bytemuck::try_cast_slice(entries_bytes).expect("validated by parse");

        self.rest = after_entries;
        Some(NoReplayGroup { header, entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn balance_entry(
        chain: u16,
        token_chain: u16,
        token_addr_seed: u8,
        balance: u128,
    ) -> BackfillBalanceEntry {
        BackfillBalanceEntry::new(
            chain,
            token_chain,
            [token_addr_seed; 32],
            Uint256::from_u128(balance),
        )
    }

    fn encode_balance_batch(entries: &[BackfillBalanceEntry]) -> std::vec::Vec<u8> {
        let mut out = std::vec![entries.len() as u8];
        for entry in entries {
            out.extend_from_slice(bytemuck::bytes_of(entry));
        }
        out
    }

    #[test]
    fn balance_batch_parses_positive_cases() {
        let one = [balance_entry(1, 1, 0xAA, 100)];
        let several = [
            balance_entry(1, 1, 0xAA, 100),
            balance_entry(1, 2, 0xAA, 200),
            balance_entry(2, 1, 0xAA, 300),
        ];
        let max_count: std::vec::Vec<BackfillBalanceEntry> = (0..255u16)
            .map(|i| balance_entry(1, i, 0, i as u128))
            .collect();

        let cases: [(&str, std::vec::Vec<BackfillBalanceEntry>); 3] = [
            ("one entry", one.to_vec()),
            ("several entries", several.to_vec()),
            ("max u8 count", max_count),
        ];
        for (name, entries) in cases {
            let data = encode_balance_batch(&entries);
            let batch = BalanceBatch::parse(&data).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(batch.entries(), entries.as_slice(), "{name}");
        }
    }

    #[test]
    fn balance_batch_rejects_malformed_wire() {
        let ok_entries = [
            balance_entry(1, 1, 0xAA, 100),
            balance_entry(1, 2, 0xAA, 200),
        ];
        let ok_data = encode_balance_batch(&ok_entries);

        let dup_entries = [
            balance_entry(1, 1, 0xAA, 100),
            balance_entry(1, 1, 0xAA, 200),
        ];
        let desc_entries = [
            balance_entry(1, 2, 0xAA, 100),
            balance_entry(1, 1, 0xAA, 200),
        ];

        let cases: [(&str, std::vec::Vec<u8>); 8] = [
            ("empty data", std::vec::Vec::new()),
            ("zero count", std::vec![0u8]),
            ("one byte short", ok_data[..ok_data.len() - 1].to_vec()),
            ("one byte long", [ok_data.as_slice(), &[0u8]].concat()),
            (
                "trailing bytes after exact entries",
                [ok_data.as_slice(), &[0xFFu8; 3]].concat(),
            ),
            ("duplicate key", encode_balance_batch(&dup_entries)),
            ("descending key", encode_balance_batch(&desc_entries)),
            ("count claims more entries than present", {
                let mut d = encode_balance_batch(&ok_entries);
                d[0] = 3;
                d
            }),
        ];
        for (name, data) in cases {
            assert_eq!(
                BalanceBatch::parse(&data).err(),
                Some(GlobalAccountantError::InvalidInstructionData),
                "{name}"
            );
        }
    }

    #[test]
    fn balance_entry_round_trips_through_bytes() {
        let entry = BackfillBalanceEntry::new(7, 9, [0x11; 32], Uint256::from_u128(12345));
        let bytes = bytemuck::bytes_of(&entry);
        let data = encode_balance_batch(core::slice::from_ref(&entry));
        let batch = BalanceBatch::parse(&data).unwrap();
        assert_eq!(batch.entries()[0], entry);
        assert_eq!(batch.entries()[0].chain(), 7);
        assert_eq!(batch.entries()[0].token_chain(), 9);
        assert_eq!(batch.entries()[0].balance(), Uint256::from_u128(12345));
        assert_eq!(bytes.len(), BackfillBalanceEntry::LEN);
    }

    fn noreplay_group(
        chain: u16,
        emitter: [u8; 32],
        entries: &[(u64, [u8; 32])],
    ) -> std::vec::Vec<u8> {
        let mut out = bytemuck::bytes_of(&BackfillNoReplayGroupHeader::new(
            chain,
            emitter,
            entries.len() as u8,
        ))
        .to_vec();
        for &(sequence, digest) in entries {
            out.extend_from_slice(bytemuck::bytes_of(&BackfillNoReplayEntry::new(
                sequence, digest,
            )));
        }
        out
    }

    fn encode_noreplay_batch(groups: &[std::vec::Vec<u8>]) -> std::vec::Vec<u8> {
        let mut out = std::vec![groups.len() as u8];
        for group in groups {
            out.extend_from_slice(group);
        }
        out
    }

    #[test]
    fn noreplay_batch_parses_positive_cases() {
        let one_group = [noreplay_group(1, [0xAA; 32], &[(1, [1; 32])])];
        let several_entries = [noreplay_group(
            1,
            [0xAA; 32],
            &[(1, [1; 32]), (2, [2; 32]), (3, [3; 32])],
        )];
        let several_groups = [
            noreplay_group(1, [0xAA; 32], &[(1, [1; 32])]),
            noreplay_group(1, [0xBB; 32], &[(5, [5; 32])]),
            noreplay_group(2, [0xAA; 32], &[(1, [1; 32]), (2, [2; 32])]),
        ];
        let max_entries: std::vec::Vec<(u64, [u8; 32])> =
            (0..255u64).map(|i| (i + 1, [0; 32])).collect();
        let max_count = [noreplay_group(1, [0xAA; 32], &max_entries)];

        type Case<'a> = (
            &'a str,
            std::vec::Vec<std::vec::Vec<u8>>,
            usize,
            &'a [usize],
        );
        let cases: [Case; 4] = [
            ("one group, one entry", one_group.to_vec(), 1, &[1]),
            (
                "one group, several entries",
                several_entries.to_vec(),
                1,
                &[3],
            ),
            ("several groups", several_groups.to_vec(), 3, &[1, 1, 2]),
            ("max u8 group entry count", max_count.to_vec(), 1, &[255]),
        ];
        for (name, groups, group_count, entry_counts) in cases {
            let data = encode_noreplay_batch(&groups);
            let batch = NoReplayBatch::parse(&data).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            let collected: std::vec::Vec<_> = batch.groups().collect();
            assert_eq!(collected.len(), group_count, "{name}");
            for (group, &expected_len) in collected.iter().zip(entry_counts) {
                assert_eq!(group.entries.len(), expected_len, "{name}");
                assert_eq!(group.header.entry_count as usize, expected_len, "{name}");
            }
        }
    }

    #[test]
    fn noreplay_batch_rejects_malformed_wire() {
        let ok_groups = [
            noreplay_group(1, [0xAA; 32], &[(1, [1; 32])]),
            noreplay_group(1, [0xBB; 32], &[(5, [5; 32])]),
        ];
        let ok_data = encode_noreplay_batch(&ok_groups);

        let mut zero_entry_count = noreplay_group(1, [0xAA; 32], &[(1, [1; 32])]);
        zero_entry_count[34] = 0;

        let dup_group_groups = [
            noreplay_group(1, [0xAA; 32], &[(1, [1; 32])]),
            noreplay_group(1, [0xAA; 32], &[(2, [2; 32])]),
        ];
        let desc_group_groups = [
            noreplay_group(1, [0xBB; 32], &[(1, [1; 32])]),
            noreplay_group(1, [0xAA; 32], &[(2, [2; 32])]),
        ];
        let dup_seq_group = [noreplay_group(1, [0xAA; 32], &[(1, [1; 32]), (1, [2; 32])])];
        let desc_seq_group = [noreplay_group(1, [0xAA; 32], &[(2, [1; 32]), (1, [2; 32])])];

        let cases: [(&str, std::vec::Vec<u8>); 9] = [
            ("empty data", std::vec::Vec::new()),
            ("zero group count", std::vec![0u8]),
            ("one byte short", ok_data[..ok_data.len() - 1].to_vec()),
            ("one byte long", [ok_data.as_slice(), &[0u8]].concat()),
            (
                "trailing bytes after exact groups",
                [ok_data.as_slice(), &[0xFFu8; 5]].concat(),
            ),
            (
                "entry_count == 0",
                encode_noreplay_batch(std::slice::from_ref(&zero_entry_count)),
            ),
            (
                "duplicate group key",
                encode_noreplay_batch(&dup_group_groups),
            ),
            (
                "descending group key",
                encode_noreplay_batch(&desc_group_groups),
            ),
            ("duplicate sequence", encode_noreplay_batch(&dup_seq_group)),
        ];
        for (name, data) in cases {
            assert_eq!(
                NoReplayBatch::parse(&data).err(),
                Some(GlobalAccountantError::InvalidInstructionData),
                "{name}"
            );
        }
        assert_eq!(
            NoReplayBatch::parse(&encode_noreplay_batch(&desc_seq_group)).err(),
            Some(GlobalAccountantError::InvalidInstructionData),
            "descending sequence"
        );
    }

    #[test]
    fn noreplay_entry_round_trips_through_bytes() {
        let entry = BackfillNoReplayEntry::new(42, [0x22; 32]);
        let bytes = bytemuck::bytes_of(&entry);
        assert_eq!(bytes.len(), BackfillNoReplayEntry::LEN);
        let header = BackfillNoReplayGroupHeader::new(3, [0x33; 32], 1);
        assert_eq!(header.chain(), 3);
        let mut data = std::vec![1u8];
        data.extend_from_slice(bytemuck::bytes_of(&header));
        data.extend_from_slice(bytes);
        let batch = NoReplayBatch::parse(&data).unwrap();
        let groups: std::vec::Vec<_> = batch.groups().collect();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].header.chain(), 3);
        assert_eq!(groups[0].entries[0], entry);
        assert_eq!(groups[0].entries[0].sequence(), 42);
    }

    fn modify_balance_entry(kind: u8, sequence: u64, amount: u128) -> BackfillModifyBalanceEntry {
        BackfillModifyBalanceEntry::new(
            kind,
            2,
            2,
            sequence,
            [0xAA; 32],
            Uint256::from_u128(amount),
            *b"audit-log: backfilled modify    ",
        )
    }

    fn encode_modify_balance_batch(entries: &[BackfillModifyBalanceEntry]) -> std::vec::Vec<u8> {
        let mut out = std::vec![entries.len() as u8];
        for entry in entries {
            out.extend_from_slice(bytemuck::bytes_of(entry));
        }
        out
    }

    #[test]
    fn modify_balance_batch_parses_positive_cases() {
        let one = [modify_balance_entry(1, 100, 1_000)];
        let several = [
            modify_balance_entry(1, 100, 1_000),
            modify_balance_entry(2, 101, 2_000),
            modify_balance_entry(1, 102, 3_000),
            modify_balance_entry(2, 103, 4_000),
            modify_balance_entry(1, 104, 5_000),
            modify_balance_entry(2, 105, 6_000),
        ];
        let max_count: std::vec::Vec<BackfillModifyBalanceEntry> = (0..255u64)
            .map(|i| modify_balance_entry(1, i, i as u128))
            .collect();

        let cases: [(&str, std::vec::Vec<BackfillModifyBalanceEntry>); 3] = [
            ("one entry", one.to_vec()),
            ("six entries", several.to_vec()),
            ("max u8 count", max_count),
        ];
        for (name, entries) in cases {
            let data = encode_modify_balance_batch(&entries);
            let batch =
                ModifyBalanceBatch::parse(&data).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(batch.entries(), entries.as_slice(), "{name}");
        }
    }

    #[test]
    fn modify_balance_batch_rejects_malformed_wire() {
        let ok_entries = [
            modify_balance_entry(1, 100, 1_000),
            modify_balance_entry(2, 101, 2_000),
        ];
        let ok_data = encode_modify_balance_batch(&ok_entries);

        let dup_entries = [
            modify_balance_entry(1, 100, 1_000),
            modify_balance_entry(2, 100, 2_000),
        ];
        let desc_entries = [
            modify_balance_entry(1, 101, 1_000),
            modify_balance_entry(2, 100, 2_000),
        ];

        let cases: [(&str, std::vec::Vec<u8>); 8] = [
            ("empty data", std::vec::Vec::new()),
            ("zero count", std::vec![0u8]),
            ("one byte short", ok_data[..ok_data.len() - 1].to_vec()),
            ("one byte long", [ok_data.as_slice(), &[0u8]].concat()),
            (
                "trailing bytes after exact entries",
                [ok_data.as_slice(), &[0xFFu8; 3]].concat(),
            ),
            (
                "duplicate sequence",
                encode_modify_balance_batch(&dup_entries),
            ),
            (
                "descending sequence",
                encode_modify_balance_batch(&desc_entries),
            ),
            ("count claims more entries than present", {
                let mut d = encode_modify_balance_batch(&ok_entries);
                d[0] = 3;
                d
            }),
        ];
        for (name, data) in cases {
            assert_eq!(
                ModifyBalanceBatch::parse(&data).err(),
                Some(GlobalAccountantError::InvalidInstructionData),
                "{name}"
            );
        }
    }

    #[test]
    fn modify_balance_entry_round_trips_through_bytes() {
        let entry = modify_balance_entry(2, 42, 12345);
        let bytes = bytemuck::bytes_of(&entry);
        let data = encode_modify_balance_batch(core::slice::from_ref(&entry));
        let batch = ModifyBalanceBatch::parse(&data).unwrap();
        assert_eq!(batch.entries()[0], entry);
        assert_eq!(batch.entries()[0].chain_id(), 2);
        assert_eq!(batch.entries()[0].token_chain(), 2);
        assert_eq!(batch.entries()[0].sequence(), 42);
        assert_eq!(batch.entries()[0].amount(), Uint256::from_u128(12345));
        assert_eq!(bytes.len(), BackfillModifyBalanceEntry::LEN);
    }

    fn chain_registration_entry(chain: u16, sequence: u64) -> BackfillChainRegistrationEntry {
        BackfillChainRegistrationEntry::new(chain, sequence, [chain as u8; 32])
    }

    fn encode_chain_registration_batch(
        entries: &[BackfillChainRegistrationEntry],
    ) -> std::vec::Vec<u8> {
        let mut out = std::vec![entries.len() as u8];
        for entry in entries {
            out.extend_from_slice(bytemuck::bytes_of(entry));
        }
        out
    }

    #[test]
    fn chain_registration_batch_parses_positive_cases() {
        let one = [chain_registration_entry(2, 100)];
        let several = [
            chain_registration_entry(2, 300),
            chain_registration_entry(4, 100),
            chain_registration_entry(5, 200),
        ];
        let max_count: std::vec::Vec<BackfillChainRegistrationEntry> = (0..255u16)
            .map(|i| chain_registration_entry(i + 1, i as u64))
            .collect();

        let cases: [(&str, std::vec::Vec<BackfillChainRegistrationEntry>); 3] = [
            ("one entry", one.to_vec()),
            ("several entries, sequences unordered", several.to_vec()),
            ("max u8 count", max_count),
        ];
        for (name, entries) in cases {
            let data = encode_chain_registration_batch(&entries);
            let batch =
                ChainRegistrationBatch::parse(&data).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(batch.entries(), entries.as_slice(), "{name}");
        }
    }

    #[test]
    fn chain_registration_batch_rejects_malformed_wire() {
        let ok_entries = [
            chain_registration_entry(2, 100),
            chain_registration_entry(4, 101),
        ];
        let ok_data = encode_chain_registration_batch(&ok_entries);

        let dup_entries = [
            chain_registration_entry(2, 100),
            chain_registration_entry(2, 101),
        ];
        let desc_entries = [
            chain_registration_entry(4, 100),
            chain_registration_entry(2, 101),
        ];

        let cases: [(&str, std::vec::Vec<u8>); 8] = [
            ("empty data", std::vec::Vec::new()),
            ("zero count", std::vec![0u8]),
            ("one byte short", ok_data[..ok_data.len() - 1].to_vec()),
            ("one byte long", [ok_data.as_slice(), &[0u8]].concat()),
            (
                "trailing bytes after exact entries",
                [ok_data.as_slice(), &[0xFFu8; 3]].concat(),
            ),
            (
                "duplicate chain",
                encode_chain_registration_batch(&dup_entries),
            ),
            (
                "descending chain",
                encode_chain_registration_batch(&desc_entries),
            ),
            ("count claims more entries than present", {
                let mut d = encode_chain_registration_batch(&ok_entries);
                d[0] = 3;
                d
            }),
        ];
        for (name, data) in cases {
            assert_eq!(
                ChainRegistrationBatch::parse(&data).err(),
                Some(GlobalAccountantError::InvalidInstructionData),
                "{name}"
            );
        }
    }

    #[test]
    fn chain_registration_entry_round_trips_through_bytes() {
        let entry = BackfillChainRegistrationEntry::new(7, 42, [0x11; 32]);
        let bytes = bytemuck::bytes_of(&entry);
        let data = encode_chain_registration_batch(core::slice::from_ref(&entry));
        let batch = ChainRegistrationBatch::parse(&data).unwrap();
        assert_eq!(batch.entries()[0], entry);
        assert_eq!(batch.entries()[0].chain(), 7);
        assert_eq!(batch.entries()[0].sequence(), 42);
        assert_eq!(batch.entries()[0].emitter, [0x11; 32]);
        assert_eq!(bytes.len(), BackfillChainRegistrationEntry::LEN);
    }

    fn transceiver_hub_entry(
        chain: u16,
        address_seed: u8,
        hub_chain: u16,
        hub_seed: u8,
    ) -> BackfillTransceiverHubEntry {
        BackfillTransceiverHubEntry::new(chain, [address_seed; 32], hub_chain, [hub_seed; 32])
    }

    fn encode_transceiver_hub_batch(entries: &[BackfillTransceiverHubEntry]) -> std::vec::Vec<u8> {
        let mut out = std::vec![entries.len() as u8];
        for entry in entries {
            out.extend_from_slice(bytemuck::bytes_of(entry));
        }
        out
    }

    #[test]
    fn transceiver_hub_batch_parses_positive_cases() {
        let one = [transceiver_hub_entry(1, 0x7B, 1, 0x7B)];
        // Hubs and spokes share one map; a spoke points at another chain's hub.
        let several = [
            transceiver_hub_entry(1, 0x7B, 1, 0x7B),
            transceiver_hub_entry(2, 0x11, 1, 0x7B),
            transceiver_hub_entry(2, 0x22, 1, 0x7B),
            transceiver_hub_entry(5, 0x33, 1, 0x7B),
        ];
        let same_chain_ascending_address = [
            transceiver_hub_entry(2, 0x01, 2, 0x01),
            transceiver_hub_entry(2, 0x02, 2, 0x01),
        ];
        let max_count: std::vec::Vec<BackfillTransceiverHubEntry> = (0..255u16)
            .map(|i| transceiver_hub_entry(i + 1, 0x01, 1, 0x7B))
            .collect();

        let cases: [(&str, std::vec::Vec<BackfillTransceiverHubEntry>); 4] = [
            ("one self-referential hub", one.to_vec()),
            ("hub plus spokes", several.to_vec()),
            (
                "one chain, ascending addresses",
                same_chain_ascending_address.to_vec(),
            ),
            ("max u8 count", max_count),
        ];
        for (name, entries) in cases {
            let data = encode_transceiver_hub_batch(&entries);
            let batch =
                TransceiverHubBatch::parse(&data).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(batch.entries(), entries.as_slice(), "{name}");
        }
    }

    #[test]
    fn transceiver_hub_batch_rejects_malformed_wire() {
        let ok_entries = [
            transceiver_hub_entry(1, 0x7B, 1, 0x7B),
            transceiver_hub_entry(2, 0x11, 1, 0x7B),
        ];
        let ok_data = encode_transceiver_hub_batch(&ok_entries);

        let dup_entries = [
            transceiver_hub_entry(2, 0x11, 1, 0x7B),
            transceiver_hub_entry(2, 0x11, 2, 0x11),
        ];
        let desc_chain = [
            transceiver_hub_entry(4, 0x11, 1, 0x7B),
            transceiver_hub_entry(2, 0x11, 1, 0x7B),
        ];
        let desc_address = [
            transceiver_hub_entry(2, 0x22, 1, 0x7B),
            transceiver_hub_entry(2, 0x11, 1, 0x7B),
        ];

        let cases: [(&str, std::vec::Vec<u8>); 9] = [
            ("empty data", std::vec::Vec::new()),
            ("zero count", std::vec![0u8]),
            ("one byte short", ok_data[..ok_data.len() - 1].to_vec()),
            ("one byte long", [ok_data.as_slice(), &[0u8]].concat()),
            (
                "trailing bytes after exact entries",
                [ok_data.as_slice(), &[0xFFu8; 3]].concat(),
            ),
            (
                "duplicate (chain, address)",
                encode_transceiver_hub_batch(&dup_entries),
            ),
            (
                "descending chain",
                encode_transceiver_hub_batch(&desc_chain),
            ),
            (
                "descending address within a chain",
                encode_transceiver_hub_batch(&desc_address),
            ),
            ("count claims more entries than present", {
                let mut d = encode_transceiver_hub_batch(&ok_entries);
                d[0] = 3;
                d
            }),
        ];
        for (name, data) in cases {
            assert_eq!(
                TransceiverHubBatch::parse(&data).err(),
                Some(GlobalAccountantError::InvalidInstructionData),
                "{name}"
            );
        }
    }

    #[test]
    fn transceiver_hub_entry_round_trips_through_bytes() {
        let entry = BackfillTransceiverHubEntry::new(2, [0x11; 32], 1, [0x7B; 32]);
        let bytes = bytemuck::bytes_of(&entry);
        let data = encode_transceiver_hub_batch(core::slice::from_ref(&entry));
        let batch = TransceiverHubBatch::parse(&data).unwrap();
        assert_eq!(batch.entries()[0], entry);
        assert_eq!(batch.entries()[0].chain(), 2);
        assert_eq!(batch.entries()[0].hub_chain(), 1);
        assert_eq!(
            batch.entries()[0].key(),
            TransceiverHubKey::new(2, [0x11; 32])
        );
        assert_eq!(
            batch.entries()[0].hub(),
            TransceiverHubKey::new(1, [0x7B; 32])
        );
        assert_eq!(
            batch.entries()[0].layout(),
            TransceiverHubLayout::new(
                TransceiverHubKey::new(2, [0x11; 32]),
                TransceiverHubKey::new(1, [0x7B; 32]),
            )
        );
        assert_eq!(bytes.len(), BackfillTransceiverHubEntry::LEN);
    }

    fn transceiver_peer_entry(
        chain: u16,
        address_seed: u8,
        dest_chain: u16,
        peer_seed: u8,
    ) -> BackfillTransceiverPeerEntry {
        BackfillTransceiverPeerEntry::new(chain, [address_seed; 32], dest_chain, [peer_seed; 32])
    }

    fn encode_transceiver_peer_batch(
        entries: &[BackfillTransceiverPeerEntry],
    ) -> std::vec::Vec<u8> {
        let mut out = std::vec![entries.len() as u8];
        for entry in entries {
            out.extend_from_slice(bytemuck::bytes_of(entry));
        }
        out
    }

    #[test]
    fn transceiver_peer_batch_parses_positive_cases() {
        let one = [transceiver_peer_entry(1, 0x7B, 2, 0x7A)];
        // Both directions of one pair, and a second destination for the same transceiver.
        let pair = [
            transceiver_peer_entry(1, 0x7B, 2, 0x7A),
            transceiver_peer_entry(2, 0x7A, 1, 0x7B),
        ];
        let same_address_ascending_dest = [
            transceiver_peer_entry(1, 0x7B, 2, 0x7A),
            transceiver_peer_entry(1, 0x7B, 5, 0x7C),
        ];
        let max_count: std::vec::Vec<BackfillTransceiverPeerEntry> = (0..255u16)
            .map(|i| transceiver_peer_entry(i + 2, 0x01, 1, 0x7B))
            .collect();

        let cases: [(&str, std::vec::Vec<BackfillTransceiverPeerEntry>); 4] = [
            ("one peer", one.to_vec()),
            ("both directions of one pair", pair.to_vec()),
            (
                "one transceiver, ascending dest chains",
                same_address_ascending_dest.to_vec(),
            ),
            ("max u8 count", max_count),
        ];
        for (name, entries) in cases {
            let data = encode_transceiver_peer_batch(&entries);
            let batch =
                TransceiverPeerBatch::parse(&data).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(batch.entries(), entries.as_slice(), "{name}");
        }
    }

    #[test]
    fn transceiver_peer_batch_rejects_malformed_wire() {
        let ok_entries = [
            transceiver_peer_entry(1, 0x7B, 2, 0x7A),
            transceiver_peer_entry(2, 0x7A, 1, 0x7B),
        ];
        let ok_data = encode_transceiver_peer_batch(&ok_entries);

        let dup_entries = [
            transceiver_peer_entry(1, 0x7B, 2, 0x7A),
            transceiver_peer_entry(1, 0x7B, 2, 0x11),
        ];
        let desc_chain = [
            transceiver_peer_entry(2, 0x7A, 1, 0x7B),
            transceiver_peer_entry(1, 0x7B, 2, 0x7A),
        ];
        let desc_address = [
            transceiver_peer_entry(1, 0x7C, 2, 0x7A),
            transceiver_peer_entry(1, 0x7B, 2, 0x7A),
        ];
        let desc_dest_chain = [
            transceiver_peer_entry(1, 0x7B, 5, 0x7C),
            transceiver_peer_entry(1, 0x7B, 2, 0x7A),
        ];

        let cases: [(&str, std::vec::Vec<u8>); 10] = [
            ("empty data", std::vec::Vec::new()),
            ("zero count", std::vec![0u8]),
            ("one byte short", ok_data[..ok_data.len() - 1].to_vec()),
            ("one byte long", [ok_data.as_slice(), &[0u8]].concat()),
            (
                "trailing bytes after exact entries",
                [ok_data.as_slice(), &[0xFFu8; 3]].concat(),
            ),
            (
                "duplicate (chain, address, dest_chain)",
                encode_transceiver_peer_batch(&dup_entries),
            ),
            (
                "descending chain",
                encode_transceiver_peer_batch(&desc_chain),
            ),
            (
                "descending address within a chain",
                encode_transceiver_peer_batch(&desc_address),
            ),
            (
                "descending dest chain within a transceiver",
                encode_transceiver_peer_batch(&desc_dest_chain),
            ),
            ("count claims more entries than present", {
                let mut d = encode_transceiver_peer_batch(&ok_entries);
                d[0] = 3;
                d
            }),
        ];
        for (name, data) in cases {
            assert_eq!(
                TransceiverPeerBatch::parse(&data).err(),
                Some(GlobalAccountantError::InvalidInstructionData),
                "{name}"
            );
        }
    }

    /// `register_peer` rejects a peer on the transceiver's own chain, so no such row can
    /// exist in the snapshot.
    #[test]
    fn transceiver_peer_batch_rejects_a_same_chain_peer() {
        let data = encode_transceiver_peer_batch(&[transceiver_peer_entry(2, 0x7A, 2, 0x7B)]);
        assert_eq!(
            TransceiverPeerBatch::parse(&data).err(),
            Some(GlobalAccountantError::SameChainPeer)
        );
    }

    #[test]
    fn transceiver_peer_entry_round_trips_through_bytes() {
        let entry = BackfillTransceiverPeerEntry::new(2, [0x11; 32], 1, [0x7B; 32]);
        let bytes = bytemuck::bytes_of(&entry);
        let data = encode_transceiver_peer_batch(core::slice::from_ref(&entry));
        let batch = TransceiverPeerBatch::parse(&data).unwrap();
        assert_eq!(batch.entries()[0], entry);
        assert_eq!(batch.entries()[0].chain(), 2);
        assert_eq!(batch.entries()[0].dest_chain(), 1);
        assert_eq!(
            batch.entries()[0].key(),
            TransceiverPeerKey::new(2, [0x11; 32], 1)
        );
        assert_eq!(
            batch.entries()[0].layout(),
            TransceiverPeerLayout::new(TransceiverPeerKey::new(2, [0x11; 32], 1), [0x7B; 32])
        );
        assert_eq!(bytes.len(), BackfillTransceiverPeerEntry::LEN);
    }
}
