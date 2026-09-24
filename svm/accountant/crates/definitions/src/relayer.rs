//! Wormhole Standard Relayer `DeliveryInstruction`: the envelope a relayed NTT message arrives
//! in. The accountant keeps the original sender and the wrapped message.

use core::mem::size_of;

use bytemuck::{Pod, Zeroable};

use crate::error::GlobalAccountantError;
use crate::ntt::MAX_NTT_PAYLOAD_LEN;
use crate::wire;

/// `DeliveryInstruction::PAYLOAD_ID`.
pub const DELIVERY_INSTRUCTION_PAYLOAD_ID: u8 = 1;

/// `DeliveryInstruction` up to the wrapped payload.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct DeliveryHead {
    pub payload_id: u8,
    pub target_chain: [u8; 2],
    pub target_address: [u8; 32],
    pub payload_len: [u8; 4],
}

/// Between the wrapped payload and the execution info.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct DeliveryMiddle {
    pub requested_reciever_value: [u8; 32],
    pub extra_reciever_value: [u8; 32],
    pub exec_info_len: [u8; 4],
}

/// After the execution info, up to the message keys.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct DeliveryTail {
    pub refund_chain: [u8; 2],
    pub refund_address: [u8; 32],
    pub refund_delivery_provider: [u8; 32],
    pub source_delivery_provider: [u8; 32],
    pub sender_address: [u8; 32],
    pub num_messages: u8,
}

/// `MessageKey::key_type` for a VAA identifier; other types carry a `u32` length.
const KEY_TYPE_VAA: u8 = 1;

/// `MessageKey` body for [`KEY_TYPE_VAA`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct VaaKey {
    pub chain: [u8; 2],
    pub emitter: [u8; 32],
    pub sequence: [u8; 8],
}

/// Fixed fields only: empty payload, exec info and message keys.
const MIN_DELIVERY_INSTRUCTION_LEN: usize =
    size_of::<DeliveryHead>() + size_of::<DeliveryMiddle>() + size_of::<DeliveryTail>();

const _: () = {
    assert!(size_of::<DeliveryHead>() == 39);
    assert!(size_of::<DeliveryMiddle>() == 68);
    assert!(size_of::<DeliveryTail>() == 131);
    assert!(size_of::<VaaKey>() == 42);
};

/// The original sender (hub/peer key) and the wrapped message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeliveryUnwrap<'a> {
    pub sender: [u8; 32],
    pub inner_payload: &'a [u8],
}

/// Parse a `DeliveryInstruction`; strict end-of-input, as CosmWasm.
///
/// ```text
/// DeliveryHead | payload | DeliveryMiddle | exec_info | DeliveryTail | MessageKey[num_messages]
/// MessageKey: key_type(1) | (key_type==1: key(42)) | (else: key_len(u32) | key)
/// ```
pub fn parse_delivery_instruction(
    data: &[u8],
) -> Result<DeliveryUnwrap<'_>, GlobalAccountantError> {
    if data.len() > MAX_NTT_PAYLOAD_LEN {
        return Err(GlobalAccountantError::NttPayloadTooLarge);
    }
    if data.len() < MIN_DELIVERY_INSTRUCTION_LEN {
        return Err(GlobalAccountantError::MalformedDeliveryInstruction);
    }
    parse(data).ok_or(GlobalAccountantError::MalformedDeliveryInstruction)
}

fn parse(data: &[u8]) -> Option<DeliveryUnwrap<'_>> {
    let (head, rest) = data.split_at_checked(size_of::<DeliveryHead>())?;
    let head: &DeliveryHead = bytemuck::try_from_bytes(head).ok()?;
    if head.payload_id != DELIVERY_INSTRUCTION_PAYLOAD_ID {
        return None;
    }
    let (inner_payload, rest) =
        rest.split_at_checked(u32::from_be_bytes(head.payload_len) as usize)?;

    let (middle, rest) = rest.split_at_checked(size_of::<DeliveryMiddle>())?;
    let middle: &DeliveryMiddle = bytemuck::try_from_bytes(middle).ok()?;
    let (_exec_info, rest) =
        rest.split_at_checked(u32::from_be_bytes(middle.exec_info_len) as usize)?;

    let (tail, mut rest) = rest.split_at_checked(size_of::<DeliveryTail>())?;
    let tail: &DeliveryTail = bytemuck::try_from_bytes(tail).ok()?;
    for _ in 0..tail.num_messages {
        let (key_type, after_type) = rest.split_first()?;
        let (_key, after_key) = if *key_type == KEY_TYPE_VAA {
            after_type.split_at_checked(size_of::<VaaKey>())?
        } else {
            wire::split_u32_be_prefixed(after_type)?
        };
        rest = after_key;
    }
    rest.is_empty().then_some(DeliveryUnwrap {
        sender: tail.sender_address,
        inner_payload,
    })
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use super::*;
    use GlobalAccountantError as E;

    pub(crate) fn build_delivery(sender: [u8; 32], inner: &[u8], keys: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.push(DELIVERY_INSTRUCTION_PAYLOAD_ID);
        v.extend_from_slice(&7u16.to_be_bytes()); // target_chain
        v.extend_from_slice(&[0x01; 32]); // target_address
        v.extend_from_slice(&(inner.len() as u32).to_be_bytes());
        v.extend_from_slice(inner);
        v.extend_from_slice(&[0x02; 32]); // requested_reciever_value
        v.extend_from_slice(&[0x03; 32]); // extra_reciever_value
        v.extend_from_slice(&0u32.to_be_bytes()); // exec_info_len
        v.extend_from_slice(&9u16.to_be_bytes()); // refund_chain
        v.extend_from_slice(&[0x04; 32]); // refund_address
        v.extend_from_slice(&[0x05; 32]); // refund_delivery_provider
        v.extend_from_slice(&[0x06; 32]); // source_delivery_provider
        v.extend_from_slice(&sender);
        v.push(keys.len() as u8);
        for key_type in keys {
            v.push(*key_type);
            if *key_type == 1 {
                v.extend_from_slice(&[0u8; 42]);
            } else {
                v.extend_from_slice(&3u32.to_be_bytes());
                v.extend_from_slice(&[0xAB; 3]);
            }
        }
        v
    }

    #[test]
    fn parse_delivery_table() {
        let inner = [0xAB, 0xCD, 0xEF];
        let unwrap = DeliveryUnwrap {
            sender: [0x42; 32],
            inner_payload: &inner,
        };
        let mut bad_id = build_delivery([0x42; 32], &inner, &[]);
        bad_id[0] = 2;
        let mut trailing = build_delivery([0x42; 32], &inner, &[]);
        trailing.push(0x99);
        let oversized = build_delivery([0x42; 32], &[0u8; 1900], &[]);

        let minimal = build_delivery([0x42; 32], &[], &[]);
        assert_eq!(minimal.len(), MIN_DELIVERY_INSTRUCTION_LEN);

        let cases: [(&str, Vec<u8>, Result<DeliveryUnwrap<'_>, E>); 7] = [
            (
                "minimal length is the fixed fields",
                minimal,
                Ok(DeliveryUnwrap {
                    sender: [0x42; 32],
                    inner_payload: &[],
                }),
            ),
            (
                "no message keys",
                build_delivery([0x42; 32], &inner, &[]),
                Ok(unwrap),
            ),
            (
                "two vaa keys",
                build_delivery([0x42; 32], &inner, &[1, 1]),
                Ok(unwrap),
            ),
            (
                "length-prefixed key",
                build_delivery([0x42; 32], &inner, &[2]),
                Ok(unwrap),
            ),
            (
                "bad payload id",
                bad_id,
                Err(E::MalformedDeliveryInstruction),
            ),
            (
                "trailing byte",
                trailing,
                Err(E::MalformedDeliveryInstruction),
            ),
            ("over cap", oversized, Err(E::NttPayloadTooLarge)),
        ];
        for (name, msg, expected) in cases {
            assert_eq!(parse_delivery_instruction(&msg), expected, "{name}");
        }
    }
}
