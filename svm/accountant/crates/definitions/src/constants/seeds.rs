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

/// Seed for the authority PDA that signs all NoReplay CPIs: `[b"noreplay_authority"]`.
pub const NOREPLAY_AUTHORITY_SEED_PREFIX: &[u8] = b"noreplay_authority";

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
