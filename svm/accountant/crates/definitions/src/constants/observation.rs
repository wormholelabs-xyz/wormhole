//! Guardian observation signing prefix.

/// Domain-separation prefix for guardian observation signatures:
/// `keccak256(prefix ‖ observation)`. Must equal the node's `SubmitObservationPrefix`
/// (`node/pkg/accountant/submit_obs.go`). Whitepaper 0009 requires a >= 32-byte prefix.
pub const SUBMIT_OBSERVATION_PREFIX: &[u8] = b"acct_sub_obsfig_000000000000000000|";
