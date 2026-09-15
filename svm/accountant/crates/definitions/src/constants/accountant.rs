//! Accountant program address.

use crate::primitives::Pubkey;
use const_crypto::bs58;

/// Program address, from `GLOBAL_ACCOUNTANT_PROGRAM_ID` at compile time. Set per
/// target network in `justfile`.
///
/// The backfill `.so` and the operational `.so` share one program account across
/// the migration `solana program upgrade`, so both must `declare_id!` this
/// address: Anchor aborts on a declared/runtime mismatch, and every PDA derives
/// from the runtime program id.
pub const GLOBAL_ACCOUNTANT_PROGRAM_ID: Pubkey =
    bs58::decode_pubkey(env!("GLOBAL_ACCOUNTANT_PROGRAM_ID"));

/// `[u8; 32] == [u8; 32]` is not const on stable, hence the byte loop.
pub const fn is_global_accountant_program_id(id: &Pubkey) -> bool {
    let mut i = 0;
    while i < 32 {
        if id[i] != GLOBAL_ACCOUNTANT_PROGRAM_ID[i] {
            return false;
        }
        i += 1;
    }
    true
}
