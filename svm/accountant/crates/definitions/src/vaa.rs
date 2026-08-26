//! VAA body parsing: the noreplay namespace key and the Token Bridge payload.
//!
//! Wire layouts follow `sdk/vaa/structs.go` (`Unmarshal`,
//! `DecodeTransferPayloadHdr`).

use crate::error::GlobalAccountantError;
use crate::primitives::Uint256;

/// VAA body header length: timestamp (4) + nonce (4) + emitter_chain (2)
/// + emitter_address (32) + sequence (8) + consistency_level (1).
pub const VAA_BODY_HEADER_LEN: usize = 51;

// Body header field offsets.
const EMITTER_CHAIN_OFFSET: usize = 8;
const EMITTER_ADDRESS_OFFSET: usize = 10;
const SEQUENCE_OFFSET: usize = 42;
const CONSISTENCY_LEVEL_OFFSET: usize = 50;

// Token Bridge payload field offsets, relative to the payload start.
const ACTION_OFFSET: usize = 0;
const AMOUNT_OFFSET: usize = 1;
const TOKEN_ADDRESS_OFFSET: usize = 33;
const TOKEN_CHAIN_OFFSET: usize = 65;
const RECIPIENT_OFFSET: usize = 67;
const RECIPIENT_CHAIN_OFFSET: usize = 99;
const FEE_OFFSET: usize = 101;
/// Minimum transfer payload length: action 0x01 is exactly this long;
/// action 0x03 appends an arbitrary payload.
const TRANSFER_PAYLOAD_MIN: usize = 133;

const ACTION_TRANSFER: u8 = 0x01;
const ACTION_ATTEST: u8 = 0x02;
const ACTION_TRANSFER_WITH_PAYLOAD: u8 = 0x03;

// Field offsets must tile the header and the transfer payload without gaps.
const _: () = {
    assert!(EMITTER_CHAIN_OFFSET == 4 + 4);
    assert!(EMITTER_CHAIN_OFFSET + 2 == EMITTER_ADDRESS_OFFSET);
    assert!(EMITTER_ADDRESS_OFFSET + 32 == SEQUENCE_OFFSET);
    assert!(SEQUENCE_OFFSET + 8 == CONSISTENCY_LEVEL_OFFSET);
    assert!(CONSISTENCY_LEVEL_OFFSET + 1 == VAA_BODY_HEADER_LEN);

    assert!(ACTION_OFFSET + 1 == AMOUNT_OFFSET);
    assert!(AMOUNT_OFFSET + 32 == TOKEN_ADDRESS_OFFSET);
    assert!(TOKEN_ADDRESS_OFFSET + 32 == TOKEN_CHAIN_OFFSET);
    assert!(TOKEN_CHAIN_OFFSET + 2 == RECIPIENT_OFFSET);
    assert!(RECIPIENT_OFFSET + 32 == RECIPIENT_CHAIN_OFFSET);
    assert!(RECIPIENT_CHAIN_OFFSET + 2 == FEE_OFFSET);
    assert!(FEE_OFFSET + 32 == TRANSFER_PAYLOAD_MIN);
    // Guardian SDK `DecodeTransferPayloadHdr` requires 101 bytes; 133 is a superset.
    assert!(TRANSFER_PAYLOAD_MIN >= FEE_OFFSET);
};

/// Copy `N` bytes at `offset` into a fixed array. Returns `None` when `buf`
/// is too short.
#[inline]
fn read_array<const N: usize>(buf: &[u8], offset: usize) -> Option<[u8; N]> {
    let end = offset.checked_add(N)?;
    buf.get(offset..end)?.try_into().ok()
}

/// Replay-protection key from the VAA body header. `chain` and `emitter`
/// select the noreplay namespace; `sequence` indexes the bitmap. The triple
/// keys the pending PDA, the noreplay slot, and the commit log.
/// [`parse_vaa_namespace_key`] is the single source of these offsets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VaaNamespaceKey {
    /// `emitter_chain`, body bytes `[8..10]` (u16 BE).
    pub chain: u16,
    /// `emitter_address`, body bytes `[10..42]`.
    pub emitter: [u8; 32],
    /// `sequence`, body bytes `[42..50]` (u64 BE).
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
    let header: &[u8; VAA_BODY_HEADER_LEN] = body
        .get(..VAA_BODY_HEADER_LEN)
        .and_then(|h| h.try_into().ok())
        .ok_or(GlobalAccountantError::InvalidInstructionData)?;

    let chain = read_array::<2>(header, EMITTER_CHAIN_OFFSET)
        .ok_or(GlobalAccountantError::InvalidInstructionData)?;
    let emitter = read_array::<32>(header, EMITTER_ADDRESS_OFFSET)
        .ok_or(GlobalAccountantError::InvalidInstructionData)?;
    let sequence = read_array::<8>(header, SEQUENCE_OFFSET)
        .ok_or(GlobalAccountantError::InvalidInstructionData)?;

    Ok(VaaNamespaceKey {
        chain: u16::from_be_bytes(chain),
        emitter,
        sequence: u64::from_be_bytes(sequence),
    })
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
/// VAA body layout:
///
/// | offset | size | field              |
/// |--------|------|--------------------|
/// | 0      | 4    | timestamp (u32 BE) |
/// | 4      | 4    | nonce (u32 BE)     |
/// | 8      | 2    | emitter_chain      |
/// | 10     | 32   | emitter_address    |
/// | 42     | 8    | sequence (u64 BE)  |
/// | 50     | 1    | consistency_level  |
/// | 51..   | rest | payload            |
///
/// Token Bridge transfer payload, offsets relative to 51:
///
/// | offset | size | field            |
/// |--------|------|------------------|
/// | 0      | 1    | action           |
/// | 1      | 32   | amount (Uint256) |
/// | 33     | 32   | token_address    |
/// | 65     | 2    | token_chain      |
/// | 67     | 32   | recipient        |
/// | 99     | 2    | recipient_chain  |
/// | 101    | 32   | fee              |
/// | 133..  | rest | extra (0x03)     |
///
/// SECURITY: precondition `body.len() >= 52`; transfer actions require
/// `payload.len() >= 133`. Short input returns `InvalidInstructionData`.
/// Every field read is bounds-checked; the function cannot panic.
///
/// SECURITY: the guardian accountant accepts action 0x01 payloads by the same
/// `>= 133` rule. Do not tighten to `== 133`: a stricter parser here would
/// reject a VAA the network already accounted and fork balance state.
pub fn parse_token_bridge_payload(body: &[u8]) -> Result<TokenBridgeAction, GlobalAccountantError> {
    let payload = body
        .get(VAA_BODY_HEADER_LEN..)
        .ok_or(GlobalAccountantError::InvalidInstructionData)?;
    let action = *payload
        .get(ACTION_OFFSET)
        .ok_or(GlobalAccountantError::InvalidInstructionData)?;

    match action {
        ACTION_TRANSFER | ACTION_TRANSFER_WITH_PAYLOAD => {
            if payload.len() < TRANSFER_PAYLOAD_MIN {
                return Err(GlobalAccountantError::InvalidInstructionData);
            }
            let amount = read_array::<32>(payload, AMOUNT_OFFSET)
                .ok_or(GlobalAccountantError::InvalidInstructionData)?;
            let token_address = read_array::<32>(payload, TOKEN_ADDRESS_OFFSET)
                .ok_or(GlobalAccountantError::InvalidInstructionData)?;
            let token_chain = read_array::<2>(payload, TOKEN_CHAIN_OFFSET)
                .ok_or(GlobalAccountantError::InvalidInstructionData)?;
            let recipient_chain = read_array::<2>(payload, RECIPIENT_CHAIN_OFFSET)
                .ok_or(GlobalAccountantError::InvalidInstructionData)?;
            Ok(TokenBridgeAction::Transfer {
                amount: Uint256::from_be_bytes(amount),
                token_chain: u16::from_be_bytes(token_chain),
                token_address,
                recipient_chain: u16::from_be_bytes(recipient_chain),
            })
        }
        ACTION_ATTEST => Ok(TokenBridgeAction::Attest),
        other => Ok(TokenBridgeAction::Other(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HDR: usize = VAA_BODY_HEADER_LEN;
    const MIN_TRANSFER_BODY: usize = HDR + TRANSFER_PAYLOAD_MIN; // 184

    /// Mainnet VAAs, envelope included (13 signatures, body at 6 + 66 * 13).
    const FIXTURE_TRANSFER_SEQ1395207: &[u8] = include_bytes!(
        "../../../programs/global-accountant/tests/fixtures/mainnet_solana_token_bridge_transfer_seq1395207.vaa"
    );
    const FIXTURE_OTHER_SEQ2211: &[u8] = include_bytes!(
        "../../../programs/global-accountant/tests/fixtures/mainnet_solana_token_bridge_seq2211.vaa"
    );

    /// Strip the VAA envelope: version (1) + guardian set index (4) +
    /// signature count (1) + 66 bytes per signature.
    fn fixture_body(vaa: &[u8]) -> &[u8] {
        let n_sigs = vaa[5] as usize;
        assert_eq!(n_sigs, 13, "fixtures carry 13 signatures");
        &vaa[6 + 66 * n_sigs..]
    }

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
        let body = fixture_body(FIXTURE_TRANSFER_SEQ1395207);
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
        let body = fixture_body(FIXTURE_OTHER_SEQ2211);
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
        let ours = [
            ("type", ACTION_OFFSET),
            ("amount", AMOUNT_OFFSET),
            ("origin_address", TOKEN_ADDRESS_OFFSET),
            ("origin_chain", TOKEN_CHAIN_OFFSET),
            ("target_address", RECIPIENT_OFFSET),
            ("target_chain", RECIPIENT_CHAIN_OFFSET),
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
