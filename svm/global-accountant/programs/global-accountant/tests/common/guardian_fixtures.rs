//! Synthetic guardian fixtures for mollusk tests.
//!
//! Mirrors the inline helpers that submit_observations.rs maintains for the
//! inline `secp256k1_recover` path, but factored out so the real-Shim
//! integration tests can populate the Core Bridge `GuardianSet` PDA and the
//! Shim's `GuardianSignatures` PDA from the same deterministic key material.
//!
//! Wire layouts are pinned against
//! `svm/wormhole-core-shims/crates/definitions/src/zero_copy/{guardian_set,guardian_signatures}.rs`.
//! When that crate changes the offsets, regression tests against the on-disk
//! GuardianSet fixtures in the Shim's own crate flag the drift; this module
//! adopts the new offsets on the next pass.

use {
    libsecp256k1::{sign, Message, PublicKey, SecretKey},
    solana_account::Account,
    solana_pubkey::Pubkey,
};

/// 20-byte Ethereum-style address length, mirroring
/// `wormhole_svm_definitions::GUARDIAN_PUBKEY_LENGTH`.
pub const GUARDIAN_PUBKEY_LENGTH: usize = 20;
/// 66-byte on-wire signature record (1-byte guardian index + 64-byte r||s +
/// 1-byte recovery id). Mirrors `wormhole_svm_definitions::GUARDIAN_SIGNATURE_LENGTH`.
pub const GUARDIAN_SIGNATURE_LENGTH: usize = 66;
/// PDA seed for the Core Bridge `GuardianSet` account, matching
/// `wormhole_svm_definitions::GUARDIAN_SET_SEED`.
pub const GUARDIAN_SET_SEED: &[u8] = b"GuardianSet";

/// Anchor discriminator for the Shim's `GuardianSignatures` account:
/// `sha256("account:GuardianSignatures")[..8]`. Pinned here to avoid pulling
/// `sha2` into dev-deps just to recompute a constant at runtime.
///
/// Reproduce with: `echo -n "account:GuardianSignatures" | shasum -a 256`
/// → `cbb8829d710eb853...`
pub const GUARDIAN_SIGNATURES_DISCRIMINATOR: [u8; 8] =
    [0xcb, 0xb8, 0x82, 0x9d, 0x71, 0x0e, 0xb8, 0x53];

/// Minimum on-disk size of a Shim `GuardianSignatures` account:
/// discriminator (8) + refund recipient (32) + guardian set index BE (4) +
/// signature count LE (4) = 48 bytes. The trailing signatures bring the
/// total up to `MINIMUM_SIZE + n * GUARDIAN_SIGNATURE_LENGTH`.
pub const GUARDIAN_SIGNATURES_MIN_SIZE: usize = 48;

/// Deterministic guardian: secret key + derived 20-byte Ethereum address.
#[derive(Clone)]
pub struct Guardian {
    pub secret: SecretKey,
    pub eth_address: [u8; GUARDIAN_PUBKEY_LENGTH],
}

/// Build `count` deterministic guardians from a single byte seed. Two calls
/// with the same seed and count return the same keys, so tests can derive
/// PDA addresses and signatures off-line without persisting fixtures.
pub fn make_guardians(count: usize, seed: u8) -> Vec<Guardian> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut sk_bytes = [0u8; 32];
        sk_bytes[0] = seed;
        sk_bytes[1] = i as u8;
        // Bias the high bits so the scalar stays inside the secp256k1 group
        // order. `SecretKey::parse` rejects 0 and ≥ curve order; small-
        // magnitude bytes are safe.
        sk_bytes[31] = (i as u8).wrapping_add(1);

        let secret =
            SecretKey::parse(&sk_bytes).expect("deterministic seed inside secp256k1 group order");
        let public = PublicKey::from_secret_key(&secret);
        // `serialize()` emits 65 bytes: 0x04 prefix + 64 raw (X||Y). Strip
        // the prefix before keccak.
        let pk_uncompressed = public.serialize();
        let raw = &pk_uncompressed[1..];
        let hash = solana_keccak_hasher::hashv(&[raw]).to_bytes();
        let mut eth_address = [0u8; GUARDIAN_PUBKEY_LENGTH];
        eth_address.copy_from_slice(&hash[12..]);
        out.push(Guardian {
            secret,
            eth_address,
        });
    }
    out
}

/// Sign a 32-byte digest with a guardian secret. Returns the 65-byte wire
/// signature `r||s||recovery_id` accepted by both the inline
/// `secp256k1_recover` syscall and the Shim's `VerifyHash` ix.
pub fn sign_digest(guardian: &Guardian, digest: &[u8; 32]) -> [u8; 65] {
    let msg = Message::parse(digest);
    let (sig, rec) = sign(&msg, &guardian.secret);
    let sig_bytes = sig.serialize();
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig_bytes);
    out[64] = rec.serialize();
    out
}

/// Derive the canonical Core Bridge `GuardianSet` PDA for a given set index.
/// Mirrors `wormhole_svm_definitions::find_guardian_set_address`.
pub fn derive_guardian_set_pda(set_index: u32, core_bridge_program_id: &Pubkey) -> (Pubkey, u8) {
    let index_be = set_index.to_be_bytes();
    Pubkey::find_program_address(&[GUARDIAN_SET_SEED, &index_be], core_bridge_program_id)
}

/// Build a Core-Bridge-style `GuardianSet` account.
///
/// Layout (little-endian; pinned against
/// `wormhole_svm_definitions::zero_copy::GuardianSet`):
///
/// ```text
/// [gsi: u32 LE][keys_len: u32 LE][keys: 20*N][creation_time: u32 LE][expiration_time: u32 LE]
/// ```
///
/// Setting `expiration_time = 0` marks the set as never-expiring (the Shim
/// treats `expiration_time == 0 || timestamp <= expiration_time` as active).
pub fn guardian_set_account(
    set_index: u32,
    keys: &[[u8; GUARDIAN_PUBKEY_LENGTH]],
    creation_time: u32,
    expiration_time: u32,
    core_bridge_program_id: &Pubkey,
) -> Account {
    let mut data = Vec::with_capacity(8 + keys.len() * GUARDIAN_PUBKEY_LENGTH + 8);
    data.extend_from_slice(&set_index.to_le_bytes());
    data.extend_from_slice(&(keys.len() as u32).to_le_bytes());
    for key in keys {
        data.extend_from_slice(key);
    }
    data.extend_from_slice(&creation_time.to_le_bytes());
    data.extend_from_slice(&expiration_time.to_le_bytes());
    Account {
        lamports: 1_000_000_000,
        data,
        owner: *core_bridge_program_id,
        executable: false,
        rent_epoch: 0,
    }
}

/// Build a Shim `GuardianSignatures` account fixture.
///
/// Layout (pinned against
/// `wormhole_svm_definitions::zero_copy::GuardianSignatures`):
///
/// ```text
/// [disc: 8 = GUARDIAN_SIGNATURES_DISCRIMINATOR]
/// [refund_recipient: 32]
/// [gsi: u32 BE]    // big-endian — matches Core Bridge derivation
/// [sigs_len: u32 LE]
/// [signatures: 66 * N]
/// ```
///
/// Each entry in `signatures` is `(guardian_index, sig65)`; the Shim asserts
/// guardian indices are strictly increasing, so callers must sort. Owner is
/// set to the Shim program ID so the Shim's owner check in `VerifyHash`
/// passes.
pub fn guardian_signatures_account(
    set_index: u32,
    refund_recipient: &Pubkey,
    signatures: &[(u8, [u8; 65])],
    shim_program_id: &Pubkey,
) -> Account {
    let mut data = Vec::with_capacity(
        GUARDIAN_SIGNATURES_MIN_SIZE + signatures.len() * GUARDIAN_SIGNATURE_LENGTH,
    );
    data.extend_from_slice(&GUARDIAN_SIGNATURES_DISCRIMINATOR);
    data.extend_from_slice(refund_recipient.as_ref());
    // guardian_set_index is stored big-endian — the Shim treats this slice
    // as a Core Bridge seed for guardian-set PDA derivation.
    data.extend_from_slice(&set_index.to_be_bytes());
    data.extend_from_slice(&(signatures.len() as u32).to_le_bytes());
    for (idx, sig) in signatures {
        data.push(*idx);
        data.extend_from_slice(&sig[..]);
    }
    Account {
        lamports: 5_000_000_000,
        data,
        owner: *shim_program_id,
        executable: false,
        rent_epoch: 0,
    }
}
