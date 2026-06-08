//! On-chain state for the backfill program.

use bytemuck::{Pod, Zeroable};

use crate::definitions::Pubkey;

/// PDA seed for the singleton backfill-authority record.
pub const BACKFILL_AUTHORITY_SEED_PREFIX: &[u8] = b"backfill-authority";

/// Backfill authority record. Lazy-initialised on the first backfill ix; the
/// initialising signer's pubkey is recorded as the authority. Subsequent ixes
/// reject unless `signer == authority && !retired`.
///
/// On-disk layout (33 bytes):
///
/// | offset | size | field     |
/// |--------|------|-----------|
/// | 0      | 32   | authority |
/// | 32     | 1    | retired   |
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct BackfillAuthorityLayout {
    pub authority: Pubkey,
    /// `0` = active, `1` = retired (kill switch). Any non-zero value is treated
    /// as retired; tests pin only `0` and `1`.
    pub retired: u8,
}

impl BackfillAuthorityLayout {
    pub const LEN: usize = core::mem::size_of::<Self>();
}

const _: () = {
    use core::mem::offset_of;
    assert!(offset_of!(BackfillAuthorityLayout, authority) == 0);
    assert!(offset_of!(BackfillAuthorityLayout, retired) == 32);
    assert!(BackfillAuthorityLayout::LEN == 33);
};
