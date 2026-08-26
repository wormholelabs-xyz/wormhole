//! Synthetic guardian fixtures: deterministic keys, Core Bridge `GuardianSet`, and Shim
//! `GuardianSignatures` builders. Layouts follow
//! `svm/wormhole-core-shims/crates/definitions/src/zero_copy/{guardian_set,guardian_signatures}.rs`.

use {
    libsecp256k1::{sign, Message, PublicKey, SecretKey},
    solana_account::Account,
    solana_pubkey::Pubkey,
};

pub const GUARDIAN_PUBKEY_LENGTH: usize = 20;
/// Signature record: index (1) + r||s (64) + recovery id (1).
pub const GUARDIAN_SIGNATURE_LENGTH: usize = 66;
pub const GUARDIAN_SET_SEED: &[u8] = b"GuardianSet";

/// Shim `GuardianSignatures` discriminator: `sha256("account:GuardianSignatures")[..8]`.
/// Reproduce: `echo -n "account:GuardianSignatures" | shasum -a 256`.
pub const GUARDIAN_SIGNATURES_DISCRIMINATOR: [u8; 8] =
    [0xcb, 0xb8, 0x82, 0x9d, 0x71, 0x0e, 0xb8, 0x53];

/// `GuardianSignatures` header: discriminator (8) + refund recipient (32) + gsi BE (4) +
/// signature count LE (4).
pub const GUARDIAN_SIGNATURES_MIN_SIZE: usize = 48;

/// Secret key and 20-byte Ethereum address.
#[derive(Clone)]
pub struct Guardian {
    pub secret: SecretKey,
    pub eth_address: [u8; GUARDIAN_PUBKEY_LENGTH],
}

/// `count` deterministic guardians from a seed.
pub fn make_guardians(count: usize, seed: u8) -> Vec<Guardian> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut sk_bytes = [0u8; 32];
        sk_bytes[0] = seed;
        sk_bytes[1] = i as u8;
        // Small non-zero scalar stays inside the curve order.
        sk_bytes[31] = (i as u8).wrapping_add(1);

        let secret =
            SecretKey::parse(&sk_bytes).expect("deterministic seed inside secp256k1 group order");
        let public = PublicKey::from_secret_key(&secret);
        // Strip the 0x04 prefix.
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

/// 65-byte `r||s||recovery_id`.
pub fn sign_digest(guardian: &Guardian, digest: &[u8; 32]) -> [u8; 65] {
    let msg = Message::parse(digest);
    let (sig, rec) = sign(&msg, &guardian.secret);
    let sig_bytes = sig.serialize();
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig_bytes);
    out[64] = rec.serialize();
    out
}

pub fn derive_guardian_set_pda(set_index: u32, core_bridge_program_id: &Pubkey) -> (Pubkey, u8) {
    let index_be = set_index.to_be_bytes();
    Pubkey::find_program_address(&[GUARDIAN_SET_SEED, &index_be], core_bridge_program_id)
}

/// Core Bridge `GuardianSet` account:
///
/// ```text
/// [gsi: u32 LE][keys_len: u32 LE][keys: 20*N][creation_time: u32 LE][expiration_time: u32 LE]
/// ```
///
/// `expiration_time = 0`: never expires.
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

/// Shim `GuardianSignatures` account, owned by the Shim:
///
/// ```text
/// [disc: 8 = GUARDIAN_SIGNATURES_DISCRIMINATOR]
/// [refund_recipient: 32]
/// [gsi: u32 BE]
/// [sigs_len: u32 LE]
/// [signatures: 66 * N]
/// ```
///
/// `signatures` are `(guardian_index, sig65)` in increasing index order.
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
    // Big-endian: the Shim uses this slice as a PDA seed.
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
