//! `TrimmedAmount` normalization, as CosmWasm `normalize_transfer_amount`.

use crate::primitives::Uint256;

/// Decimals every amount is normalized to.
pub const TRIMMED_DECIMALS: u8 = 8;

/// Largest `exp` with `10^exp < 2^256`
const MAX_POW10_EXP: u8 = 77;

/// `(decimals, amount)` at [`TRIMMED_DECIMALS`]: truncating division down, multiplication
/// up. `None` where wormchain fails (`exp > MAX_POW10_EXP`). `u128` is exact: see the const
/// asserts below.
pub fn normalize_trimmed_amount(decimals: u8, amount: u64) -> Option<Uint256> {
    let amount = u128::from(amount);
    let normalized = match decimals.cmp(&TRIMMED_DECIMALS) {
        core::cmp::Ordering::Equal => amount,
        core::cmp::Ordering::Greater => {
            let exp = decimals - TRIMMED_DECIMALS;
            if exp > MAX_POW10_EXP {
                return None;
            }
            match 10u128.checked_pow(u32::from(exp)) {
                Some(divisor) => amount / divisor,
                None => 0,
            }
        }
        core::cmp::Ordering::Less => {
            amount.checked_mul(10u128.pow(u32::from(TRIMMED_DECIMALS - decimals)))?
        }
    };
    Some(Uint256::from_u128(normalized))
}

const _: () = {
    // Scale-up fits `u128`; a `u64` over `10^20` is `0`.
    assert!(u64::MAX as u128 <= u128::MAX / 10u128.pow(TRIMMED_DECIMALS as u32));
    assert!(10u128.checked_pow(20).unwrap() > u64::MAX as u128);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_table() {
        let cases: [(&str, u8, u64, Option<Uint256>); 9] = [
            (
                "3 to 8 scales up",
                3,
                1000,
                Some(Uint256::from_u128(100_000_000)),
            ),
            (
                "18 to 8 truncates",
                18,
                1_000_000_000_000_000_000,
                Some(Uint256::from_u128(100_000_000)),
            ),
            ("18 to 8 below one unit", 18, 1, Some(Uint256::ZERO)),
            ("identity at 8", 8, 12_345, Some(Uint256::from_u128(12_345))),
            ("0 to 8", 0, 5, Some(Uint256::from_u128(500_000_000))),
            (
                "u64::MAX at 0 decimals",
                0,
                u64::MAX,
                Some(Uint256::from_u128((u64::MAX as u128) * 100_000_000)),
            ),
            ("wormchain pow boundary", 85, 1, Some(Uint256::ZERO)),
            ("past wormchain pow boundary", 86, 1, None),
            ("u8 ceiling", 255, u64::MAX, None),
        ];
        for (name, decimals, amount, expected) in cases {
            assert_eq!(
                normalize_trimmed_amount(decimals, amount),
                expected,
                "{name}"
            );
        }
    }
}
