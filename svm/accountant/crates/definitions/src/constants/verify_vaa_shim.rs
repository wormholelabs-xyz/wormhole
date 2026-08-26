//! Verify VAA Shim program ID and `verify_hash` selector / wire size.

use crate::primitives::Pubkey;
use const_crypto::bs58;

/// Verify VAA Shim program ID, from `VERIFY_VAA_SHIM_PROGRAM_ID` at compile time (same
/// variable `wormhole-svm-definitions` `from-env` reads). Set per target network in `justfile`.
pub const VERIFY_VAA_SHIM_PROGRAM_ID: Pubkey =
    bs58::decode_pubkey(env!("VERIFY_VAA_SHIM_PROGRAM_ID"));

/// `verify_hash` discriminator: first 8 bytes of `sha256("global:verify_hash")`.
pub const VERIFY_HASH_SELECTOR: [u8; 8] = [22, 152, 160, 69, 241, 148, 14, 124];

/// `verify_hash` data length: selector (8) + guardian-set bump (1) + digest (32).
pub const VERIFY_HASH_DATA_LEN: usize = 8 + 1 + 32;

#[cfg(test)]
mod tests {
    use super::*;

    /// `VERIFY_VAA_SHIM_PROGRAM_ID` must name a known shim deployment.
    #[test]
    fn program_id_is_known_wormhole_svm_definitions_deployment() {
        use wormhole_svm_definitions::solana::{devnet, localnet, mainnet};
        let known = [
            mainnet::VERIFY_VAA_SHIM_PROGRAM_ID_ARRAY,
            devnet::VERIFY_VAA_SHIM_PROGRAM_ID_ARRAY,
            localnet::VERIFY_VAA_SHIM_PROGRAM_ID_ARRAY,
        ];
        assert!(known.contains(&VERIFY_VAA_SHIM_PROGRAM_ID));
    }

    /// Same derivation `wormhole-svm-shim` uses for its `VERIFY_HASH_SELECTOR`.
    #[test]
    fn selector_is_anchor_discriminator_of_verify_hash() {
        assert_eq!(
            VERIFY_HASH_SELECTOR,
            wormhole_svm_definitions::make_anchor_discriminator(b"global:verify_hash")
        );
    }
}
