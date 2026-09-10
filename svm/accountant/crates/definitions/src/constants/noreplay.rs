//! `solana-noreplay`: program ID, instruction discriminators, and bitmap geometry.

use bytemuck::{Pod, Zeroable};
use const_crypto::bs58;

use crate::primitives::Pubkey;

/// `solana-noreplay` program ID, from `NOREPLAY_PROGRAM_ID` at compile time (the variable
/// `solana-noreplay` itself reads). Set per target network in `justfile`.
pub const NOREPLAY_PROGRAM_ID: Pubkey = bs58::decode_pubkey(env!("NOREPLAY_PROGRAM_ID"));

/// `MarkUsed` discriminator. Wire format:
/// `[disc: u8][namespace_len: u16 LE][namespace: ≤64 B][sequence: u64 LE]`.
pub const NOREPLAY_MARK_USED_DISCRIMINATOR: u8 = 1;

/// `MarkUsedBulk` discriminator. Wire format:
/// `[disc: u8][namespace_len: u16 LE][namespace: ≤64 B][bucket_index: u64 LE][or_mask: 128 B]`.
/// Effect: `bitmap |= or_mask`.
pub const NOREPLAY_MARK_USED_BULK_DISCRIMINATOR: u8 = 3;

/// Bits per bucket. Bucket = `sequence / BITS_PER_BUCKET`; bit = `sequence % BITS_PER_BUCKET`.
pub const NOREPLAY_BITS_PER_BUCKET: u64 = (NOREPLAY_BITMAP_BYTES * 8) as u64;

/// Bitmap bytes per bucket.
pub const NOREPLAY_BITMAP_BYTES: usize = 128;

/// NoReplay bitmap PDA data: `bump ‖ bitmap`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct NoReplayBitmapAccount {
    pub bump: u8,
    pub bitmap: [u8; NOREPLAY_BITMAP_BYTES],
}

impl NoReplayBitmapAccount {
    pub const LEN: usize = core::mem::size_of::<Self>();

    /// Exact-length view; `None` on any other length.
    pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
        bytemuck::try_from_bytes(bytes).ok()
    }

    /// Index of the bucket PDA holding `sequence`.
    pub const fn bucket_index(sequence: u64) -> u64 {
        sequence / NOREPLAY_BITS_PER_BUCKET
    }

    /// Bit position of `sequence` within its bucket: `0..NOREPLAY_BITS_PER_BUCKET`.
    pub const fn bit_index(sequence: u64) -> usize {
        (sequence % NOREPLAY_BITS_PER_BUCKET) as usize
    }

    /// Bit for `sequence` within this bucket.
    pub fn is_marked(&self, sequence: u64) -> bool {
        let bit = Self::bit_index(sequence);
        self.bitmap[bit / 8] & (1 << (bit % 8)) != 0
    }
}

/// NoReplay namespace for the accountant: `chain_be ‖ emitter`. NoReplay splits
/// namespaces longer than 32 bytes into two PDA seeds at byte 32.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct NoReplayNamespace {
    pub chain: [u8; 2],
    pub emitter: [u8; 32],
}

impl NoReplayNamespace {
    pub const LEN: usize = core::mem::size_of::<Self>();
    const SEED_SPLIT: usize = 32;

    pub fn new(chain: u16, emitter: [u8; 32]) -> Self {
        Self {
            chain: chain.to_be_bytes(),
            emitter,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }

    /// The two PDA seed chunks NoReplay derives from this namespace.
    pub fn seed_chunks(&self) -> (&[u8], &[u8]) {
        self.as_bytes().split_at(Self::SEED_SPLIT)
    }
}

/// `MarkUsed` instruction data: `disc ‖ namespace_len_le ‖ namespace ‖ sequence_le`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct NoReplayMarkUsedData {
    pub discriminator: u8,
    pub namespace_len: [u8; 2],
    pub namespace: NoReplayNamespace,
    pub sequence: [u8; 8],
}

impl NoReplayMarkUsedData {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn new(namespace: NoReplayNamespace, sequence: u64) -> Self {
        Self {
            discriminator: NOREPLAY_MARK_USED_DISCRIMINATOR,
            namespace_len: (NoReplayNamespace::LEN as u16).to_le_bytes(),
            namespace,
            sequence: sequence.to_le_bytes(),
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }
}

const _: () = {
    use core::mem::offset_of;
    assert!(NoReplayNamespace::LEN <= 64); // NoReplay `MAX_NAMESPACE_LEN`
    assert!(NoReplayBitmapAccount::LEN == 129);
    assert!(offset_of!(NoReplayBitmapAccount, bitmap) == 1);
    assert!(NoReplayNamespace::LEN == 34);
    assert!(NoReplayMarkUsedData::LEN == 45);
    assert!(offset_of!(NoReplayMarkUsedData, namespace_len) == 1);
    assert!(offset_of!(NoReplayMarkUsedData, namespace) == 3);
    assert!(offset_of!(NoReplayMarkUsedData, sequence) == 37);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_solana_noreplay_and_addresses_bits() {
        let devnet = bs58::decode_pubkey("repMHgR5BEpGLeZvM5iGoNNDPw4eu2BS6sXJzaC8K4t");
        assert_eq!(NOREPLAY_PROGRAM_ID, devnet);
        assert_eq!(NOREPLAY_MARK_USED_DISCRIMINATOR, solana_noreplay::MARK_USED);
        assert_eq!(
            NOREPLAY_MARK_USED_BULK_DISCRIMINATOR,
            solana_noreplay::MARK_USED_BULK
        );
        assert_eq!(NOREPLAY_BITS_PER_BUCKET, solana_noreplay::BITS_PER_BUCKET);
        assert_eq!(NOREPLAY_BITMAP_BYTES, solana_noreplay::BITMAP_BYTES);
        assert_eq!(
            NOREPLAY_BITMAP_BYTES,
            solana_noreplay::MARK_USED_BULK_MASK_LEN
        );
        assert_eq!(
            NoReplayBitmapAccount::LEN,
            solana_noreplay::BITMAP_ACCOUNT_SIZE
        );

        let namespace = NoReplayNamespace::new(2, [0xAB; 32]);
        let bytes = NoReplayMarkUsedData::new(namespace, 5_000)
            .as_bytes()
            .to_vec();
        assert_eq!(bytes[0], solana_noreplay::MARK_USED);
        let parsed = solana_noreplay::InstructionData::try_from(&bytes[1..]).unwrap();
        assert_eq!(parsed.namespace, namespace.as_bytes());
        assert_eq!(parsed.sequence, 5_000);

        let index_cases: [(u64, u64, usize); 6] = [
            (0, 0, 0),
            (1, 0, 1),
            (1023, 0, 1023),
            (1024, 1, 0),
            (2049, 2, 1),
            (u64::MAX, u64::MAX / 1024, 1023),
        ];
        for (sequence, bucket, bit) in index_cases {
            assert_eq!(
                NoReplayBitmapAccount::bucket_index(sequence),
                bucket,
                "bucket {sequence}"
            );
            assert_eq!(
                NoReplayBitmapAccount::bit_index(sequence),
                bit,
                "bit {sequence}"
            );
        }

        let mut account = NoReplayBitmapAccount::zeroed();
        account.bitmap[0] = 0b0000_0001;
        account.bitmap[127] = 0b1000_0000;
        let bit_cases: [(&str, u64, bool); 4] = [
            ("bit 0 set", 0, true),
            ("bit 1 clear", 1, false),
            ("bit 1023 set", 1023, true),
            ("bit 1024 aliases bit 0", 1024, true),
        ];
        for (name, sequence, marked) in bit_cases {
            assert_eq!(account.is_marked(sequence), marked, "{name}");
        }
        assert_eq!(NoReplayBitmapAccount::from_bytes(&[0u8; 128]), None);
    }
}
