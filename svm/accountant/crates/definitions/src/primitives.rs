//! Primitive value types: the address alias and the on-disk 256-bit integer.

use bytemuck::{Pod, Zeroable};

/// 32-byte address, layout-compatible with `solana_address::Address`.
pub type Pubkey = [u8; 32];

/// 256-bit unsigned integer, 32 big-endian bytes on disk (VAA `amount` wire order).
/// Derived `Ord` compares bytes lexicographically, which is numeric order for big-endian.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Pod, Zeroable)]
pub struct Uint256(pub [u8; 32]);

impl From<u128> for Uint256 {
    fn from(v: u128) -> Self {
        Self::from_u128(v)
    }
}

impl Uint256 {
    /// Number of 64-bit limbs.
    const LIMBS: usize = 4;

    /// All-zero value.
    pub const ZERO: Self = Self([0u8; 32]);

    /// All-ones value (`2^256 - 1`).
    pub const MAX: Self = Self([0xffu8; 32]);

    /// `v` in the low 16 bytes, big-endian; high 16 bytes zero.
    pub fn from_u128(v: u128) -> Self {
        let mut bytes = [0u8; 32];
        bytes[16..].copy_from_slice(&v.to_be_bytes());
        Self(bytes)
    }

    /// From 32 big-endian bytes.
    pub const fn from_be_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Big-endian limbs: index 0 is the most significant.
    #[inline]
    fn limbs(self) -> [u64; Self::LIMBS] {
        let mut out = [0u64; Self::LIMBS];
        for (i, limb) in out.iter_mut().enumerate() {
            let mut chunk = [0u8; 8];
            chunk.copy_from_slice(&self.0[i * 8..(i + 1) * 8]);
            *limb = u64::from_be_bytes(chunk);
        }
        out
    }

    #[inline]
    fn from_limbs(limbs: [u64; Self::LIMBS]) -> Self {
        let mut bytes = [0u8; 32];
        for (i, limb) in limbs.iter().enumerate() {
            bytes[i * 8..(i + 1) * 8].copy_from_slice(&limb.to_be_bytes());
        }
        Self(bytes)
    }

    /// `None` on overflow.
    ///
    /// SECURITY: postcondition `result >= self` and `result >= other`.
    #[inline]
    pub fn checked_add(self, other: Self) -> Option<Self> {
        let a = self.limbs();
        let b = other.limbs();
        let mut out = [0u64; Self::LIMBS];
        let mut carry = 0u128;
        // Least significant limb first. `u64 + u64 + carry` fits in `u128`;
        // the low 64 bits are the limb, the rest is the carry.
        for i in (0..Self::LIMBS).rev() {
            let sum = u128::from(a[i]) + u128::from(b[i]) + carry;
            out[i] = sum as u64;
            carry = sum >> 64;
        }
        if carry != 0 {
            return None;
        }
        let result = Self::from_limbs(out);
        debug_assert!(result >= self);
        debug_assert!(result >= other);
        Some(result)
    }

    /// `None` on underflow.
    ///
    /// SECURITY: postcondition `result <= self`.
    #[inline]
    pub fn checked_sub(self, other: Self) -> Option<Self> {
        let a = self.limbs();
        let b = other.limbs();
        let mut out = [0u64; Self::LIMBS];
        let mut borrow = 0i128;
        // Least significant limb first. `u64 - u64 - borrow` fits in `i128`;
        // a negative result borrows one from the next limb.
        for i in (0..Self::LIMBS).rev() {
            let diff = i128::from(a[i]) - i128::from(b[i]) - borrow;
            out[i] = diff as u64;
            borrow = i128::from(diff < 0);
        }
        if borrow != 0 {
            return None;
        }
        let result = Self::from_limbs(out);
        debug_assert!(result <= self);
        Some(result)
    }

    /// Big-endian bit accessor: index 0 is the most significant bit.
    #[inline]
    fn bit(self, index: usize) -> bool {
        (self.0[index / 8] >> (7 - index % 8)) & 1 == 1
    }

    /// `self * 2 + carry_in`, truncated to 256 bits.
    #[inline]
    fn shl1(self, carry_in: bool) -> Self {
        let mut out = [0u8; 32];
        let mut carry = u8::from(carry_in);
        for i in (0..32).rev() {
            out[i] = (self.0[i] << 1) | carry;
            carry = self.0[i] >> 7;
        }
        Self(out)
    }

    /// `self - other` modulo `2^256`.
    #[inline]
    fn wrapping_sub(self, other: Self) -> Self {
        let a = self.limbs();
        let b = other.limbs();
        let mut out = [0u64; Self::LIMBS];
        let mut borrow = 0i128;
        for i in (0..Self::LIMBS).rev() {
            let diff = i128::from(a[i]) - i128::from(b[i]) - borrow;
            out[i] = diff as u64;
            borrow = i128::from(diff < 0);
        }
        Self::from_limbs(out)
    }

    /// `None` on overflow.
    ///
    /// SECURITY: postcondition `result` divided by either nonzero operand
    /// recovers the other exactly (checked in debug).
    #[inline]
    pub fn checked_mul(self, other: Self) -> Option<Self> {
        // Little-endian limb copies: index 0 least significant.
        let mut a = self.limbs();
        a.reverse();
        let mut b = other.limbs();
        b.reverse();
        // Schoolbook multiply: 512-bit product in 8 little-endian limbs.
        // `u64 * u64 + u64 + carry` fits in `u128`.
        let mut prod = [0u64; 2 * Self::LIMBS];
        for i in 0..Self::LIMBS {
            let mut carry = 0u128;
            for j in 0..Self::LIMBS {
                let cur = u128::from(prod[i + j]) + u128::from(a[i]) * u128::from(b[j]) + carry;
                prod[i + j] = cur as u64;
                carry = cur >> 64;
            }
            prod[i + Self::LIMBS] = carry as u64;
        }
        // A nonzero high half means the product needs more than 256 bits.
        if prod[Self::LIMBS..].iter().any(|&limb| limb != 0) {
            return None;
        }
        let mut out = [0u64; Self::LIMBS];
        for (i, limb) in out.iter_mut().enumerate() {
            *limb = prod[Self::LIMBS - 1 - i];
        }
        let result = Self::from_limbs(out);
        debug_assert!(other == Self::ZERO || result.checked_div(other) == Some(self));
        debug_assert!(self == Self::ZERO || result.checked_div(self) == Some(other));
        Some(result)
    }

    /// Truncating division. `None` only when `divisor` is zero.
    ///
    /// SECURITY: postcondition `result <= self`.
    #[inline]
    pub fn checked_div(self, divisor: Self) -> Option<Self> {
        if divisor == Self::ZERO {
            return None;
        }
        // Binary long division, most significant dividend bit first; exactly
        // 256 iterations. Invariant at loop entry: rem < divisor.
        let mut quotient = [0u8; 32];
        let mut rem = Self::ZERO;
        for index in 0..256 {
            // `2*rem + bit` can need bit 256 when `divisor > 2^255`: capture
            // it before the truncating shift.
            let shifted_out = rem.bit(0);
            rem = rem.shl1(self.bit(index));
            if shifted_out || rem >= divisor {
                // With bit 256 set the shifted remainder exceeds any divisor;
                // wrapping subtraction cancels that bit and the result fits.
                rem = rem.wrapping_sub(divisor);
                quotient[index / 8] |= 1 << (7 - index % 8);
            }
        }
        let result = Self(quotient);
        debug_assert!(result <= self);
        debug_assert!(rem < divisor);
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruint::aliases::U256;

    fn reference(v: Uint256) -> U256 {
        U256::from_be_bytes(v.0)
    }

    fn from_reference(v: U256) -> Uint256 {
        Uint256(v.to_be_bytes())
    }

    fn pow2(shift: usize) -> Uint256 {
        from_reference(U256::from(1u8) << shift)
    }

    fn edge_values() -> std::vec::Vec<Uint256> {
        let one = Uint256::from_u128(1);
        let mut out = std::vec![Uint256::ZERO, one, Uint256::MAX];
        for shift in [63usize, 64, 127, 128, 191, 192, 255] {
            let p = reference(pow2(shift));
            out.push(from_reference(p));
            out.push(from_reference(p - U256::from(1u8)));
            if p < U256::MAX {
                out.push(from_reference(p + U256::from(1u8)));
            }
        }
        out.push(from_reference(U256::MAX - U256::from(1u8)));
        out.push(Uint256::from_u128(u64::MAX as u128));
        out.push(Uint256::from_u128(u128::MAX));
        out
    }

    fn assert_matches_reference(a: Uint256, b: Uint256) {
        assert_eq!(
            a.checked_add(b),
            reference(a).checked_add(reference(b)).map(from_reference),
            "add {a:?} + {b:?}"
        );
        assert_eq!(
            a.checked_sub(b),
            reference(a).checked_sub(reference(b)).map(from_reference),
            "sub {a:?} - {b:?}"
        );
        assert_eq!(
            a.checked_mul(b),
            reference(a).checked_mul(reference(b)).map(from_reference),
            "mul {a:?} * {b:?}"
        );
        assert_eq!(
            a.checked_div(b),
            reference(a).checked_div(reference(b)).map(from_reference),
            "div {a:?} / {b:?}"
        );
        assert_eq!(
            a.cmp(&b),
            reference(a).cmp(&reference(b)),
            "cmp {a:?} {b:?}"
        );
    }

    #[test]
    fn matches_ruint_on_edge_pairs() {
        let edges = edge_values();
        for &a in &edges {
            for &b in &edges {
                assert_matches_reference(a, b);
            }
        }
    }

    #[test]
    fn matches_ruint_on_random_pairs() {
        const ITERATIONS: usize = 20_000;
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next_u64 = move || {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545_F491_4F6C_DD1D)
        };
        let mut random = |limbs_set: usize| {
            let mut bytes = [0u8; 32];
            for limb in (Uint256::LIMBS - limbs_set)..Uint256::LIMBS {
                bytes[limb * 8..(limb + 1) * 8].copy_from_slice(&next_u64().to_be_bytes());
            }
            Uint256(bytes)
        };
        for i in 0..ITERATIONS {
            let a = random(1 + i % Uint256::LIMBS);
            let b = random(1 + (i / 7) % Uint256::LIMBS);
            assert_matches_reference(a, b);
        }
    }

    #[test]
    fn known_answers() {
        let one = Uint256::from_u128(1);
        let mut almost_max = Uint256::MAX;
        almost_max.0[31] = 0xfe;
        let low_limbs_full = Uint256::from_u128(u128::MAX);

        let adds: [(&str, Uint256, Uint256, Option<Uint256>); 5] = [
            ("max plus one overflows", Uint256::MAX, one, None),
            (
                "max plus zero",
                Uint256::MAX,
                Uint256::ZERO,
                Some(Uint256::MAX),
            ),
            (
                "500 plus 200",
                Uint256::from_u128(500),
                Uint256::from_u128(200),
                Some(Uint256::from_u128(700)),
            ),
            ("carry into top limb", almost_max, one, Some(Uint256::MAX)),
            ("carry across limb 2", low_limbs_full, one, Some(pow2(128))),
        ];
        for (name, a, b, expected) in adds {
            assert_eq!(a.checked_add(b), expected, "{name}");
        }

        let subs: [(&str, Uint256, Uint256, Option<Uint256>); 5] = [
            ("zero minus one underflows", Uint256::ZERO, one, None),
            (
                "zero minus zero",
                Uint256::ZERO,
                Uint256::ZERO,
                Some(Uint256::ZERO),
            ),
            (
                "500 minus 200",
                Uint256::from_u128(500),
                Uint256::from_u128(200),
                Some(Uint256::from_u128(300)),
            ),
            ("borrow across limb 2", pow2(128), one, Some(low_limbs_full)),
            (
                "borrow across limb 3",
                pow2(64),
                one,
                Some(Uint256::from_u128(u64::MAX as u128)),
            ),
        ];
        for (name, a, b, expected) in subs {
            assert_eq!(a.checked_sub(b), expected, "{name}");
        }

        let muls: [(&str, Uint256, Uint256, Option<Uint256>); 5] = [
            (
                "500 times 200",
                Uint256::from_u128(500),
                Uint256::from_u128(200),
                Some(Uint256::from_u128(100_000)),
            ),
            ("max times one", Uint256::MAX, one, Some(Uint256::MAX)),
            (
                "max times two overflows",
                Uint256::MAX,
                Uint256::from_u128(2),
                None,
            ),
            ("carry across halves", pow2(128), pow2(127), Some(pow2(255))),
            ("pow2 overflow", pow2(128), pow2(128), None),
        ];
        for (name, a, b, expected) in muls {
            assert_eq!(a.checked_mul(b), expected, "{name}");
        }

        let divs: [(&str, Uint256, Uint256, Option<Uint256>); 6] = [
            ("divisor zero", one, Uint256::ZERO, None),
            ("zero dividend", Uint256::ZERO, Uint256::MAX, Some(Uint256::ZERO)),
            (
                "500 over 200 truncates",
                Uint256::from_u128(500),
                Uint256::from_u128(200),
                Some(Uint256::from_u128(2)),
            ),
            ("max over one", Uint256::MAX, one, Some(Uint256::MAX)),
            ("max over max", Uint256::MAX, Uint256::MAX, Some(one)),
            ("top-bit divisor", Uint256::MAX, pow2(255), Some(one)),
        ];
        for (name, a, b, expected) in divs {
            assert_eq!(a.checked_div(b), expected, "{name}");
        }

        assert!(Uint256::from_u128(1) < Uint256::from_u128(2));
        assert!(Uint256::MAX > low_limbs_full);
        assert!(pow2(128) > low_limbs_full);
        assert!(pow2(255) > pow2(254));

        let v = Uint256::from_u128(0x1234);
        let mut wire = [0u8; 32];
        wire[30] = 0x12;
        wire[31] = 0x34;
        assert_eq!(v.0, wire);
        assert_eq!(reference(v), U256::from(0x1234u32));
    }
}
