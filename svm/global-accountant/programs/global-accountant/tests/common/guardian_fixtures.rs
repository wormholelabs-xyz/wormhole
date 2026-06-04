//! Synthetic guardian fixtures for the real-Shim mollusk tests: deterministic
//! key material plus Core Bridge `GuardianSet` and Shim `GuardianSignatures`
//! account builders. Wire layouts are pinned against
//! `svm/wormhole-core-shims/crates/definitions/src/zero_copy/{guardian_set,guardian_signatures}.rs`.

use {
    libsecp256k1::{sign, Message, PublicKey, SecretKey},
    solana_account::Account,
    solana_pubkey::Pubkey,
};

/// 20-byte Ethereum-style guardian address length.
pub const GUARDIAN_PUBKEY_LENGTH: usize = 20;
/// On-wire signature record: 1-byte index + 64-byte r||s + 1-byte recovery id.
pub const GUARDIAN_SIGNATURE_LENGTH: usize = 66;
/// PDA seed for the Core Bridge `GuardianSet` account.
pub const GUARDIAN_SET_SEED: &[u8] = b"GuardianSet";

/// Anchor discriminator for the Shim's `GuardianSignatures` account:
/// `sha256("account:GuardianSignatures")[..8]`. Pinned to avoid a `sha2`
/// dev-dep; reproduce with `echo -n "account:GuardianSignatures" | shasum -a 256`.
pub const GUARDIAN_SIGNATURES_DISCRIMINATOR: [u8; 8] =
    [0xcb, 0xb8, 0x82, 0x9d, 0x71, 0x0e, 0xb8, 0x53];

/// Minimum size of a Shim `GuardianSignatures` account: discriminator (8) +
/// refund recipient (32) + guardian set index BE (4) + signature count LE (4).
/// Total = `GUARDIAN_SIGNATURES_MIN_SIZE + n * GUARDIAN_SIGNATURE_LENGTH`.
pub const GUARDIAN_SIGNATURES_MIN_SIZE: usize = 48;

/// Deterministic guardian: secret key + derived 20-byte Ethereum address.
#[derive(Clone)]
pub struct Guardian {
    pub secret: SecretKey,
    pub eth_address: [u8; GUARDIAN_PUBKEY_LENGTH],
}

/// Build `count` deterministic guardians from a byte seed; identical inputs
/// yield identical keys.
pub fn make_guardians(count: usize, seed: u8) -> Vec<Guardian> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut sk_bytes = [0u8; 32];
        sk_bytes[0] = seed;
        sk_bytes[1] = i as u8;
        // Keep the scalar small and non-zero so it stays inside the curve order
        // (`SecretKey::parse` rejects 0 and ≥ order).
        sk_bytes[31] = (i as u8).wrapping_add(1);

        let secret =
            SecretKey::parse(&sk_bytes).expect("deterministic seed inside secp256k1 group order");
        let public = PublicKey::from_secret_key(&secret);
        // `serialize()` emits 0x04 prefix + 64 raw (X||Y); strip the prefix.
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

/// Sign a 32-byte digest, returning the 65-byte `r||s||recovery_id` record.
pub fn sign_digest(guardian: &Guardian, digest: &[u8; 32]) -> [u8; 65] {
    let msg = Message::parse(digest);
    let (sig, rec) = sign(&msg, &guardian.secret);
    let sig_bytes = sig.serialize();
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig_bytes);
    out[64] = rec.serialize();
    out
}

/// Derive the Core Bridge `GuardianSet` PDA for a set index.
pub fn derive_guardian_set_pda(set_index: u32, core_bridge_program_id: &Pubkey) -> (Pubkey, u8) {
    let index_be = set_index.to_be_bytes();
    Pubkey::find_program_address(&[GUARDIAN_SET_SEED, &index_be], core_bridge_program_id)
}

/// Build a Core-Bridge-style `GuardianSet` account.
///
/// ```text
/// [gsi: u32 LE][keys_len: u32 LE][keys: 20*N][creation_time: u32 LE][expiration_time: u32 LE]
/// ```
///
/// `expiration_time = 0` marks the set never-expiring.
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
/// ```text
/// [disc: 8 = GUARDIAN_SIGNATURES_DISCRIMINATOR]
/// [refund_recipient: 32]
/// [gsi: u32 BE]    // big-endian — matches Core Bridge derivation
/// [sigs_len: u32 LE]
/// [signatures: 66 * N]
/// ```
///
/// `signatures` entries are `(guardian_index, sig65)`; callers must supply them
/// in strictly increasing index order. Owner is the Shim so its `VerifyHash`
/// owner check passes.
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
    // Big-endian: the Shim uses this slice as a Core Bridge PDA seed.
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
