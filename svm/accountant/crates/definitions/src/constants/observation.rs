//! Guardian observation signing prefixes.
//!
//! Domain-separation prefixes guardians prepend when signing a global-accountant
//! observation: `keccak256(prefix ‖ observation)`. Byte-for-byte identical to the
//! node's `SubmitObservationPrefix` / `NttSubmitObservationPrefix`
//! (`node/pkg/accountant/submit_obs.go`).
//!
//! The prefix makes an accountant attestation a distinct signing domain from a
//! VAA signature (Wormhole whitepaper 0009 requires a >= 32-byte prefix per
//! signed message type). That decoupling is what lets the accountant gate VAA
//! issuance: a guardian can vouch an observation to the accountant without that
//! vouch being reusable as — or implied by — its VAA signature. WTT and NTT use
//! distinct prefixes so an observation for one product cannot be replayed as an
//! observation for the other.

/// WTT (Token Bridge) observation signing prefix.
pub const SUBMIT_OBSERVATION_PREFIX: &[u8] = b"acct_sub_obsfig_000000000000000000|";

/// NTT observation signing prefix.
pub const NTT_SUBMIT_OBSERVATION_PREFIX: &[u8] = b"ntt_acct_sub_obsfig_00000000000000|";
