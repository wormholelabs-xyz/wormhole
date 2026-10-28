//! Accountant program addresses, one per program family.

use const_crypto::bs58;

use crate::primitives::Pubkey;

/// WTT accountant program address, from `GLOBAL_ACCOUNTANT_PROGRAM_ID` at compile time.
/// Set per target network in `justfile`.
///
/// The backfill `.so` and the operational `.so` share one program account across the
/// migration `solana program upgrade`, so both must `declare_id!` this address: Anchor
/// aborts on a declared/runtime mismatch, and every PDA derives from the runtime program id.
pub const GLOBAL_ACCOUNTANT_PROGRAM_ID: Pubkey =
    bs58::decode_pubkey(env!("GLOBAL_ACCOUNTANT_PROGRAM_ID"));

/// NTT accountant program address, from `NTT_GLOBAL_ACCOUNTANT_PROGRAM_ID` at compile time.
/// Set per target network in `justfile`. Same upgrade-in-place constraint as the WTT address.
pub const NTT_GLOBAL_ACCOUNTANT_PROGRAM_ID: Pubkey =
    bs58::decode_pubkey(env!("NTT_GLOBAL_ACCOUNTANT_PROGRAM_ID"));

/// `[u8; 32] == [u8; 32]` is not const on stable, hence the byte loop.
pub const fn pubkey_eq(a: &Pubkey, b: &Pubkey) -> bool {
    let mut i = 0;
    while i < 32 {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pubkey_eq_is_exact_over_all_bytes() {
        assert!(pubkey_eq(
            &GLOBAL_ACCOUNTANT_PROGRAM_ID,
            &GLOBAL_ACCOUNTANT_PROGRAM_ID
        ));
        assert!(!pubkey_eq(
            &GLOBAL_ACCOUNTANT_PROGRAM_ID,
            &NTT_GLOBAL_ACCOUNTANT_PROGRAM_ID
        ));
        for i in 0..32 {
            let mut flipped = GLOBAL_ACCOUNTANT_PROGRAM_ID;
            flipped[i] ^= 1;
            assert!(
                !pubkey_eq(&GLOBAL_ACCOUNTANT_PROGRAM_ID, &flipped),
                "byte {i}"
            );
        }
    }
}
