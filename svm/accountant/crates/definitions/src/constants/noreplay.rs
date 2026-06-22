//! `solana-noreplay`: program ID, instruction discriminators, and bitmap geometry.

use crate::primitives::Pubkey;

/// Canonical program ID for `solana-noreplay`
/// (`repMHgR5BEpGLeZvM5iGoNNDPw4eu2BS6sXJzaC8K4t`). Raw bytes to keep this
/// crate Solana-SDK-free.
pub const NOREPLAY_PROGRAM_ID: Pubkey = [
    0x0c, 0xb8, 0x38, 0x00, 0x73, 0xdf, 0x36, 0x25, 0xa1, 0x32, 0x11, 0x1f, 0xee, 0x67, 0x8d, 0xd0,
    0x6b, 0x7e, 0x3d, 0xf2, 0x90, 0xa2, 0xb1, 0xd5, 0x4a, 0x48, 0x5b, 0xdb, 0x72, 0x61, 0x82, 0x91,
];

/// Discriminator for `solana-noreplay`'s `MarkUsed`. Wire format:
/// `[disc: u8][namespace_len: u16 LE][namespace: ≤64 B][sequence: u64 LE]`.
pub const NOREPLAY_MARK_USED_DISCRIMINATOR: u8 = 1;

/// Discriminator for `solana-noreplay`'s `MarkUsedBulk`. Wire format:
/// `[disc: u8][namespace_len: u16 LE][namespace: ≤64 B][bucket_index: u64 LE]
///  [or_mask: 128 B]`. Semantics: `bitmap |= or_mask` (OR-only; never clears
/// bits). Used by the backfill program to flip many bits per CPI, dropping
/// per-entry CU from ~3,000 to ~80 in the dense-bucket case.
///
/// Allocated as the next free byte in the noreplay program's dispatch table
/// after `CreateBitmap=0`, `MarkUsed=1`, `UnmarkUsed=2`.
pub const NOREPLAY_MARK_USED_BULK_DISCRIMINATOR: u8 = 3;

/// Bits per bitmap bucket. Bucket index is `sequence / BITS_PER_BUCKET`, bit
/// offset is `sequence % BITS_PER_BUCKET`.
pub const NOREPLAY_BITS_PER_BUCKET: u64 = 1024;

/// Bitmap payload size inside a noreplay PDA (account is 1-byte bump + bitmap).
pub const NOREPLAY_BITMAP_BYTES: usize = 128;

/// Byte offset of the bitmap payload (byte 0 is the stored canonical bump).
pub const NOREPLAY_BITMAP_OFFSET: usize = 1;
