//! `keccak256` helpers shared across handlers.
//!
//! `solana-keccak-hasher` (with the `sha3` feature) provides a real
//! implementation on both targets — the syscall on-chain, a software
//! fallback via `sha3` off-chain — so unlike pinocchio's raw syscall wrapper
//! (which only worked on-chain and needed a host `unreachable!()` stub), no
//! cfg-gating is needed here at all (migration plan §2e).

use solana_keccak_hasher::hashv;

/// `keccak256(data)`.
pub(crate) fn keccak256(data: &[u8]) -> [u8; 32] {
    hashv(&[data]).to_bytes()
}

/// `keccak256(keccak256(body))` — the Wormhole VAA digest convention used by
/// guardian signing and the Verify VAA Shim's `VerifyHash`.
pub fn double_keccak256(body: &[u8]) -> [u8; 32] {
    let inner = keccak256(body);
    keccak256(&inner)
}

/// `keccak256(prefix ‖ tx_hash ‖ body)` — the guardian observation signing
/// digest. Mirrors the node's `vaa.MessageSigningDigest(prefix, observation)`
/// (`sdk/vaa/structs.go`): a *single* keccak over the domain-separation prefix
/// followed by the observation bytes, where the observation is `tx_hash` (the
/// source-chain transaction id, audit-only) concatenated with the canonical VAA
/// `body`.
///
/// This is deliberately NOT [`double_keccak256`]: that produces the VAA-body
/// digest, which keys dedup/quorum state and verifies VAA signatures on the
/// `submit_vaas` path. The prefixed single-keccak here is the *observation*
/// attestation domain — the two must stay distinct so an observation signature
/// is never interchangeable with a VAA signature.
pub fn observation_signing_digest(prefix: &[u8], tx_hash: &[u8; 32], body: &[u8]) -> [u8; 32] {
    hashv(&[prefix, tx_hash, body]).to_bytes()
}
