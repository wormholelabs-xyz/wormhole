//! VAA body parsing: the noreplay namespace key and the Token Bridge payload.
//!
//! Wire layouts follow `sdk/vaa/structs.go` (`Unmarshal`,
//! `DecodeTransferPayloadHdr`).

use bytemuck::{Pod, Zeroable};

use crate::error::GlobalAccountantError;
use crate::primitives::Uint256;

const ACTION_TRANSFER: u8 = 0x01;
const ACTION_ATTEST: u8 = 0x02;
const ACTION_TRANSFER_WITH_PAYLOAD: u8 = 0x03;

/// Zero-copy view of the 51-byte VAA body header. All fields are byte arrays,
/// so the struct has alignment 1 and no padding; big-endian decoding happens
/// in the accessors.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct VaaBodyHeader {
    pub timestamp: [u8; 4],
    pub nonce: [u8; 4],
    pub emitter_chain: [u8; 2],
    pub emitter_address: [u8; 32],
    pub sequence: [u8; 8],
    pub consistency_level: u8,
}

const _: () = {
    use core::mem::offset_of;
    assert!(VaaBodyHeader::LEN == 51);
    assert!(offset_of!(VaaBodyHeader, emitter_chain) == 8);
    assert!(offset_of!(VaaBodyHeader, emitter_address) == 10);
    assert!(offset_of!(VaaBodyHeader, sequence) == 42);
    assert!(offset_of!(VaaBodyHeader, consistency_level) == 50);
};

impl VaaBodyHeader {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn new(
        timestamp: u32,
        nonce: u32,
        emitter_chain: u16,
        emitter_address: [u8; 32],
        sequence: u64,
        consistency_level: u8,
    ) -> Self {
        Self {
            timestamp: timestamp.to_be_bytes(),
            nonce: nonce.to_be_bytes(),
            emitter_chain: emitter_chain.to_be_bytes(),
            emitter_address,
            sequence: sequence.to_be_bytes(),
            consistency_level,
        }
    }

    /// Split `body` into the header view and the payload.
    ///
    /// SECURITY: precondition `body.len() >= 51`; otherwise
    /// `InvalidInstructionData`. Cannot panic.
    pub fn split(body: &[u8]) -> Result<(&Self, &[u8]), GlobalAccountantError> {
        let (head, payload) = body
            .split_at_checked(Self::LEN)
            .ok_or(GlobalAccountantError::InvalidInstructionData)?;
        let header = bytemuck::try_from_bytes(head)
            .map_err(|_| GlobalAccountantError::InvalidInstructionData)?;
        Ok((header, payload))
    }

    pub fn emitter_chain(&self) -> u16 {
        u16::from_be_bytes(self.emitter_chain)
    }

    pub fn sequence(&self) -> u64 {
        u64::from_be_bytes(self.sequence)
    }

    pub fn namespace_key(&self) -> VaaNamespaceKey {
        VaaNamespaceKey {
            chain: self.emitter_chain(),
            emitter: self.emitter_address,
            sequence: self.sequence(),
        }
    }
}

/// Zero-copy view of the fixed 133-byte head of a Token Bridge transfer
/// payload (action 0x01 is exactly this; 0x03 appends an arbitrary payload).
/// Layout per `sdk/vaa/structs.go` `DecodeTransferPayloadHdr`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct TokenBridgeTransfer {
    pub action: u8,
    pub amount: [u8; 32],
    pub token_address: [u8; 32],
    pub token_chain: [u8; 2],
    pub recipient: [u8; 32],
    pub recipient_chain: [u8; 2],
    pub fee: [u8; 32],
}

const _: () = {
    use core::mem::offset_of;
    assert!(TokenBridgeTransfer::LEN == 133);
    assert!(offset_of!(TokenBridgeTransfer, amount) == 1);
    assert!(offset_of!(TokenBridgeTransfer, token_address) == 33);
    assert!(offset_of!(TokenBridgeTransfer, token_chain) == 65);
    assert!(offset_of!(TokenBridgeTransfer, recipient) == 67);
    assert!(offset_of!(TokenBridgeTransfer, recipient_chain) == 99);
    // Guardian SDK `DecodeTransferPayloadHdr` requires 101 bytes; 133 is a superset.
    assert!(offset_of!(TokenBridgeTransfer, fee) == 101);
};

impl TokenBridgeTransfer {
    /// Action 0x01 is exactly this long; action 0x03 appends an arbitrary payload.
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn new(
        action: u8,
        amount: Uint256,
        token_address: [u8; 32],
        token_chain: u16,
        recipient: [u8; 32],
        recipient_chain: u16,
        fee: Uint256,
    ) -> Self {
        Self {
            action,
            amount: amount.0,
            token_address,
            token_chain: token_chain.to_be_bytes(),
            recipient,
            recipient_chain: recipient_chain.to_be_bytes(),
            fee: fee.0,
        }
    }

    pub fn token_chain(&self) -> u16 {
        u16::from_be_bytes(self.token_chain)
    }

    pub fn recipient_chain(&self) -> u16 {
        u16::from_be_bytes(self.recipient_chain)
    }
}

/// Replay-protection key from the VAA body header. `chain` and `emitter`
/// select the noreplay namespace; `sequence` indexes the bitmap. The triple
/// keys the pending PDA, the noreplay slot, and the commit log.
/// [`VaaBodyHeader`] is the single source of these offsets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VaaNamespaceKey {
    pub chain: u16,
    pub emitter: [u8; 32],
    pub sequence: u64,
}

/// Parse the noreplay namespace key from a VAA body.
///
/// SECURITY: precondition `body.len() >= 51`; otherwise returns
/// `InvalidInstructionData`. Every field read is bounds-checked; the function
/// cannot panic.
///
/// SECURITY: postcondition: the returned fields are byte-exact copies of body
/// bytes `[8..10]`, `[10..42]`, `[42..50]`.
pub fn parse_vaa_namespace_key(body: &[u8]) -> Result<VaaNamespaceKey, GlobalAccountantError> {
    let (header, _payload) = VaaBodyHeader::split(body)?;
    Ok(header.namespace_key())
}

/// Decoded Token Bridge payload. Carries only the fields the accountant
/// applies at quorum commit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenBridgeAction {
    /// Action 0x01 (`Transfer`) or 0x03 (`TransferWithPayload`). Both move
    /// balances identically.
    Transfer {
        amount: Uint256,
        token_chain: u16,
        token_address: [u8; 32],
        recipient_chain: u16,
    },
    /// Action 0x02 (`Attest`). Commit completes with no balance change.
    Attest,
    /// Any other action byte. Commit paths reject with
    /// [`GlobalAccountantError::UnknownTokenBridgePayload`] and leave the
    /// NoReplay slot free for a future upgrade.
    Other(u8),
}

/// Parse the payload at `body[51..]` into a [`TokenBridgeAction`].
///
/// Layouts: [`VaaBodyHeader`], [`TokenBridgeTransfer`].
///
/// SECURITY: precondition `body.len() >= 52`; transfer actions require
/// `payload.len() >= 133`. Short input returns `InvalidInstructionData`.
/// Every field read is bounds-checked; the function cannot panic.
///
/// SECURITY: the guardian accountant accepts action 0x01 payloads by the same
/// `>= 133` rule. Do not tighten to `== 133`: a stricter parser here would
/// reject a VAA the network already accounted and fork balance state.
pub fn parse_token_bridge_payload(body: &[u8]) -> Result<TokenBridgeAction, GlobalAccountantError> {
    let (_header, payload) = VaaBodyHeader::split(body)?;
    let action = *payload
        .first()
        .ok_or(GlobalAccountantError::InvalidInstructionData)?;

    match action {
        ACTION_TRANSFER | ACTION_TRANSFER_WITH_PAYLOAD => {
            let head = payload
                .get(..TokenBridgeTransfer::LEN)
                .ok_or(GlobalAccountantError::InvalidInstructionData)?;
            let transfer: &TokenBridgeTransfer = bytemuck::try_from_bytes(head)
                .map_err(|_| GlobalAccountantError::InvalidInstructionData)?;
            Ok(TokenBridgeAction::Transfer {
                amount: Uint256::from_be_bytes(transfer.amount),
                token_chain: transfer.token_chain(),
                token_address: transfer.token_address,
                recipient_chain: transfer.recipient_chain(),
            })
        }
        ACTION_ATTEST => Ok(TokenBridgeAction::Attest),
        other => Ok(TokenBridgeAction::Other(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use accountant_test_fixtures::{MAINNET_OTHER_SEQ2211, MAINNET_TRANSFER_SEQ1395207};

    const HDR: usize = VaaBodyHeader::LEN;
    const TRANSFER_BODY: usize = HDR + TokenBridgeTransfer::LEN;

    fn header_body(chain: u16, emitter: [u8; 32], sequence: u64) -> [u8; HDR] {
        let header = VaaBodyHeader::new(0, 0, chain, emitter, sequence, 0);
        let mut body = [0u8; HDR];
        body.copy_from_slice(bytemuck::bytes_of(&header));
        body
    }

    fn transfer_body(
        action: u8,
        amount: Uint256,
        token_address: [u8; 32],
        token_chain: u16,
        recipient_chain: u16,
    ) -> [u8; TRANSFER_BODY] {
        let mut recipient = [0u8; 32];
        recipient[0] = 0xAB;
        recipient[31] = 0xCD;
        let transfer = TokenBridgeTransfer::new(
            action,
            amount,
            token_address,
            token_chain,
            recipient,
            recipient_chain,
            Uint256::ZERO,
        );
        let mut body = [0u8; TRANSFER_BODY];
        body[HDR..].copy_from_slice(bytemuck::bytes_of(&transfer));
        body
    }

    fn action_body<const N: usize>(action: u8) -> [u8; N] {
        let mut body = [0u8; N];
        body[HDR] = action;
        body
    }

    fn hex(hex: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
        }
        out
    }

    #[test]
    fn parses_table() {
        use GlobalAccountantError as E;
        let mut emitter = [0u8; 32];
        emitter[0] = 0xAA;
        emitter[31] = 0xBB;
        let marked = header_body(2, emitter, 0x0102_0304_0506_0708);
        let marked_key = VaaNamespaceKey {
            chain: 2,
            emitter,
            sequence: 0x0102_0304_0506_0708,
        };
        let mut long = [0xFFu8; HDR + 200];
        long[..HDR].copy_from_slice(&marked);
        let namespace_cases: [(&str, &[u8], Result<VaaNamespaceKey, E>); 6] = [
            ("marked fields decode", &marked, Ok(marked_key)),
            (
                "max values",
                &header_body(u16::MAX, [0xFF; 32], u64::MAX),
                Ok(VaaNamespaceKey {
                    chain: u16::MAX,
                    emitter: [0xFF; 32],
                    sequence: u64::MAX,
                }),
            ),
            (
                "zero body at header len",
                &[0u8; HDR],
                Ok(VaaNamespaceKey {
                    chain: 0,
                    emitter: [0; 32],
                    sequence: 0,
                }),
            ),
            ("trailing payload ignored", &long, Ok(marked_key)),
            (
                "one byte short",
                &[0xFFu8; HDR - 1],
                Err(E::InvalidInstructionData),
            ),
            ("empty", &[], Err(E::InvalidInstructionData)),
        ];
        for (name, body, expected) in namespace_cases {
            assert_eq!(parse_vaa_namespace_key(body), expected, "{name}");
        }

        let mut token_address = [0u8; 32];
        token_address[0] = 0x11;
        token_address[31] = 0x99;
        let amount = Uint256::from_u128(1_000_000);
        let transfer_01 = transfer_body(0x01, amount, token_address, 2, 10);
        let transfer_03 = transfer_body(0x03, amount, token_address, 2, 10);
        let mut transfer_03_extra = [0xEEu8; TRANSFER_BODY + 40];
        transfer_03_extra[..TRANSFER_BODY].copy_from_slice(&transfer_03);
        // The guardian SDK (`DecodeTransferPayloadHdr`) requires >= 101 bytes and sets no
        // upper bound, so trailing bytes after an action 0x01 payload parse too.
        let mut transfer_01_extra = [0xEEu8; TRANSFER_BODY + 40];
        transfer_01_extra[..TRANSFER_BODY].copy_from_slice(&transfer_01);
        let expected_transfer = TokenBridgeAction::Transfer {
            amount,
            token_chain: 2,
            token_address,
            recipient_chain: 10,
        };
        let payload_cases: [(&str, &[u8], Result<TokenBridgeAction, E>); 14] = [
            ("action 0x01 exact 133", &transfer_01, Ok(expected_transfer)),
            (
                "action 0x01 with trailing payload",
                &transfer_01_extra,
                Ok(expected_transfer),
            ),
            (
                "action 0x03 decodes as 0x01",
                &transfer_03,
                Ok(expected_transfer),
            ),
            (
                "action 0x03 with trailing payload",
                &transfer_03_extra,
                Ok(expected_transfer),
            ),
            (
                "max field values",
                &transfer_body(0x01, Uint256::MAX, [0xFF; 32], u16::MAX, u16::MAX),
                Ok(TokenBridgeAction::Transfer {
                    amount: Uint256::MAX,
                    token_chain: u16::MAX,
                    token_address: [0xFF; 32],
                    recipient_chain: u16::MAX,
                }),
            ),
            (
                "attest at 52 bytes",
                &action_body::<{ HDR + 1 }>(0x02),
                Ok(TokenBridgeAction::Attest),
            ),
            (
                "attest with trailing bytes",
                &action_body::<{ HDR + 100 }>(0x02),
                Ok(TokenBridgeAction::Attest),
            ),
            (
                "action 0x00",
                &action_body::<{ HDR + 1 }>(0x00),
                Ok(TokenBridgeAction::Other(0x00)),
            ),
            (
                "action 0x77",
                &action_body::<{ HDR + 1 }>(0x77),
                Ok(TokenBridgeAction::Other(0x77)),
            ),
            (
                "action 0xFF",
                &action_body::<{ HDR + 1 }>(0xFF),
                Ok(TokenBridgeAction::Other(0xFF)),
            ),
            (
                "transfer payload 132 bytes",
                &transfer_01[..TRANSFER_BODY - 1],
                Err(E::InvalidInstructionData),
            ),
            (
                "transfer payload 11 bytes",
                &action_body::<{ HDR + 11 }>(0x01),
                Err(E::InvalidInstructionData),
            ),
            ("header only", &[0u8; HDR], Err(E::InvalidInstructionData)),
            ("empty", &[], Err(E::InvalidInstructionData)),
        ];
        for (name, body, expected) in payload_cases {
            assert_eq!(parse_token_bridge_payload(body), expected, "{name}");
        }
    }

    #[test]
    fn mainnet_fixtures_decode() {
        let transfer = MAINNET_TRANSFER_SEQ1395207.body();
        assert_eq!(transfer.len(), TRANSFER_BODY);
        assert_eq!(
            parse_vaa_namespace_key(transfer),
            Ok(VaaNamespaceKey {
                chain: 1,
                emitter: hex("ec7372995d5cc8732397fb0ad35c0121e0eaa90d26f828a534cab54391b3a4f5"),
                sequence: 1_395_207,
            })
        );
        assert_eq!(
            parse_token_bridge_payload(transfer),
            Ok(TokenBridgeAction::Transfer {
                amount: Uint256::from_u128(1_624_428_966_986),
                token_chain: 2,
                token_address: hex(
                    "000000000000000000000000814e0908b12a99fecf5bc101bb5d0b8b5cdf7d26"
                ),
                recipient_chain: 2,
            })
        );

        let other = MAINNET_OTHER_SEQ2211.body();
        let key = parse_vaa_namespace_key(other).unwrap();
        assert_eq!((key.chain, key.sequence), (1, 2211));
        assert_eq!(
            parse_token_bridge_payload(other),
            Ok(TokenBridgeAction::Other(0x99))
        );
    }

    #[test]
    fn payload_offsets_match_guardian_sdk() {
        use core::mem::offset_of;
        let cases: [(&str, usize, usize); 6] = [
            ("type", 0, offset_of!(TokenBridgeTransfer, action)),
            ("amount", 1, offset_of!(TokenBridgeTransfer, amount)),
            (
                "origin_address",
                33,
                offset_of!(TokenBridgeTransfer, token_address),
            ),
            (
                "origin_chain",
                65,
                offset_of!(TokenBridgeTransfer, token_chain),
            ),
            (
                "target_address",
                67,
                offset_of!(TokenBridgeTransfer, recipient),
            ),
            (
                "target_chain",
                99,
                offset_of!(TokenBridgeTransfer, recipient_chain),
            ),
        ];
        for (name, go_sdk, ours) in cases {
            assert_eq!(ours, go_sdk, "{name}");
        }
    }
}
