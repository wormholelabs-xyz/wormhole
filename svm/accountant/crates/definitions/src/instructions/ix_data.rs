//! Instruction-data layouts after the 1-byte discriminator. Fixed prefixes are
//! `repr(C)` views; a trailing `body_len` (u16 LE) frames the variable VAA body.
//! Instruction-data integers are little-endian; VAA body fields are big-endian.

use bytemuck::{Pod, Zeroable};

use crate::error::GlobalAccountantError;

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

/// `submit_observations` prefix (104 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct SubmitObservationsIxData {
    /// Little-endian.
    pub guardian_set_index: [u8; 4],
    pub guardian_index: u8,
    /// `r ‖ s ‖ recovery_id`.
    pub signature: [u8; 65],
    /// Source-chain transaction id; part of the signing digest only.
    pub tx_hash: [u8; 32],
    pub body_len: [u8; 2],
}

impl SubmitObservationsIxData {
    pub fn guardian_set_index(&self) -> u32 {
        u32::from_le_bytes(self.guardian_set_index)
    }
}

impl IxPrefix for SubmitObservationsIxData {
    fn body_len(&self) -> usize {
        u16::from_le_bytes(self.body_len) as usize
    }
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
    assert!(SubmitObservationsIxData::LEN == 104);
    assert!(offset_of!(SubmitObservationsIxData, guardian_index) == 4);
    assert!(offset_of!(SubmitObservationsIxData, signature) == 5);
    assert!(offset_of!(SubmitObservationsIxData, tx_hash) == 70);
    assert!(offset_of!(SubmitObservationsIxData, body_len) == 102);
    assert!(SubmitVaasIxData::LEN == 3);
    assert!(RegisterChainIxData::LEN == 3);
    assert!(offset_of!(RegisterChainIxData, body_len) == 1);
    assert!(ModifyBalanceIxData::LEN == 3);
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
    fn submit_observations_prefix_round_trips() {
        let mut prefix = SubmitObservationsIxData::zeroed();
        prefix.guardian_set_index = 4u32.to_le_bytes();
        prefix.guardian_index = 12;
        prefix.signature[64] = 1;
        prefix.tx_hash = [0xCC; 32];
        prefix.body_len = 52u16.to_le_bytes();
        let data = framed(&prefix, &[0u8; 52]);
        let (view, body) = split_body::<SubmitObservationsIxData>(&data).unwrap();
        assert_eq!(view.guardian_set_index(), 4);
        assert_eq!(view.guardian_index, 12);
        assert_eq!(view.signature[64], 1);
        assert_eq!(view.tx_hash, [0xCC; 32]);
        assert_eq!(body.len(), 52);
    }
}
