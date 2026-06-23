//! Guardian observation signing prefix.

/// Domain-separation prefix guardians prepend when signing a global-accountant
/// observation: `keccak256(prefix ‖ observation)`. Byte-for-byte identical to
/// the node's `SubmitObservationPrefix` (`node/pkg/accountant/submit_obs.go`).
///
/// The prefix makes an accountant attestation a distinct signing domain from a
/// VAA signature (Wormhole whitepaper 0009 requires a >= 32-byte prefix per
/// signed message type). That decoupling is what lets the accountant gate VAA
/// issuance: a guardian can vouch an observation to the accountant without that
/// vouch being reusable as — or implied by — its VAA signature.
pub const SUBMIT_OBSERVATION_PREFIX: &[u8] = b"acct_sub_obsfig_000000000000000000|";
