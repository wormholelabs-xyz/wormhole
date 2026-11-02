//! Verify VAA Shim program ID plus the `verify_hash` and `post_signatures` wire layouts.

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

/// `post_signatures` discriminator: first 8 bytes of `sha256("global:post_signatures")`.
pub const POST_SIGNATURES_SELECTOR: [u8; 8] = [0x8a, 0x02, 0x35, 0xa6, 0x2d, 0x4d, 0x89, 0x33];

/// `post_signatures` instruction prefix. The signature block follows:
/// `guardian_signatures_len` entries of `guardian_index ‖ signature`, 66 bytes each.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct PostSignaturesIxData {
    pub selector: [u8; 8],
    /// Little-endian.
    pub guardian_set_index: [u8; 4],
    /// Sizes the guardian-signatures account; can exceed the signatures in this call.
    pub total_signatures: u8,
    /// Little-endian count of 66-byte entries that follow this prefix.
    pub guardian_signatures_len: [u8; 4],
}

impl PostSignaturesIxData {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub const fn new(
        guardian_set_index: u32,
        total_signatures: u8,
        guardian_signatures_len: u32,
    ) -> Self {
        Self {
            selector: POST_SIGNATURES_SELECTOR,
            guardian_set_index: guardian_set_index.to_le_bytes(),
            total_signatures,
            guardian_signatures_len: guardian_signatures_len.to_le_bytes(),
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }
}

const _: () = {
    use core::mem::offset_of;
    assert!(PostSignaturesIxData::LEN == 17);
    assert!(offset_of!(PostSignaturesIxData, guardian_set_index) == 8);
    assert!(offset_of!(PostSignaturesIxData, total_signatures) == 12);
    assert!(offset_of!(PostSignaturesIxData, guardian_signatures_len) == 13);
};

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

    #[test]
    fn post_signatures_matches_shim_builder() {
        use wormhole_svm_shim::verify_vaa::{
            PostSignatures, PostSignaturesAccounts, PostSignaturesData,
        };

        assert_eq!(
            POST_SIGNATURES_SELECTOR,
            wormhole_svm_definitions::make_anchor_discriminator(b"global:post_signatures")
        );

        let program_id = Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID);
        let payer = Pubkey::new_unique();
        let guardian_signatures = Pubkey::new_unique();
        let entries: [[u8; 66]; 2] = [[0xA1; 66], [0xB2; 66]];
        let theirs = PostSignatures {
            program_id: &program_id,
            accounts: PostSignaturesAccounts {
                payer: &payer,
                guardian_signatures: &guardian_signatures,
            },
            data: PostSignaturesData::new(6, 13, &entries),
        }
        .instruction();

        let mut ours = PostSignaturesIxData::new(6, 13, 2).as_bytes().to_vec();
        ours.extend_from_slice(&entries[0]);
        ours.extend_from_slice(&entries[1]);
        assert_eq!(theirs.data, ours);
        assert_eq!(theirs.accounts.len(), 3);
        assert!(theirs.accounts[0].is_signer && theirs.accounts[0].is_writable);
        assert!(theirs.accounts[1].is_signer && theirs.accounts[1].is_writable);
        assert!(!theirs.accounts[2].is_signer && !theirs.accounts[2].is_writable);
    }
}
