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

    const HDR: usize = VaaBodyHeader::LEN;
    const MIN_TRANSFER_BODY: usize = HDR + TokenBridgeTransfer::LEN; // 184

    use accountant_test_fixtures::{MAINNET_OTHER_SEQ2211, MAINNET_TRANSFER_SEQ1395207};

    /// Body with a 133-byte transfer payload; recipient bytes are marked to
    /// catch off-by-one reads.
    fn transfer_body(
        action: u8,
        amount: Uint256,
        token_address: [u8; 32],
        token_chain: u16,
        recipient_chain: u16,
    ) -> [u8; MIN_TRANSFER_BODY] {
        let mut body = [0u8; MIN_TRANSFER_BODY];
        body[HDR] = action;
        body[HDR + 1..HDR + 33].copy_from_slice(&amount.0);
        body[HDR + 33..HDR + 65].copy_from_slice(&token_address);
        body[HDR + 65..HDR + 67].copy_from_slice(&token_chain.to_be_bytes());
        body[HDR + 67] = 0xAB;
        body[HDR + 98] = 0xCD;
        body[HDR + 99..HDR + 101].copy_from_slice(&recipient_chain.to_be_bytes());
        body
    }

    fn header_body(chain: u16, emitter: [u8; 32], sequence: u64) -> [u8; HDR] {
        let mut body = [0u8; HDR];
        body[8..10].copy_from_slice(&chain.to_be_bytes());
        body[10..42].copy_from_slice(&emitter);
        body[42..50].copy_from_slice(&sequence.to_be_bytes());
        body
    }

    #[test]
    fn namespace_key_table() {
        struct Case<'a> {
            name: &'static str,
            body: &'a [u8],
            expect: Result<VaaNamespaceKey, GlobalAccountantError>,
        }
        let mut emitter_marked = [0u8; 32];
        emitter_marked[0] = 0xAA;
        emitter_marked[31] = 0xBB;
        let marked = header_body(2, emitter_marked, 0x0102_0304_0506_0708);
        let max = header_body(u16::MAX, [0xFF; 32], u64::MAX);
        let zero = [0u8; HDR];
        let short = [0xFFu8; HDR - 1];
        let empty: [u8; 0] = [];
        let mut long = [0xFFu8; HDR + 200];
        long[..HDR].copy_from_slice(&marked);

        let cases = [
            Case {
                name: "marked fields decode",
                body: &marked,
                expect: Ok(VaaNamespaceKey {
                    chain: 2,
                    emitter: emitter_marked,
                    sequence: 0x0102_0304_0506_0708,
                }),
            },
            Case {
                name: "max values",
                body: &max,
                expect: Ok(VaaNamespaceKey {
                    chain: u16::MAX,
                    emitter: [0xFF; 32],
                    sequence: u64::MAX,
                }),
            },
            Case {
                name: "zero body at exact header len",
                body: &zero,
                expect: Ok(VaaNamespaceKey {
                    chain: 0,
                    emitter: [0; 32],
                    sequence: 0,
                }),
            },
            Case {
                name: "trailing payload ignored",
                body: &long,
                expect: Ok(VaaNamespaceKey {
                    chain: 2,
                    emitter: emitter_marked,
                    sequence: 0x0102_0304_0506_0708,
                }),
            },
            Case {
                name: "one byte short",
                body: &short,
                expect: Err(GlobalAccountantError::InvalidInstructionData),
            },
            Case {
                name: "empty",
                body: &empty,
                expect: Err(GlobalAccountantError::InvalidInstructionData),
            },
        ];
        assert!(!cases.is_empty());
        for c in &cases {
            assert_eq!(parse_vaa_namespace_key(c.body), c.expect, "{}", c.name);
        }
    }

    #[test]
    fn token_bridge_payload_table() {
        struct Case<'a> {
            name: &'static str,
            body: &'a [u8],
            expect: Result<TokenBridgeAction, GlobalAccountantError>,
        }
        let mut token_address = [0u8; 32];
        token_address[0] = 0x11;
        token_address[31] = 0x99;
        let transfer_01 = transfer_body(0x01, Uint256::from_u128(1_000_000), token_address, 2, 10);
        let transfer_03 = transfer_body(0x03, Uint256::from_u128(1_000_000), token_address, 2, 10);
        let transfer_max = transfer_body(0x01, Uint256::MAX, [0xFF; 32], u16::MAX, u16::MAX);
        let mut transfer_03_extra = [0u8; MIN_TRANSFER_BODY + 40];
        transfer_03_extra[..MIN_TRANSFER_BODY].copy_from_slice(&transfer_03);
        transfer_03_extra[MIN_TRANSFER_BODY..].fill(0xEE);
        let mut transfer_132 = [0u8; MIN_TRANSFER_BODY - 1];
        transfer_132.copy_from_slice(&transfer_01[..MIN_TRANSFER_BODY - 1]);
        let mut attest = [0u8; HDR + 1];
        attest[HDR] = 0x02;
        let mut attest_long = [0u8; HDR + 100];
        attest_long[HDR] = 0x02;
        let mut action_00 = [0u8; HDR + 1];
        action_00[HDR] = 0x00;
        let mut action_ff = [0u8; HDR + 1];
        action_ff[HDR] = 0xFF;
        let mut action_77 = [0u8; HDR + 1];
        action_77[HDR] = 0x77;
        let header_only = [0u8; HDR];
        let mut transfer_short = [0u8; HDR + 11];
        transfer_short[HDR] = 0x01;
        let empty: [u8; 0] = [];

        let expected_transfer = Ok(TokenBridgeAction::Transfer {
            amount: Uint256::from_u128(1_000_000),
            token_chain: 2,
            token_address,
            recipient_chain: 10,
        });
        let cases = [
            Case {
                name: "action 0x01 exact 133",
                body: &transfer_01,
                expect: expected_transfer,
            },
            Case {
                name: "action 0x03 decodes as 0x01",
                body: &transfer_03,
                expect: expected_transfer,
            },
            Case {
                name: "action 0x03 with trailing payload",
                body: &transfer_03_extra,
                expect: expected_transfer,
            },
            Case {
                name: "max field values",
                body: &transfer_max,
                expect: Ok(TokenBridgeAction::Transfer {
                    amount: Uint256::MAX,
                    token_chain: u16::MAX,
                    token_address: [0xFF; 32],
                    recipient_chain: u16::MAX,
                }),
            },
            Case {
                name: "attest at 52 bytes",
                body: &attest,
                expect: Ok(TokenBridgeAction::Attest),
            },
            Case {
                name: "attest with trailing bytes",
                body: &attest_long,
                expect: Ok(TokenBridgeAction::Attest),
            },
            Case {
                name: "action 0x00",
                body: &action_00,
                expect: Ok(TokenBridgeAction::Other(0x00)),
            },
            Case {
                name: "action 0x77",
                body: &action_77,
                expect: Ok(TokenBridgeAction::Other(0x77)),
            },
            Case {
                name: "action 0xFF",
                body: &action_ff,
                expect: Ok(TokenBridgeAction::Other(0xFF)),
            },
            Case {
                name: "transfer payload 132 bytes",
                body: &transfer_132,
                expect: Err(GlobalAccountantError::InvalidInstructionData),
            },
            Case {
                name: "transfer payload 11 bytes",
                body: &transfer_short,
                expect: Err(GlobalAccountantError::InvalidInstructionData),
            },
            Case {
                name: "header only, no action byte",
                body: &header_only,
                expect: Err(GlobalAccountantError::InvalidInstructionData),
            },
            Case {
                name: "empty",
                body: &empty,
                expect: Err(GlobalAccountantError::InvalidInstructionData),
            },
        ];
        assert!(!cases.is_empty());
        for c in &cases {
            assert_eq!(parse_token_bridge_payload(c.body), c.expect, "{}", c.name);
        }
    }

    #[test]
    fn mainnet_transfer_fixture_decodes() {
        let body = MAINNET_TRANSFER_SEQ1395207.body();
        assert_eq!(body.len(), MIN_TRANSFER_BODY);

        let key = parse_vaa_namespace_key(body).unwrap();
        let mut emitter = [0u8; 32];
        hex_into(
            "ec7372995d5cc8732397fb0ad35c0121e0eaa90d26f828a534cab54391b3a4f5",
            &mut emitter,
        );
        assert_eq!(
            key,
            VaaNamespaceKey {
                chain: 1,
                emitter,
                sequence: 1_395_207,
            }
        );

        let mut token_address = [0u8; 32];
        hex_into(
            "000000000000000000000000814e0908b12a99fecf5bc101bb5d0b8b5cdf7d26",
            &mut token_address,
        );
        assert_eq!(
            parse_token_bridge_payload(body),
            Ok(TokenBridgeAction::Transfer {
                amount: Uint256::from_u128(1_624_428_966_986),
                token_chain: 2,
                token_address,
                recipient_chain: 2,
            })
        );
    }

    #[test]
    fn mainnet_non_token_bridge_fixture_is_other() {
        let body = MAINNET_OTHER_SEQ2211.body();
        let key = parse_vaa_namespace_key(body).unwrap();
        assert_eq!(key.chain, 1);
        assert_eq!(key.sequence, 2211);
        assert_eq!(
            parse_token_bridge_payload(body),
            Ok(TokenBridgeAction::Other(0x99))
        );
    }

    /// Offsets match `DecodeTransferPayloadHdr` in `sdk/vaa/structs.go`:
    /// type at 0, amount 1..33, origin address 33..65, origin chain 65..67,
    /// target address 67..99, target chain 99..101.
    #[test]
    fn payload_offsets_match_guardian_sdk() {
        let go_sdk: [(&str, usize); 6] = [
            ("type", 0),
            ("amount", 1),
            ("origin_address", 33),
            ("origin_chain", 65),
            ("target_address", 67),
            ("target_chain", 99),
        ];
        use core::mem::offset_of;
        let ours = [
            ("type", offset_of!(TokenBridgeTransfer, action)),
            ("amount", offset_of!(TokenBridgeTransfer, amount)),
            (
                "origin_address",
                offset_of!(TokenBridgeTransfer, token_address),
            ),
            ("origin_chain", offset_of!(TokenBridgeTransfer, token_chain)),
            ("target_address", offset_of!(TokenBridgeTransfer, recipient)),
            (
                "target_chain",
                offset_of!(TokenBridgeTransfer, recipient_chain),
            ),
        ];
        assert_eq!(go_sdk, ours);
    }

    fn hex_into(hex: &str, out: &mut [u8]) {
        assert_eq!(hex.len(), out.len() * 2);
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
        }
    }
}
