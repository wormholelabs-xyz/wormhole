//! Primitive value types shared across the accountant: the untyped address
//! alias and the on-disk 256-bit unsigned integer.

use bytemuck::{Pod, Zeroable};

/// 32-byte address, layout-compatible with `solana_address::Address` and
/// pinocchio's `Address`. Untyped to keep this crate Solana-SDK-free.
pub type Pubkey = [u8; 32];

/// 256-bit unsigned integer stored on-disk as 32 **big-endian** bytes. BE
/// matches the VAA `amount` wire format so a payload's bytes copy in directly.
/// `#[repr(transparent)]` over `[u8; 32]` keeps it `Pod`; arithmetic round-trips
/// through `ruint`'s `U256` at the boundary.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct Uint256(pub [u8; 32]);

impl Uint256 {
    /// All-zero value.
    pub const ZERO: Self = Self([0u8; 32]);

    /// All-ones value (`2^256 - 1`).
    pub const MAX: Self = Self([0xffu8; 32]);

    /// Build a `Uint256` from a `u128`, big-endian (low 16 bytes carry the
    /// value).
    pub const fn from_u128(v: u128) -> Self {
        let v_be = v.to_be_bytes();
        let mut bytes = [0u8; 32];
        let mut i = 0;
        while i < 16 {
            bytes[16 + i] = v_be[i];
            i += 1;
        }
        Self(bytes)
    }

    /// Add, returning `None` on overflow.
    #[inline]
    pub fn checked_add(self, other: Self) -> Option<Self> {
        let a = ruint::aliases::U256::from_be_bytes::<32>(self.0);
        let b = ruint::aliases::U256::from_be_bytes::<32>(other.0);
        a.checked_add(b).map(|r| Self(r.to_be_bytes::<32>()))
    }

    /// Subtract, returning `None` on underflow.
    #[inline]
    pub fn checked_sub(self, other: Self) -> Option<Self> {
        let a = ruint::aliases::U256::from_be_bytes::<32>(self.0);
        let b = ruint::aliases::U256::from_be_bytes::<32>(other.0);
        a.checked_sub(b).map(|r| Self(r.to_be_bytes::<32>()))
    }

    /// Multiply, returning `None` on overflow.
    #[inline]
    pub fn checked_mul(self, other: Self) -> Option<Self> {
        let a = ruint::aliases::U256::from_be_bytes::<32>(self.0);
        let b = ruint::aliases::U256::from_be_bytes::<32>(other.0);
        a.checked_mul(b).map(|r| Self(r.to_be_bytes::<32>()))
    }

    /// Integer division, truncating toward zero. Returns `None` only if
    /// `divisor` is zero.
    #[inline]
    pub fn checked_div(self, divisor: Self) -> Option<Self> {
        let a = ruint::aliases::U256::from_be_bytes::<32>(self.0);
        let b = ruint::aliases::U256::from_be_bytes::<32>(divisor.0);
        a.checked_div(b).map(|r| Self(r.to_be_bytes::<32>()))
    }

    /// Construct from 32 big-endian bytes.
    pub const fn from_be_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl PartialOrd for Uint256 {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Uint256 {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        // Lexicographic over big-endian bytes equals numerical order.
        self.0.cmp(&other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uint256_add_basic() {
        let a = Uint256::from_u128(500);
        let b = Uint256::from_u128(200);
        assert_eq!(a.checked_add(b), Some(Uint256::from_u128(700)));
    }

    #[test]
    fn uint256_add_overflow() {
        assert_eq!(Uint256::MAX.checked_add(Uint256::from_u128(200)), None);
    }

    #[test]
    fn uint256_sub_basic() {
        let a = Uint256::from_u128(500);
        let b = Uint256::from_u128(200);
        assert_eq!(a.checked_sub(b), Some(Uint256::from_u128(300)));
    }

    #[test]
    fn uint256_sub_underflow() {
        assert_eq!(
            Uint256::ZERO.checked_sub(Uint256::from_u128(200)),
            None,
            "subtracting from zero must return None, not wrap"
        );
    }

    #[test]
    fn uint256_round_trip_bytes() {
        let original = Uint256::from_u128(0xdead_beef_cafe_babe_u128);
        let restored = Uint256::from_be_bytes(original.0);
        assert_eq!(original, restored);
    }

    #[test]
    fn uint256_big_endian_wire_order() {
        // `0x1234` packs MSB-first (network byte order), matching the VAA
        // `amount` encoding.
        let v = Uint256::from_u128(0x1234);
        let mut expected = [0u8; 32];
        expected[30] = 0x12;
        expected[31] = 0x34;
        assert_eq!(v.0, expected);
    }

    #[test]
    fn uint256_ordering() {
        assert!(Uint256::from_u128(1) < Uint256::from_u128(2));
        assert!(Uint256::MAX > Uint256::from_u128(u128::MAX));
        assert!(Uint256::ZERO < Uint256::from_u128(1));
        // Higher MSB sorts higher even with smaller LSBs.
        let mut a_bytes = [0u8; 32];
        a_bytes[0] = 0x01;
        let a = Uint256::from_be_bytes(a_bytes);
        let b = Uint256::from_u128(u128::MAX);
        assert!(a > b);
    }

    #[test]
    fn uint256_add_then_sub_round_trips() {
        let start = Uint256::from_u128(500);
        let added = start.checked_add(Uint256::from_u128(200)).unwrap();
        assert_eq!(added, Uint256::from_u128(700));
        let restored = added.checked_sub(Uint256::from_u128(200)).unwrap();
        assert_eq!(restored, start);
    }
}
