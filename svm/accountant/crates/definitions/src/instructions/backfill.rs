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

use bytemuck::{Pod, Zeroable};

use crate::error::GlobalAccountantError;
use crate::primitives::Uint256;

/// `BackfillBalance` / `BackfillNoReplay` instruction discriminator.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackfillInstruction {
    BackfillNoReplay = 0,
    BackfillBalance = 1,
}

impl BackfillInstruction {
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::BackfillNoReplay),
            1 => Some(Self::BackfillBalance),
            _ => None,
        }
    }
}

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

const _: () = {
    use core::mem::offset_of;
    assert!(BackfillBalanceEntry::LEN == 68);
    assert!(offset_of!(BackfillBalanceEntry, token_chain) == 2);
    assert!(offset_of!(BackfillBalanceEntry, token_address) == 4);
    assert!(offset_of!(BackfillBalanceEntry, balance) == 36);
    assert!(BackfillNoReplayGroupHeader::LEN == 35);
    assert!(offset_of!(BackfillNoReplayGroupHeader, emitter) == 2);
    assert!(offset_of!(BackfillNoReplayGroupHeader, entry_count) == 34);
    assert!(BackfillNoReplayEntry::LEN == 40);
    assert!(offset_of!(BackfillNoReplayEntry, digest) == 8);
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

    #[test]
    fn backfill_instruction_from_u8() {
        let cases: [(u8, Option<BackfillInstruction>); 3] = [
            (0, Some(BackfillInstruction::BackfillNoReplay)),
            (1, Some(BackfillInstruction::BackfillBalance)),
            (2, None),
        ];
        for (value, expected) in cases {
            assert_eq!(BackfillInstruction::from_u8(value), expected, "{value}");
        }
    }
}
