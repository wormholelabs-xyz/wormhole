//! `solana-noreplay`: program ID, instruction discriminators, and bitmap geometry.

use crate::primitives::Pubkey;
use const_crypto::bs58;

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
pub const NOREPLAY_BITS_PER_BUCKET: u64 = 1024;

/// Bitmap payload size in a NoReplay PDA (1-byte bump + bitmap).
pub const NOREPLAY_BITMAP_BYTES: usize = 128;

/// Bitmap payload offset; byte 0 holds the bump.
pub const NOREPLAY_BITMAP_OFFSET: usize = 1;

#[cfg(test)]
mod tests {
    use super::*;

    /// Only deployment as of 2026-08: Solana devnet `repMHgR5BEpGLeZvM5iGoNNDPw4eu2BS6sXJzaC8K4t`.
    #[test]
    fn program_id_is_known_deployment() {
        let devnet = bs58::decode_pubkey("repMHgR5BEpGLeZvM5iGoNNDPw4eu2BS6sXJzaC8K4t");
        assert_eq!(NOREPLAY_PROGRAM_ID, devnet);
    }

    #[test]
    fn wire_constants_match_solana_noreplay() {
        assert_eq!(NOREPLAY_MARK_USED_DISCRIMINATOR, solana_noreplay::MARK_USED);
        assert_eq!(NOREPLAY_BITS_PER_BUCKET, solana_noreplay::BITS_PER_BUCKET);
        assert_eq!(NOREPLAY_BITMAP_BYTES, solana_noreplay::BITMAP_BYTES);
        assert_eq!(
            NOREPLAY_BITMAP_OFFSET + NOREPLAY_BITMAP_BYTES,
            solana_noreplay::BITMAP_ACCOUNT_SIZE
        );
    }

    #[test]
    fn mark_used_bulk_matches_solana_noreplay() {
        assert_eq!(
            NOREPLAY_MARK_USED_BULK_DISCRIMINATOR,
            solana_noreplay::MARK_USED_BULK
        );
        assert_eq!(
            NOREPLAY_BITMAP_BYTES,
            solana_noreplay::MARK_USED_BULK_MASK_LEN
        );
    }
}
