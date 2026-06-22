//! PDA seed prefixes for every account type.

/// PDA seed prefix for [`crate::PendingObservationsLayout`]. Full tuple:
/// `(b"pending", chain_be, emitter, sequence_be, digest)`. The digest suffix
/// lets fork/reorg observations accumulate in sibling buckets and binds each
/// bucket to its digest, so no runtime digest-equality check is needed.
pub const PENDING_OBSERVATIONS_SEED_PREFIX: &[u8] = b"pending";

/// PDA seed prefix for [`crate::BalanceAccountLayout`]. Full tuple:
/// `(b"account", chain_be, token_chain_be, token_address)`. Big-endian chain
/// fields match the VAA wire format and the other seed derivations.
pub const ACCOUNT_SEED_PREFIX: &[u8] = b"account";

/// PDA seed prefix for [`crate::ChainRegistrationLayout`]. Full tuple:
/// `(b"chain_registration", chain_be)`.
pub const CHAIN_REGISTRATION_SEED_PREFIX: &[u8] = b"chain_registration";

/// PDA seed prefix for [`crate::ModificationLayout`]. Full tuple:
/// `(b"modification", sequence_be)`. Existence of this PDA enforces replay
/// protection on the governance path.
pub const MODIFICATION_SEED_PREFIX: &[u8] = b"modification";

/// PDA seed prefix for [`crate::RelayerChainRegistrationLayout`]. Full tuple:
/// `(b"relayer_chain_registration", chain_be)`. NTT's analogue of
/// [`CHAIN_REGISTRATION_SEED_PREFIX`]; kept distinct so the relayer and Token
/// Bridge registries never share an address.
pub const RELAYER_CHAIN_REGISTRATION_SEED_PREFIX: &[u8] = b"relayer_chain_registration";

/// PDA seed prefix for [`crate::TransceiverHubLayout`]. Full tuple:
/// `(b"transceiver_hub", chain_be, address)`.
pub const TRANSCEIVER_HUB_SEED_PREFIX: &[u8] = b"transceiver_hub";

/// PDA seed prefix for [`crate::TransceiverPeerLayout`]. Full tuple:
/// `(b"transceiver_peer", chain_be, address, dest_chain_be)`.
pub const TRANSCEIVER_PEER_SEED_PREFIX: &[u8] = b"transceiver_peer";

/// PDA seed prefix for the global-accountant authority that signs all
/// `solana-noreplay` CPIs. Full tuple: `[b"noreplay-authority"]`. One global
/// authority suffices because the noreplay namespace (`chain_be ‖ emitter`)
/// already segregates per-emitter sequence spaces.
pub const NOREPLAY_AUTHORITY_SEED_PREFIX: &[u8] = b"noreplay-authority";
