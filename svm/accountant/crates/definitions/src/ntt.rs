//! NTT message wire formats and amount normalization — parity with the
//! CosmWasm NTT global accountant. All multi-byte integers are big-endian
//! (`wormhole-io` `Readable`). Pure `no_std` byte parsing; host-testable.
//!
//! See the frozen D0 spec in `accountant-migration.md`. Only the fields the
//! accountant needs are extracted; `source_token` / `to` / nested length
//! prefixes are consumed but not returned (the accounted token identity comes
//! from the transceiver-hub mapping, not the message's `source_token`).

use crate::error::GlobalAccountantError;
use crate::primitives::Uint256;

/// `WormholeTransceiver::PREFIX` — leads a `TransceiverMessage` (an NTT transfer).
pub const TRANSCEIVER_MESSAGE_PREFIX: [u8; 4] = [0x99, 0x45, 0xff, 0x10];
/// `NativeTokenTransfer::PREFIX` — leads the inner NTT manager payload.
pub const NATIVE_TOKEN_TRANSFER_PREFIX: [u8; 4] = [0x99, 0x4e, 0x54, 0x54];
/// `WormholeTransceiver::INFO_PREFIX` — hub-registration (transceiver info) message.
pub const TRANSCEIVER_INFO_PREFIX: [u8; 4] = [0x9c, 0x23, 0xbd, 0x3b];
/// `WormholeTransceiver::PEER_INFO_PREFIX` — peer-registration message.
pub const TRANSCEIVER_PEER_INFO_PREFIX: [u8; 4] = [0x18, 0xfc, 0x67, 0xc2];

/// Decimals every NTT transfer amount is normalized to before accounting
/// (`TRIMMED_DECIMALS` in the CosmWasm contract).
pub const TRIMMED_DECIMALS: u8 = 8;

/// `10^exp` as a [`Uint256`], or `None` on overflow (absurd `exp`).
fn pow10(exp: u8) -> Option<Uint256> {
    let ten = Uint256::from_u128(10);
    let mut acc = Uint256::from_u128(1);
    for _ in 0..exp {
        acc = acc.checked_mul(ten)?;
    }
    Some(acc)
}

/// Normalize a `TrimmedAmount` `(decimals, amount)` to a [`Uint256`] at
/// [`TRIMMED_DECIMALS`] (8), matching CosmWasm `normalize_transfer_amount`:
/// truncating integer division when scaling down, multiplication when scaling
/// up. Returns `None` only on multiply overflow (caller maps to an error).
pub fn normalize_trimmed_amount(decimals: u8, amount: u64) -> Option<Uint256> {
    let amt = Uint256::from_u128(amount as u128);
    match decimals.cmp(&TRIMMED_DECIMALS) {
        core::cmp::Ordering::Equal => Some(amt),
        // from > 8: divide (truncates toward zero). pow10 is never zero, so
        // `checked_div` only returns None on overflow of pow10 itself.
        core::cmp::Ordering::Greater => amt.checked_div(pow10(decimals - TRIMMED_DECIMALS)?),
        // from < 8: multiply (may overflow 256-bit → None).
        core::cmp::Ordering::Less => amt.checked_mul(pow10(TRIMMED_DECIMALS - decimals)?),
    }
}

/// Bounds-checked cursor read of `n` bytes; advances `cursor`.
fn take<'a>(
    data: &'a [u8],
    cursor: &mut usize,
    n: usize,
) -> Result<&'a [u8], GlobalAccountantError> {
    let end = cursor
        .checked_add(n)
        .ok_or(GlobalAccountantError::InvalidInstructionData)?;
    if end > data.len() {
        return Err(GlobalAccountantError::InvalidInstructionData);
    }
    let slice = &data[*cursor..end];
    *cursor = end;
    Ok(slice)
}

fn read_u8(data: &[u8], cursor: &mut usize) -> Result<u8, GlobalAccountantError> {
    Ok(take(data, cursor, 1)?[0])
}

fn read_u16(data: &[u8], cursor: &mut usize) -> Result<u16, GlobalAccountantError> {
    let s = take(data, cursor, 2)?;
    Ok(u16::from_be_bytes([s[0], s[1]]))
}

fn read_u64(data: &[u8], cursor: &mut usize) -> Result<u64, GlobalAccountantError> {
    let s = take(data, cursor, 8)?;
    let mut b = [0u8; 8];
    b.copy_from_slice(s);
    Ok(u64::from_be_bytes(b))
}

fn read_addr32(data: &[u8], cursor: &mut usize) -> Result<[u8; 32], GlobalAccountantError> {
    let s = take(data, cursor, 32)?;
    let mut b = [0u8; 32];
    b.copy_from_slice(s);
    Ok(b)
}

fn expect_prefix(
    data: &[u8],
    cursor: &mut usize,
    prefix: &[u8; 4],
) -> Result<(), GlobalAccountantError> {
    let s = take(data, cursor, 4)?;
    if s != prefix {
        return Err(GlobalAccountantError::InvalidInstructionData);
    }
    Ok(())
}

/// The accountant-relevant fields of an NTT transfer: the normalized transfer
/// amount and the recipient chain. Token identity is intentionally absent — it
/// comes from the transceiver-hub mapping, not the message's `source_token`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NttTransfer {
    /// Amount normalized to [`TRIMMED_DECIMALS`].
    pub amount: Uint256,
    /// `to_chain` from the `NativeTokenTransfer` (the recipient chain).
    pub recipient_chain: u16,
}

/// Parse a `TransceiverMessage<WormholeTransceiver, NativeTokenTransfer>` and
/// extract the accountant-relevant fields. Layout (big-endian):
///
/// ```text
/// TransceiverMessage:
///   prefix(4)=99 45 FF 10 | source_ntt_manager(32) | recipient_ntt_manager(32)
///   | ntt_manager_payload_len(u16) | NttManagerMessage | transceiver_payload_len(u16) | …
/// NttManagerMessage:
///   id(32) | sender(32) | payload_len(u16) | NativeTokenTransfer
/// NativeTokenTransfer:
///   prefix(4)=99 4E 54 54 | TrimmedAmount[decimals(1), amount(u64)]
///   | source_token(32) | to(32) | to_chain(u16)
/// ```
///
/// Nested length prefixes are consumed but not enforced against the inner
/// fixed-size payloads (matching the sequential `wormhole-io` read). Only
/// well-formed, guardian-quorum'd messages reach accounting.
pub fn parse_ntt_transfer(payload: &[u8]) -> Result<NttTransfer, GlobalAccountantError> {
    let c = &mut 0usize;

    // --- TransceiverMessage ---
    expect_prefix(payload, c, &TRANSCEIVER_MESSAGE_PREFIX)?;
    let _source_ntt_manager = read_addr32(payload, c)?;
    let _recipient_ntt_manager = read_addr32(payload, c)?;
    let _ntt_manager_payload_len = read_u16(payload, c)?;

    // --- NttManagerMessage ---
    let _id = read_addr32(payload, c)?;
    let _sender = read_addr32(payload, c)?;
    let _inner_payload_len = read_u16(payload, c)?;

    // --- NativeTokenTransfer ---
    expect_prefix(payload, c, &NATIVE_TOKEN_TRANSFER_PREFIX)?;
    // TrimmedAmount: decimals FIRST, then the u64 amount.
    let decimals = read_u8(payload, c)?;
    let raw_amount = read_u64(payload, c)?;
    let _source_token = read_addr32(payload, c)?;
    let _to = read_addr32(payload, c)?;
    let recipient_chain = read_u16(payload, c)?;

    let amount = normalize_trimmed_amount(decimals, raw_amount)
        .ok_or(GlobalAccountantError::InvalidInstructionData)?;
    Ok(NttTransfer {
        amount,
        recipient_chain,
    })
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use alloc::vec::Vec;

    use super::*;

    #[test]
    fn normalize_scale_up() {
        // 3 decimals → 8: ×10^5. 1000 → 100_000_000.
        assert_eq!(
            normalize_trimmed_amount(3, 1000),
            Some(Uint256::from_u128(100_000_000))
        );
    }

    #[test]
    fn normalize_scale_down_truncates() {
        // 18 decimals → 8: ÷10^10. 1e18 → 100_000_000; 1 → 0 (truncation).
        assert_eq!(
            normalize_trimmed_amount(18, 1_000_000_000_000_000_000),
            Some(Uint256::from_u128(100_000_000))
        );
        assert_eq!(normalize_trimmed_amount(18, 1), Some(Uint256::ZERO));
    }

    #[test]
    fn normalize_identity_at_eight() {
        assert_eq!(
            normalize_trimmed_amount(8, 12_345),
            Some(Uint256::from_u128(12_345))
        );
    }

    /// Build a well-formed `TransceiverMessage` carrying one `NativeTokenTransfer`.
    fn build_msg(decimals: u8, raw_amount: u64, to_chain: u16) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&TRANSCEIVER_MESSAGE_PREFIX);
        v.extend_from_slice(&[0xAA; 32]); // source_ntt_manager
        v.extend_from_slice(&[0xBB; 32]); // recipient_ntt_manager
        v.extend_from_slice(&145u16.to_be_bytes()); // ntt_manager_payload_len (informational)
        v.extend_from_slice(&[0xCC; 32]); // id
        v.extend_from_slice(&[0xDD; 32]); // sender
        v.extend_from_slice(&79u16.to_be_bytes()); // inner payload_len (informational)
        v.extend_from_slice(&NATIVE_TOKEN_TRANSFER_PREFIX);
        v.push(decimals);
        v.extend_from_slice(&raw_amount.to_be_bytes());
        v.extend_from_slice(&[0xEE; 32]); // source_token (ignored)
        v.extend_from_slice(&[0xFF; 32]); // to (ignored)
        v.extend_from_slice(&to_chain.to_be_bytes());
        v
    }

    #[test]
    fn parse_extracts_normalized_amount_and_recipient_chain() {
        let msg = build_msg(8, 12_345, 10);
        let t = parse_ntt_transfer(&msg).unwrap();
        assert_eq!(t.amount, Uint256::from_u128(12_345));
        assert_eq!(t.recipient_chain, 10);

        // Scaling applies through the parse path too.
        let scaled = parse_ntt_transfer(&build_msg(3, 1000, 2)).unwrap();
        assert_eq!(scaled.amount, Uint256::from_u128(100_000_000));
        assert_eq!(scaled.recipient_chain, 2);
    }

    #[test]
    fn parse_rejects_bad_transceiver_prefix() {
        let mut msg = build_msg(8, 1, 1);
        msg[0] = 0x00;
        assert_eq!(
            parse_ntt_transfer(&msg),
            Err(GlobalAccountantError::InvalidInstructionData)
        );
    }

    #[test]
    fn parse_rejects_bad_ntt_prefix() {
        let mut msg = build_msg(8, 1, 1);
        // NativeTokenTransfer prefix starts at 4+32+32+2+32+32+2 = 138.
        msg[138] = 0x00;
        assert_eq!(
            parse_ntt_transfer(&msg),
            Err(GlobalAccountantError::InvalidInstructionData)
        );
    }

    #[test]
    fn parse_rejects_truncated() {
        let msg = build_msg(8, 1, 1);
        assert_eq!(
            parse_ntt_transfer(&msg[..msg.len() - 1]),
            Err(GlobalAccountantError::InvalidInstructionData)
        );
    }
}
