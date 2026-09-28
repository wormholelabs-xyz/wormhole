//! Instruction-data layouts after the 1-byte discriminator. Fixed prefixes are
//! `repr(C)` views; a trailing `body_len` (u16 LE) frames the variable VAA body.
//! Instruction-data integers are little-endian; VAA body fields are big-endian.

use bytemuck::{Pod, Zeroable};

use crate::error::GlobalAccountantError;
use crate::primitives::Uint256;

/// Fixed prefix followed by a `body_len`-framed body.
pub trait IxPrefix: Pod {
    const LEN: usize = core::mem::size_of::<Self>();

    fn body_len(&self) -> usize;
}

/// Split `data` into the prefix view and the body.
///
/// SECURITY: precondition `data.len() == P::LEN + prefix.body_len()`; anything else is
/// `InvalidInstructionData`. Cannot panic.
pub fn split_body<P: IxPrefix>(data: &[u8]) -> Result<(&P, &[u8]), GlobalAccountantError> {
    let (head, body) = data
        .split_at_checked(P::LEN)
        .ok_or(GlobalAccountantError::InvalidInstructionData)?;
    let prefix: &P = bytemuck::try_from_bytes(head)
        .map_err(|_| GlobalAccountantError::InvalidInstructionData)?;
    if body.len() != prefix.body_len() {
        return Err(GlobalAccountantError::InvalidInstructionData);
    }
    Ok((prefix, body))
}

/// `submit_observations` data: 278 bytes, fixed size. Carries only the fields this
/// program uses; every other real-body field folds into `digest`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct SubmitObservationsIxData {
    /// Little-endian.
    pub guardian_set_index: [u8; 4],
    pub guardian_index: u8,
    /// `r ‖ s ‖ recovery_id`.
    pub signature: [u8; 65],
    /// Byte length of `tx_id`: 32 or 64.
    pub tx_id_len: u8,
    /// Source-chain transaction id, zero-padded past `tx_id_len`; part of the signing
    /// digest only. Read through `tx_id()`.
    pub tx_id: [u8; SIGNATURE_TX_ID_LEN],
    /// Token Bridge action byte. 0x02 is a no-op; anything but 0x01/0x02/0x03 is
    /// `UnknownTokenBridgePayload`.
    pub action: u8,
    /// Big-endian.
    pub chain: [u8; 2],
    pub emitter: [u8; 32],
    /// Big-endian.
    pub sequence: [u8; 8],
    /// Big-endian. Set only when `action` is a transfer.
    pub token_chain: [u8; 2],
    /// Set only when `action` is a transfer.
    pub token_address: [u8; 32],
    /// Big-endian. Set only when `action` is a transfer.
    pub recipient_chain: [u8; 2],
    /// Set only when `action` is a transfer.
    pub amount: Uint256,
    /// `double_keccak256` of the real VAA body.
    pub digest: [u8; 32],
}

/// Hash-sized transaction id.
pub const HASH_TX_ID_LEN: usize = 32;
/// Solana-family id: the 64-byte ed25519 transaction signature.
pub const SIGNATURE_TX_ID_LEN: usize = 64;

/// Source-chain transaction id; one variant per valid length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxId<'a> {
    Hash(&'a [u8; HASH_TX_ID_LEN]),
    Signature(&'a [u8; SIGNATURE_TX_ID_LEN]),
}

impl TxId<'_> {
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            TxId::Hash(id) => id.as_slice(),
            TxId::Signature(id) => id.as_slice(),
        }
    }
}

impl SubmitObservationsIxData {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn guardian_set_index(&self) -> u32 {
        u32::from_le_bytes(self.guardian_set_index)
    }

    pub fn chain(&self) -> u16 {
        u16::from_be_bytes(self.chain)
    }

    pub fn sequence(&self) -> u64 {
        u64::from_be_bytes(self.sequence)
    }

    pub fn token_chain(&self) -> u16 {
        u16::from_be_bytes(self.token_chain)
    }

    pub fn recipient_chain(&self) -> u16 {
        u16::from_be_bytes(self.recipient_chain)
    }

    /// Exact-length view.
    pub fn from_bytes(data: &[u8]) -> Result<&Self, GlobalAccountantError> {
        bytemuck::try_from_bytes(data).map_err(|_| GlobalAccountantError::InvalidInstructionData)
    }

    /// The first `tx_id_len` bytes of `tx_id`.
    ///
    /// SECURITY: `tx_id_len` is exactly 32 or 64 and every byte past it is zero, so one
    /// id has one encoding. Anything else is `InvalidInstructionData`.
    pub fn tx_id(&self) -> Result<TxId<'_>, GlobalAccountantError> {
        let tx_id = match usize::from(self.tx_id_len) {
            HASH_TX_ID_LEN => {
                let (id, padding) = self
                    .tx_id
                    .split_first_chunk::<HASH_TX_ID_LEN>()
                    .ok_or(GlobalAccountantError::InvalidInstructionData)?;
                if padding.iter().any(|&b| b != 0) {
                    return Err(GlobalAccountantError::InvalidInstructionData);
                }
                TxId::Hash(id)
            }
            SIGNATURE_TX_ID_LEN => TxId::Signature(&self.tx_id),
            _ => return Err(GlobalAccountantError::InvalidInstructionData),
        };
        debug_assert!(tx_id.as_bytes().len() == usize::from(self.tx_id_len));
        Ok(tx_id)
    }

    /// `action ‖ chain ‖ emitter ‖ sequence ‖ token_chain ‖ token_address ‖
    /// recipient_chain ‖ amount ‖ digest`, 143 bytes. Hashed for the signing digest and
    /// the content digest.
    pub fn fields_and_digest(&self) -> [u8; 143] {
        bytemuck::cast(ObservationFieldsAndDigest {
            action: self.action,
            chain: self.chain,
            emitter: self.emitter,
            sequence: self.sequence,
            token_chain: self.token_chain,
            token_address: self.token_address,
            recipient_chain: self.recipient_chain,
            amount: self.amount,
            digest: self.digest,
        })
    }
}

/// The exact fields `fields_and_digest` hashes, as their own `Pod` layout. Naming every
/// field in a struct literal is exhaustiveness-checked by the compiler; `bytemuck::cast`
/// then reinterprets it as bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
struct ObservationFieldsAndDigest {
    action: u8,
    chain: [u8; 2],
    emitter: [u8; 32],
    sequence: [u8; 8],
    token_chain: [u8; 2],
    token_address: [u8; 32],
    recipient_chain: [u8; 2],
    amount: Uint256,
    digest: [u8; 32],
}

/// `submit_vaas` prefix (3 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct SubmitVaasIxData {
    pub guardian_set_bump: u8,
    pub body_len: [u8; 2],
}

impl IxPrefix for SubmitVaasIxData {
    fn body_len(&self) -> usize {
        u16::from_le_bytes(self.body_len) as usize
    }
}

/// `register_chain` prefix (3 bytes). PDA bumps derive on-chain.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct RegisterChainIxData {
    pub guardian_set_bump: u8,
    pub body_len: [u8; 2],
}

impl IxPrefix for RegisterChainIxData {
    fn body_len(&self) -> usize {
        u16::from_le_bytes(self.body_len) as usize
    }
}

/// `modify_balance` prefix (3 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct ModifyBalanceIxData {
    pub guardian_set_bump: u8,
    pub body_len: [u8; 2],
}

impl IxPrefix for ModifyBalanceIxData {
    fn body_len(&self) -> usize {
        u16::from_le_bytes(self.body_len) as usize
    }
}

/// `upgrade_contract` prefix (3 bytes). PDA bumps derive on-chain.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct UpgradeContractIxData {
    pub guardian_set_bump: u8,
    pub body_len: [u8; 2],
}

impl IxPrefix for UpgradeContractIxData {
    fn body_len(&self) -> usize {
        u16::from_le_bytes(self.body_len) as usize
    }
}

/// `close_pending` data (40 bytes, no body).
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct ClosePendingIxData {
    pub emitter: [u8; 32],
    /// Big-endian, as in the VAA body.
    pub sequence: [u8; 8],
}

impl ClosePendingIxData {
    pub const LEN: usize = core::mem::size_of::<Self>();

    /// Exact-length view.
    pub fn from_bytes(data: &[u8]) -> Result<&Self, GlobalAccountantError> {
        bytemuck::try_from_bytes(data).map_err(|_| GlobalAccountantError::InvalidInstructionData)
    }

    pub fn sequence(&self) -> u64 {
        u64::from_be_bytes(self.sequence)
    }
}

const _: () = {
    use core::mem::offset_of;
    assert!(SubmitObservationsIxData::LEN == 278);
    assert!(core::mem::size_of::<ObservationFieldsAndDigest>() == 143);
    assert!(offset_of!(SubmitObservationsIxData, guardian_index) == 4);
    assert!(offset_of!(SubmitObservationsIxData, signature) == 5);
    assert!(offset_of!(SubmitObservationsIxData, tx_id_len) == 70);
    assert!(offset_of!(SubmitObservationsIxData, tx_id) == 71);
    assert!(offset_of!(SubmitObservationsIxData, action) == 135);
    assert!(offset_of!(SubmitObservationsIxData, chain) == 136);
    assert!(offset_of!(SubmitObservationsIxData, emitter) == 138);
    assert!(offset_of!(SubmitObservationsIxData, sequence) == 170);
    assert!(offset_of!(SubmitObservationsIxData, token_chain) == 178);
    assert!(offset_of!(SubmitObservationsIxData, token_address) == 180);
    assert!(offset_of!(SubmitObservationsIxData, recipient_chain) == 212);
    assert!(offset_of!(SubmitObservationsIxData, amount) == 214);
    assert!(offset_of!(SubmitObservationsIxData, digest) == 246);
    assert!(SubmitVaasIxData::LEN == 3);
    assert!(RegisterChainIxData::LEN == 3);
    assert!(offset_of!(RegisterChainIxData, body_len) == 1);
    assert!(ModifyBalanceIxData::LEN == 3);
    assert!(UpgradeContractIxData::LEN == 3);
    assert!(offset_of!(UpgradeContractIxData, body_len) == 1);
    assert!(ClosePendingIxData::LEN == 40);
    assert!(offset_of!(ClosePendingIxData, sequence) == 32);
};

#[cfg(test)]
mod tests {
    use super::*;

    fn framed<P: IxPrefix>(prefix: &P, body: &[u8]) -> std::vec::Vec<u8> {
        let mut out = bytemuck::bytes_of(prefix).to_vec();
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn framing_table() {
        let body = [0xABu8; 60];
        let prefix = SubmitVaasIxData {
            guardian_set_bump: 7,
            body_len: 60u16.to_le_bytes(),
        };
        let data = framed(&prefix, &body);
        let (view, got) = split_body::<SubmitVaasIxData>(&data).unwrap();
        assert_eq!(view.guardian_set_bump, 7);
        assert_eq!(got, &body[..]);

        let split_cases: [(&str, std::vec::Vec<u8>); 4] = [
            ("one byte short", data[..data.len() - 1].to_vec()),
            ("one byte long", [data.as_slice(), &[0]].concat()),
            ("prefix only", data[..SubmitVaasIxData::LEN].to_vec()),
            ("empty", std::vec::Vec::new()),
        ];
        for (name, bytes) in split_cases {
            assert_eq!(
                split_body::<SubmitVaasIxData>(&bytes).err(),
                Some(GlobalAccountantError::InvalidInstructionData),
                "{name}"
            );
        }

        let upgrade = UpgradeContractIxData {
            guardian_set_bump: 3,
            body_len: 5u16.to_le_bytes(),
        };
        let upgrade_data = framed(&upgrade, &[9; 5]);
        let (upgrade_view, upgrade_body) =
            split_body::<UpgradeContractIxData>(&upgrade_data).unwrap();
        assert_eq!(upgrade_view.guardian_set_bump, 3);
        assert_eq!(upgrade_body, &[9; 5]);

        let mut close = [0u8; ClosePendingIxData::LEN];
        close[32..].copy_from_slice(&9u64.to_be_bytes());
        let close_cases: [(&str, std::vec::Vec<u8>, bool); 3] = [
            ("close_pending exact", close.to_vec(), true),
            ("close_pending short", close[..39].to_vec(), false),
            (
                "close_pending long",
                [close.as_slice(), &[0]].concat(),
                false,
            ),
        ];
        for (name, bytes, ok) in close_cases {
            assert_eq!(ClosePendingIxData::from_bytes(&bytes).is_ok(), ok, "{name}");
        }
        assert_eq!(
            ClosePendingIxData::from_bytes(&close).unwrap().sequence(),
            9
        );
    }

    #[test]
    fn submit_observations_round_trips() {
        let mut ix = SubmitObservationsIxData::zeroed();
        ix.guardian_set_index = 4u32.to_le_bytes();
        ix.guardian_index = 12;
        ix.signature[64] = 1;
        ix.tx_id_len = 32;
        ix.tx_id[..32].copy_from_slice(&[0xCC; 32]);
        ix.action = 0x01;
        ix.chain = 2u16.to_be_bytes();
        ix.emitter = [0xEE; 32];
        ix.sequence = 7u64.to_be_bytes();
        ix.token_chain = 3u16.to_be_bytes();
        ix.token_address = [0x33; 32];
        ix.recipient_chain = 5u16.to_be_bytes();
        ix.amount = Uint256::from_u128(1_000);
        ix.digest = [0x55; 32];

        let bytes = bytemuck::bytes_of(&ix).to_vec();
        let view = SubmitObservationsIxData::from_bytes(&bytes).unwrap();
        assert_eq!(view.guardian_set_index(), 4);
        assert_eq!(view.guardian_index, 12);
        assert_eq!(view.signature[64], 1);
        assert_eq!(view.tx_id(), Ok(TxId::Hash(&[0xCC; 32])));
        assert_eq!(view.action, 0x01);
        assert_eq!(view.chain(), 2);
        assert_eq!(view.emitter, [0xEE; 32]);
        assert_eq!(view.sequence(), 7);
        assert_eq!(view.token_chain(), 3);
        assert_eq!(view.token_address, [0x33; 32]);
        assert_eq!(view.recipient_chain(), 5);
        assert_eq!(view.amount, Uint256::from_u128(1_000));
        assert_eq!(view.digest, [0x55; 32]);

        let short = &bytes[..bytes.len() - 1];
        assert!(SubmitObservationsIxData::from_bytes(short).is_err());
        let long = [bytes.as_slice(), &[0]].concat();
        assert!(SubmitObservationsIxData::from_bytes(&long).is_err());
    }

    #[test]
    fn tx_id_parses_only_canonical_lengths() {
        let hash = [0xCC; HASH_TX_ID_LEN];
        let signature = [0xDD; SIGNATURE_TX_ID_LEN];
        let mut hash_padded = [0u8; SIGNATURE_TX_ID_LEN];
        hash_padded[..HASH_TX_ID_LEN].copy_from_slice(&hash);
        let mut dirty_padding = hash_padded;
        dirty_padding[SIGNATURE_TX_ID_LEN - 1] = 1;

        let cases: [(&str, u8, [u8; SIGNATURE_TX_ID_LEN], Result<TxId, _>); 6] = [
            ("hash", 32, hash_padded, Ok(TxId::Hash(&hash))),
            ("signature", 64, signature, Ok(TxId::Signature(&signature))),
            ("hash, dirty padding", 32, dirty_padding, Err(())),
            ("zero length", 0, [0u8; SIGNATURE_TX_ID_LEN], Err(())),
            ("off by one", 33, hash_padded, Err(())),
            ("over max", 65, signature, Err(())),
        ];
        for (name, len, bytes, expected) in cases {
            let mut ix = SubmitObservationsIxData::zeroed();
            ix.tx_id_len = len;
            ix.tx_id = bytes;
            let expected = expected.map_err(|()| GlobalAccountantError::InvalidInstructionData);
            assert_eq!(ix.tx_id(), expected, "{name}");
        }
    }

    #[test]
    fn fields_and_digest_packs_fields_in_order() {
        let mut ix = SubmitObservationsIxData::zeroed();
        ix.action = 0x03;
        ix.chain = 7u16.to_be_bytes();
        ix.emitter = [0x11; 32];
        ix.sequence = 9u64.to_be_bytes();
        ix.token_chain = 2u16.to_be_bytes();
        ix.token_address = [0x22; 32];
        ix.recipient_chain = 5u16.to_be_bytes();
        ix.amount = Uint256::from_u128(4_242);
        ix.digest = [0x99; 32];

        let packed = ix.fields_and_digest();
        assert_eq!(packed.len(), 143);
        assert_eq!(packed[0], 0x03);
        assert_eq!(&packed[1..3], 7u16.to_be_bytes());
        assert_eq!(&packed[3..35], &[0x11; 32]);
        assert_eq!(&packed[35..43], 9u64.to_be_bytes());
        assert_eq!(&packed[43..45], 2u16.to_be_bytes());
        assert_eq!(&packed[45..77], &[0x22; 32]);
        assert_eq!(&packed[77..79], 5u16.to_be_bytes());
        assert_eq!(&packed[79..111], Uint256::from_u128(4_242).0);
        assert_eq!(&packed[111..143], &[0x99; 32]);
    }
}
