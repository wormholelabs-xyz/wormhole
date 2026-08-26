//! `keccak256` helpers. `solana-keccak-hasher` runs on both SBF and host targets.

use solana_keccak_hasher::hashv;

/// `keccak256(data)`.
pub(crate) fn keccak256(data: &[u8]) -> [u8; 32] {
    hashv(&[data]).to_bytes()
}

/// `keccak256(keccak256(body))`: the VAA digest that guardians sign and the Shim checks.
pub fn double_keccak256(body: &[u8]) -> [u8; 32] {
    let inner = keccak256(body);
    keccak256(&inner)
}

/// `keccak256(prefix ‖ tx_hash ‖ body)`: the observation signing digest. Equals the node's
/// `vaa.MessageSigningDigest(prefix, observation)` in `sdk/vaa/structs.go`.
///
/// SECURITY: single keccak with prefix. Keep distinct from [`double_keccak256`] so an
/// observation signature cannot serve as a VAA signature.
pub fn observation_signing_digest(prefix: &[u8], tx_hash: &[u8; 32], body: &[u8]) -> [u8; 32] {
    hashv(&[prefix, tx_hash, body]).to_bytes()
}
