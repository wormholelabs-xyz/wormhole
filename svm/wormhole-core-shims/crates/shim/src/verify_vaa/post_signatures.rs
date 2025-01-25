use solana_program::pubkey::Pubkey;

pub const SIGNATURE_LENGTH: usize = 66;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostSignaturesAccounts<'ix> {
    pub payer: &'ix Pubkey,

    /// Program-derived accounts required for the post signatures instruction.
    ///
    /// NOTE: If `None` is used for any of these accounts,
    /// [PostMessage::instruction] will find these addresses. For on-chain
    /// applications, it is recommended to provide these accounts to save on
    /// CU costs (1,500 CU per bump iteration per account).
    pub derived: PostSignaturesDerivedAccounts<'ix>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]

pub struct PostSignaturesDerivedAccounts<'ix> {
    pub guardian_signatures: Option<&'ix Pubkey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostSignaturesData<'ix> {
    pub guardian_set_index: u32,
    pub total_signatures: u8,
    pub guardian_signatures: &'ix [[u8; SIGNATURE_LENGTH]],
}

impl<'ix> PostSignaturesData<'ix> {
    pub const MINIMUM_SIZE: usize = {
        4 // guardian_set_index
        + 1 // total_signatures
        + 4 // guardian_signatures length
    };

    #[inline(always)]
    pub(super) fn deserialize(data: &'ix [u8]) -> Option<Self> {
        if data.len() < Self::MINIMUM_SIZE {
            return None;
        }

        let guardian_set_index = u32::from_le_bytes(data[..4].try_into().unwrap());
        let total_signatures = data[4];
        let guardian_signatures_len = u32::from_le_bytes(data[5..9].try_into().unwrap()) as usize;

        let encoded_signatures_len = guardian_signatures_len * SIGNATURE_LENGTH;
        if data.len() < Self::MINIMUM_SIZE + encoded_signatures_len {
            return None;
        }

        let guardian_signatures = &data[9..(9 + encoded_signatures_len)];

        // Safety: Guardian signatures are contiguous and its length is a
        // multiple of SIGNATURE_LENGTH.
        let guardian_signatures = unsafe {
            core::slice::from_raw_parts(
                guardian_signatures.as_ptr() as *const [u8; SIGNATURE_LENGTH],
                guardian_signatures_len,
            )
        };

        // NOTE: We do not care about trailing bytes.

        Some(Self {
            guardian_set_index,
            total_signatures,
            guardian_signatures,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostSignatures<'ix> {
    pub program_id: &'ix Pubkey,
    pub accounts: PostSignaturesAccounts<'ix>,
    pub data: PostSignaturesData<'ix>,
}
