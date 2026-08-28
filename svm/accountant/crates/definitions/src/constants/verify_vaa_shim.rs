//! Verify VAA Shim program ID and `verify_hash` selector / wire size.

use bytemuck::{Pod, Zeroable};
use const_crypto::bs58;

use crate::primitives::Pubkey;

/// Verify VAA Shim program ID, from `VERIFY_VAA_SHIM_PROGRAM_ID` at compile time (same
/// variable `wormhole-svm-definitions` `from-env` reads). Set per target network in `justfile`.
pub const VERIFY_VAA_SHIM_PROGRAM_ID: Pubkey =
    bs58::decode_pubkey(env!("VERIFY_VAA_SHIM_PROGRAM_ID"));

/// `verify_hash` discriminator: first 8 bytes of `sha256("global:verify_hash")`.
pub const VERIFY_HASH_SELECTOR: [u8; 8] = [22, 152, 160, 69, 241, 148, 14, 124];

/// `verify_hash` instruction data: `selector ‖ guardian_set_bump ‖ digest`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct VerifyHashData {
    pub selector: [u8; 8],
    pub guardian_set_bump: u8,
    pub digest: [u8; 32],
}

impl VerifyHashData {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub const fn new(guardian_set_bump: u8, digest: [u8; 32]) -> Self {
        Self {
            selector: VERIFY_HASH_SELECTOR,
            guardian_set_bump,
            digest,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }
}

const _: () = {
    use core::mem::offset_of;
    assert!(VerifyHashData::LEN == 41);
    assert!(offset_of!(VerifyHashData, guardian_set_bump) == 8);
    assert!(offset_of!(VerifyHashData, digest) == 9);
};

#[cfg(test)]
mod tests {
    use super::*;
    use solana_program::pubkey::Pubkey;
    use wormhole_svm_definitions::solana::{devnet, localnet, mainnet};
    use wormhole_svm_shim::verify_vaa::{VerifyHash, VerifyHashAccounts};

    #[test]
    fn matches_wormhole_svm_definitions_and_shim() {
        let known = [
            mainnet::VERIFY_VAA_SHIM_PROGRAM_ID_ARRAY,
            devnet::VERIFY_VAA_SHIM_PROGRAM_ID_ARRAY,
            localnet::VERIFY_VAA_SHIM_PROGRAM_ID_ARRAY,
        ];
        assert!(known.contains(&VERIFY_VAA_SHIM_PROGRAM_ID));
        assert_eq!(
            VERIFY_HASH_SELECTOR,
            wormhole_svm_definitions::make_anchor_discriminator(b"global:verify_hash")
        );

        let program_id = Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID);
        let guardian_set = Pubkey::new_unique();
        let guardian_signatures = Pubkey::new_unique();
        let digest = [0xD1u8; 32];
        let theirs = VerifyHash {
            program_id: &program_id,
            accounts: VerifyHashAccounts {
                guardian_set: &guardian_set,
                guardian_signatures: &guardian_signatures,
            },
            data: wormhole_svm_shim::verify_vaa::VerifyHashData::new(
                7,
                solana_program::keccak::Hash(digest),
            ),
        }
        .instruction();
        assert_eq!(theirs.data, VerifyHashData::new(7, digest).as_bytes());
        assert_eq!(theirs.accounts.len(), 2);
        assert!(theirs
            .accounts
            .iter()
            .all(|m| !m.is_writable && !m.is_signer));
    }
}
