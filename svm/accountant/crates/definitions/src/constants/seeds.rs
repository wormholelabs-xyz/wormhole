//! PDA seed prefixes for every account type.

/// Seed prefix for [`crate::PendingObservationsLayout`]:
/// `(b"pending", chain_be, emitter, sequence_be, digest)`. The digest seed gives
/// fork observations their own bucket.
pub const PENDING_OBSERVATIONS_SEED_PREFIX: &[u8] = b"pending";

/// Seed prefix for [`crate::BalanceAccountLayout`]:
/// `(b"account", chain_be, token_chain_be, token_address)`.
pub const ACCOUNT_SEED_PREFIX: &[u8] = b"account";

/// Seed prefix for [`crate::ChainRegistrationLayout`]: `(b"chain_registration", chain_be)`.
pub const CHAIN_REGISTRATION_SEED_PREFIX: &[u8] = b"chain_registration";

/// Seed prefix for [`crate::ModifyBalanceLayout`]: `(b"modify_balance", sequence_be)`.
/// PDA existence is the governance-path replay protection.
pub const MODIFY_BALANCE_SEED_PREFIX: &[u8] = b"modify_balance";

/// Seed prefix for [`crate::RelayerChainRegistrationLayout`]:
/// `(b"relayer_chain_registration", chain_be)`. NTT's analogue of
/// [`CHAIN_REGISTRATION_SEED_PREFIX`]; the distinct prefix gives the relayer
/// registry its own address space.
pub const RELAYER_CHAIN_REGISTRATION_SEED_PREFIX: &[u8] = b"relayer_chain_registration";

/// Seed prefix for [`crate::TransceiverHubLayout`]:
/// `(b"transceiver_hub", chain_be, address)`.
pub const TRANSCEIVER_HUB_SEED_PREFIX: &[u8] = b"transceiver_hub";

/// Seed prefix for [`crate::TransceiverPeerLayout`]:
/// `(b"transceiver_peer", chain_be, address, dest_chain_be)`.
pub const TRANSCEIVER_PEER_SEED_PREFIX: &[u8] = b"transceiver_peer";

/// Seed for the authority PDA that signs all NoReplay CPIs: `[b"noreplay_authority"]`.
pub const NOREPLAY_AUTHORITY_SEED_PREFIX: &[u8] = b"noreplay_authority";

/// Seed for the program upgrade authority PDA: `[b"upgrade"]`. Matches the Core Bridge
/// and Token Bridge upgrade authority seed.
pub const UPGRADE_AUTHORITY_SEED_PREFIX: &[u8] = b"upgrade";

/// Core Bridge `GuardianSet` seed prefix: `(b"GuardianSet", guardian_set_index_be)`.
/// Owner is `CORE_BRIDGE_PROGRAM_ID`.
pub const GUARDIAN_SET_SEED: &[u8] = b"GuardianSet";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guardian_set_seed_matches_wormhole_svm_definitions() {
        assert_eq!(
            GUARDIAN_SET_SEED,
            wormhole_svm_definitions::GUARDIAN_SET_SEED
        );
    }
}
