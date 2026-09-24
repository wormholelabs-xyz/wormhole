use libsecp256k1::{sign, Message, PublicKey, SecretKey};
use solana_account::Account;
use solana_pubkey::Pubkey;
pub use wormhole_svm_definitions::{
    GUARDIAN_PUBKEY_LENGTH, GUARDIAN_SET_SEED, GUARDIAN_SIGNATURES_DISCRIMINATOR,
    GUARDIAN_SIGNATURE_LENGTH,
};

pub use wormhole_svm_definitions::zero_copy::GuardianSet;

pub const GUARDIAN_SIGNATURES_MIN_SIZE: usize = 48;

#[derive(Clone)]
pub struct Guardian {
    pub secret: SecretKey,
    pub eth_address: [u8; GUARDIAN_PUBKEY_LENGTH],
}

pub fn make_guardians(count: usize, seed: u8) -> Vec<Guardian> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut sk_bytes = [0u8; 32];
        sk_bytes[0] = seed;
        sk_bytes[1] = i as u8;
        sk_bytes[31] = (i as u8).wrapping_add(1);
        let secret = SecretKey::parse(&sk_bytes).expect("secp256k1 scalar");
        let public = PublicKey::from_secret_key(&secret);
        let raw = &public.serialize()[1..];
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

pub fn sign_digest(guardian: &Guardian, digest: &[u8; 32]) -> [u8; 65] {
    let (sig, rec) = sign(&Message::parse(digest), &guardian.secret);
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig.serialize());
    out[64] = rec.serialize();
    out
}

pub fn derive_guardian_set_pda(set_index: u32, core_bridge_program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[GUARDIAN_SET_SEED, &set_index.to_be_bytes()],
        core_bridge_program_id,
    )
}

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
    data.extend_from_slice(&set_index.to_be_bytes());
    data.extend_from_slice(&(signatures.len() as u32).to_le_bytes());
    data.extend_from_slice(&signature_block(signatures));
    Account {
        lamports: 5_000_000_000,
        data,
        owner: *shim_program_id,
        executable: false,
        rent_epoch: 0,
    }
}

/// Concatenate `(guardian_index, signature)` pairs into the shim's wire form.
pub fn signature_block(signatures: &[(u8, [u8; 65])]) -> Vec<u8> {
    let mut block = Vec::with_capacity(signatures.len() * GUARDIAN_SIGNATURE_LENGTH);
    for (idx, sig) in signatures {
        block.push(*idx);
        block.extend_from_slice(&sig[..]);
    }
    block
}

/// Copy of `existing` with `expiration_time` replaced. Reads the fields through the
/// zero-copy `GuardianSet` view and rebuilds via [`guardian_set_account`].
pub fn guardian_set_with_expiration(existing: &Account, expiration_time: u32) -> Account {
    let set = GuardianSet::new(&existing.data).expect("guardian set layout");
    let keys: Vec<[u8; GUARDIAN_PUBKEY_LENGTH]> = (0..set.keys_len() as usize)
        .map(|i| set.key(i).expect("key within keys_len"))
        .collect();
    let mut account = guardian_set_account(
        set.guardian_set_index(),
        &keys,
        set.creation_time(),
        expiration_time,
        &existing.owner,
    );
    account.lamports = existing.lamports;
    account
}
