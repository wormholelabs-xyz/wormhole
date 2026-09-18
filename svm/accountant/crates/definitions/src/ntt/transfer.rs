//! `TransceiverMessage<WormholeTransceiver, NativeTokenTransfer>`: an NTT transfer.

use core::mem::size_of;

use bytemuck::{Pod, Zeroable};

use crate::error::GlobalAccountantError;
use crate::primitives::Uint256;
use crate::wire;

use super::amount::normalize_trimmed_amount;
use super::MAX_NTT_PAYLOAD_LEN;

/// `WH_TRANSCEIVER_PAYLOAD_PREFIX`: leads a `TransceiverMessage`. NTT PR #46 introduced it as
/// `0x99 'E' 0xFF 0x10` while documenting the intent `0x99 'E''W''H'`; the shipped value is
/// what transceivers emit, so it stays.
pub const TRANSCEIVER_MESSAGE_PREFIX: [u8; 4] = [0x99, b'E', 0xff, 0x10];
/// `NTT_PREFIX`, the NTT `0x99 ‖ tag` convention: leads a `NativeTokenTransfer`.
pub const NATIVE_TOKEN_TRANSFER_PREFIX: [u8; 4] = [0x99, b'N', b'T', b'T'];

/// `TransceiverMessage` up to the manager message.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct TransceiverHead {
    pub prefix: [u8; 4],
    pub source_ntt_manager: [u8; 32],
    pub recipient_ntt_manager: [u8; 32],
    pub ntt_manager_payload_len: [u8; 2],
}

/// `NttManagerMessage` up to its payload.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct ManagerHead {
    pub id: [u8; 32],
    pub sender: [u8; 32],
    pub payload_len: [u8; 2],
}

/// `NativeTokenTransfer` fixed fields; an optional additional payload follows.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct NativeTokenTransfer {
    pub prefix: [u8; 4],
    pub decimals: u8,
    pub amount: [u8; 8],
    pub source_token: [u8; 32],
    pub to: [u8; 32],
    pub to_chain: [u8; 2],
}

/// Fixed fields only: empty additional and transceiver payloads.
const MIN_TRANSFER_LEN: usize = size_of::<TransceiverHead>()
    + size_of::<ManagerHead>()
    + size_of::<NativeTokenTransfer>()
    + size_of::<u16>(); // transceiver_payload_len

const _: () = {
    assert!(size_of::<TransceiverHead>() == 70);
    assert!(size_of::<ManagerHead>() == 66);
    assert!(size_of::<NativeTokenTransfer>() == 79);
};

/// Fields the accountant routes on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NttTransfer {
    /// Amount normalized to [`super::TRIMMED_DECIMALS`].
    pub amount: Uint256,
    /// `to_chain` of the `NativeTokenTransfer`.
    pub recipient_chain: u16,
}

/// Parse a transfer.
///
/// ```text
/// TransceiverHead | ManagerHead | NativeTokenTransfer | [additional_len(u16) | additional]
///   | transceiver_payload_len(u16) | transceiver_payload
/// ```
///
/// SECURITY: each length-prefixed section is parsed as its own slice and must be consumed
/// exactly, and the input must end after `transceiver_payload`, as the `ntt-messages` reader
/// requires. Over [`MAX_NTT_PAYLOAD_LEN`]: `NttPayloadTooLarge`.
pub fn parse_ntt_transfer(payload: &[u8]) -> Result<NttTransfer, GlobalAccountantError> {
    if payload.len() > MAX_NTT_PAYLOAD_LEN {
        return Err(GlobalAccountantError::NttPayloadTooLarge);
    }
    if payload.len() < MIN_TRANSFER_LEN {
        return Err(GlobalAccountantError::MalformedNttMessage);
    }
    parse(payload).ok_or(GlobalAccountantError::MalformedNttMessage)
}

fn parse(payload: &[u8]) -> Option<NttTransfer> {
    let (head, rest) = payload.split_at_checked(size_of::<TransceiverHead>())?;
    let head: &TransceiverHead = bytemuck::try_from_bytes(head).ok()?;
    if head.prefix != TRANSCEIVER_MESSAGE_PREFIX {
        return None;
    }
    let (manager_bytes, rest) =
        rest.split_at_checked(u16::from_be_bytes(head.ntt_manager_payload_len) as usize)?;
    let (_transceiver_payload, rest) = wire::split_u16_be_prefixed(rest)?;
    if !rest.is_empty() {
        return None;
    }

    let (manager, inner_bytes) = manager_bytes.split_at_checked(size_of::<ManagerHead>())?;
    let manager: &ManagerHead = bytemuck::try_from_bytes(manager).ok()?;
    if inner_bytes.len() != u16::from_be_bytes(manager.payload_len) as usize {
        return None;
    }

    let (transfer, rest) = inner_bytes.split_at_checked(size_of::<NativeTokenTransfer>())?;
    let transfer: &NativeTokenTransfer = bytemuck::try_from_bytes(transfer).ok()?;
    if transfer.prefix != NATIVE_TOKEN_TRANSFER_PREFIX {
        return None;
    }
    // Additional payload is present iff bytes follow the fixed fields.
    if !rest.is_empty() {
        let (_additional_payload, after_additional) = wire::split_u16_be_prefixed(rest)?;
        if !after_additional.is_empty() {
            return None;
        }
    }

    Some(NttTransfer {
        amount: normalize_trimmed_amount(transfer.decimals, u64::from_be_bytes(transfer.amount))?,
        recipient_chain: u16::from_be_bytes(transfer.to_chain),
    })
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use super::*;
    use GlobalAccountantError as E;

    /// `NativeTokenTransfer` with an optional additional payload.
    pub(crate) fn native_transfer(
        decimals: u8,
        raw_amount: u64,
        to_chain: u16,
        additional: Option<&[u8]>,
    ) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&NATIVE_TOKEN_TRANSFER_PREFIX);
        v.push(decimals);
        v.extend_from_slice(&raw_amount.to_be_bytes());
        v.extend_from_slice(&[0xEE; 32]); // source_token
        v.extend_from_slice(&[0xFF; 32]); // to
        v.extend_from_slice(&to_chain.to_be_bytes());
        if let Some(extra) = additional {
            v.extend_from_slice(&(extra.len() as u16).to_be_bytes());
            v.extend_from_slice(extra);
        }
        v
    }

    /// `TransceiverMessage` around `inner`, length prefixes consistent.
    pub(crate) fn transceiver_message(inner: &[u8], transceiver_payload: &[u8]) -> Vec<u8> {
        let mut manager = Vec::new();
        manager.extend_from_slice(&[0xCC; 32]); // id
        manager.extend_from_slice(&[0xDD; 32]); // sender
        manager.extend_from_slice(&(inner.len() as u16).to_be_bytes());
        manager.extend_from_slice(inner);

        let mut v = Vec::new();
        v.extend_from_slice(&TRANSCEIVER_MESSAGE_PREFIX);
        v.extend_from_slice(&[0xAA; 32]); // source_ntt_manager
        v.extend_from_slice(&[0xBB; 32]); // recipient_ntt_manager
        v.extend_from_slice(&(manager.len() as u16).to_be_bytes());
        v.extend_from_slice(&manager);
        v.extend_from_slice(&(transceiver_payload.len() as u16).to_be_bytes());
        v.extend_from_slice(transceiver_payload);
        v
    }

    fn build_msg(decimals: u8, raw_amount: u64, to_chain: u16) -> Vec<u8> {
        transceiver_message(&native_transfer(decimals, raw_amount, to_chain, None), &[])
    }

    /// Offsets inside `build_msg` output.
    const MANAGER_LEN_OFFSET: usize = 4 + 32 + 32;
    const INNER_LEN_OFFSET: usize = MANAGER_LEN_OFFSET + 2 + 32 + 32;
    const NTT_PREFIX_OFFSET: usize = INNER_LEN_OFFSET + 2;

    #[test]
    fn parse_ntt_transfer_table() {
        let ok = NttTransfer {
            amount: Uint256::from_u128(12_345),
            recipient_chain: 10,
        };
        let mut bad_outer_prefix = build_msg(8, 12_345, 10);
        bad_outer_prefix[0] = 0;
        let mut bad_inner_prefix = build_msg(8, 12_345, 10);
        bad_inner_prefix[NTT_PREFIX_OFFSET] = 0;
        let mut truncated = build_msg(8, 12_345, 10);
        truncated.pop();
        let mut manager_len_short = build_msg(8, 12_345, 10);
        manager_len_short[MANAGER_LEN_OFFSET + 1] -= 1;
        let mut inner_len_long = build_msg(8, 12_345, 10);
        inner_len_long[INNER_LEN_OFFSET + 1] += 1;
        let mut trailing = build_msg(8, 12_345, 10);
        trailing.push(0x99);
        let with_transceiver_payload =
            transceiver_message(&native_transfer(8, 12_345, 10, None), &[0x01, 0x02, 0x03]);
        let with_additional =
            transceiver_message(&native_transfer(8, 12_345, 10, Some(&[0x42; 5])), &[]);
        let mut additional_len_off = with_additional.clone();
        additional_len_off[NTT_PREFIX_OFFSET + 79 + 1] += 1;
        let oversized =
            transceiver_message(&native_transfer(8, 12_345, 10, Some(&[0u8; 1900])), &[]);
        let overflowing_decimals = build_msg(86, 1, 10);

        let cases: [(&str, Vec<u8>, Result<NttTransfer, E>); 14] = [
            ("well formed", build_msg(8, 12_345, 10), Ok(ok)),
            (
                "minimal length is the fixed fields",
                {
                    let msg = build_msg(8, 12_345, 10);
                    assert_eq!(msg.len(), MIN_TRANSFER_LEN);
                    msg
                },
                Ok(ok),
            ),
            (
                "scaled through parse",
                build_msg(3, 1000, 2),
                Ok(NttTransfer {
                    amount: Uint256::from_u128(100_000_000),
                    recipient_chain: 2,
                }),
            ),
            (
                "transceiver payload present",
                with_transceiver_payload,
                Ok(ok),
            ),
            ("additional payload present", with_additional, Ok(ok)),
            (
                "bad transceiver prefix",
                bad_outer_prefix,
                Err(E::MalformedNttMessage),
            ),
            (
                "bad ntt prefix",
                bad_inner_prefix,
                Err(E::MalformedNttMessage),
            ),
            ("truncated", truncated, Err(E::MalformedNttMessage)),
            (
                "manager len mismatch",
                manager_len_short,
                Err(E::MalformedNttMessage),
            ),
            (
                "inner len mismatch",
                inner_len_long,
                Err(E::MalformedNttMessage),
            ),
            (
                "additional len mismatch",
                additional_len_off,
                Err(E::MalformedNttMessage),
            ),
            ("trailing byte", trailing, Err(E::MalformedNttMessage)),
            (
                "decimals past wormchain pow",
                overflowing_decimals,
                Err(E::MalformedNttMessage),
            ),
            ("over cap", oversized, Err(E::NttPayloadTooLarge)),
        ];
        for (name, msg, expected) in cases {
            assert_eq!(parse_ntt_transfer(&msg), expected, "{name}");
        }
    }
}
