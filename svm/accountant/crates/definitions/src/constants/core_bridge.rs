//! Wormhole Core Bridge program ID.

use crate::primitives::Pubkey;
use const_crypto::bs58;

/// Core Bridge program ID, from `BRIDGE_ADDRESS` at compile time (same variable
/// `wormhole-svm-definitions` `from-env` reads). Set per target network in `justfile`.
/// `close_pending` checks `GuardianSet` ownership against it. Without the check, a forged
/// expired set could close a pending PDA.
pub const CORE_BRIDGE_PROGRAM_ID: Pubkey = bs58::decode_pubkey(env!("BRIDGE_ADDRESS"));

#[cfg(test)]
mod tests {
    use super::*;

    /// `BRIDGE_ADDRESS` must name a known Wormhole Core Bridge deployment.
    #[test]
    fn is_known_wormhole_svm_definitions_deployment() {
        use wormhole_svm_definitions::solana::{devnet, localnet, mainnet};
        let known = [
            mainnet::CORE_BRIDGE_PROGRAM_ID_ARRAY,
            devnet::CORE_BRIDGE_PROGRAM_ID_ARRAY,
            localnet::CORE_BRIDGE_PROGRAM_ID_ARRAY,
        ];
        assert!(known.contains(&CORE_BRIDGE_PROGRAM_ID));
    }
}
