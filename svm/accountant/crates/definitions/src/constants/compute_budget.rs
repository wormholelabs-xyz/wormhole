//! Compute Budget program ID and the `SetComputeUnitLimit` wire layout.
//!
//! The Compute Budget instruction enum is borsh-encoded: a one-byte variant tag,
//! then the variant payload. `SetComputeUnitLimit` is variant 2 with a `u32`
//! little-endian unit count.

use bytemuck::{Pod, Zeroable};
use const_crypto::bs58;

use crate::primitives::Pubkey;

pub const COMPUTE_BUDGET_PROGRAM_ID: Pubkey =
    bs58::decode_pubkey("ComputeBudget111111111111111111111111111111");

pub const SET_COMPUTE_UNIT_LIMIT_TAG: u8 = 2;

/// `SetComputeUnitLimit` instruction data: `tag ‖ units`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct SetComputeUnitLimitData {
    pub tag: u8,
    /// Little-endian.
    pub units: [u8; 4],
}

impl SetComputeUnitLimitData {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub const fn new(units: u32) -> Self {
        Self {
            tag: SET_COMPUTE_UNIT_LIMIT_TAG,
            units: units.to_le_bytes(),
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }
}

const _: () = {
    use core::mem::offset_of;
    assert!(SetComputeUnitLimitData::LEN == 5);
    assert!(offset_of!(SetComputeUnitLimitData, units) == 1);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_compute_unit_limit_encoding() {
        let cases: [(u32, [u8; 5]); 3] = [
            (0, [2, 0, 0, 0, 0]),
            (400_000, [2, 0x80, 0x1a, 0x06, 0x00]),
            (u32::MAX, [2, 0xff, 0xff, 0xff, 0xff]),
        ];
        for (units, expected) in cases {
            assert_eq!(SetComputeUnitLimitData::new(units).as_bytes(), &expected);
        }
    }

    #[test]
    fn program_id_matches_solana_program_decoder() {
        assert_eq!(
            COMPUTE_BUDGET_PROGRAM_ID,
            solana_program::pubkey!("ComputeBudget111111111111111111111111111111").to_bytes()
        );
    }
}
