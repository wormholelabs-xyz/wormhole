//! VAA body parsing: the noreplay namespace key and the Token Bridge payload.

use crate::error::GlobalAccountantError;
use crate::primitives::Uint256;

/// Fixed VAA body header length: timestamp (4) + nonce (4) + emitter_chain (2)
/// + emitter_address (32) + sequence (8) + consistency_level (1).
pub const VAA_BODY_HEADER_LEN: usize = 51;

// Byte offsets of the namespace-key fields within the VAA body header. These are
// `const`, so they inline at every use site and cost no extra compute units or
// binary size versus literal slice bounds.
const EMITTER_CHAIN_OFFSET: usize = 8;
const EMITTER_ADDRESS_OFFSET: usize = 10;
const SEQUENCE_OFFSET: usize = 42;

/// Replay-protection key parsed from the VAA body header. `chain` and `emitter`
/// form the noreplay namespace; `sequence` indexes the bitmap within it. The
/// triple keys all accountant state (the pending PDA, the noreplay slot, and the
/// commit log), so this struct and [`parse_vaa_namespace_key`] are the sole
/// authority for these offsets — do not re-derive them in instruction modules.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VaaNamespaceKey {
    /// `emitter_chain`, body bytes `[8..10]` (u16 BE).
    pub chain: u16,
    /// `emitter_address`, body bytes `[10..42]`.
    pub emitter: [u8; 32],
    /// `sequence`, body bytes `[42..50]` (u64 BE).
    pub sequence: u64,
}

/// Parse the noreplay namespace key from a VAA body header. Rejects bodies
/// shorter than the 51-byte header with `InvalidInstructionData`.
pub fn parse_vaa_namespace_key(body: &[u8]) -> Result<VaaNamespaceKey, GlobalAccountantError> {
    if body.len() < VAA_BODY_HEADER_LEN {
        return Err(GlobalAccountantError::InvalidInstructionData);
    }
    let chain = u16::from_be_bytes([body[EMITTER_CHAIN_OFFSET], body[EMITTER_CHAIN_OFFSET + 1]]);
    let mut emitter = [0u8; 32];
    emitter.copy_from_slice(&body[EMITTER_ADDRESS_OFFSET..EMITTER_ADDRESS_OFFSET + 32]);
    let mut sequence_bytes = [0u8; 8];
    sequence_bytes.copy_from_slice(&body[SEQUENCE_OFFSET..SEQUENCE_OFFSET + 8]);
    Ok(VaaNamespaceKey {
        chain,
        emitter,
        sequence: u64::from_be_bytes(sequence_bytes),
    })
}

/// Decoded Token Bridge VAA payload, carrying only the fields the accountant
/// needs at quorum commit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenBridgeAction {
    /// Action 0x01 (`Transfer`) and 0x03 (`TransferWithPayload`) — same
    /// accountant logic; only these fields affect balances.
    Transfer {
        amount: Uint256,
        token_chain: u16,
        token_address: [u8; 32],
        recipient_chain: u16,
    },
    /// Action 0x02 (`Attest`) — moves no value; commit finishes but skips
    /// balance updates.
    Attest,
    /// Any other action byte. Both commit paths reject it with
    /// [`GlobalAccountantError::UnknownTokenBridgePayload`], leaving the
    /// NoReplay slot unconsumed for a future upgrade.
    Other,
}

/// Parse a VAA body's payload (bytes at `body[51..]`) into a
/// [`TokenBridgeAction`].
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
/// Token Bridge transfer payload, starting at offset 51:
///
/// | offset | size | field            |
/// |--------|------|------------------|
/// | 0      | 1    | action           |
/// | 1      | 32   | amount (Uint256) |
/// | 33     | 32   | token_address    |
/// | 65     | 2    | token_chain      |
/// | 67     | 32   | recipient        |
/// | 99     | 2    | recipient_chain  |
/// | 101    | 32   | fee (action 1)   |
/// | 133..  | rest | extra (action 3) |
///
/// Requires ≥ 52 bytes (to read the action), or ≥ 184 for transfer actions;
/// returns `InvalidInstructionData` on any short slice.
pub fn parse_token_bridge_payload(body: &[u8]) -> Result<TokenBridgeAction, GlobalAccountantError> {
    const ACTION_TRANSFER: u8 = 0x01;
    const ACTION_ATTEST: u8 = 0x02;
    const ACTION_TRANSFER_WITH_PAYLOAD: u8 = 0x03;
    const TRANSFER_PAYLOAD_MIN: usize = 1 + 32 + 32 + 2 + 32 + 2 + 32; // 133

    if body.len() <= VAA_BODY_HEADER_LEN {
        return Err(GlobalAccountantError::InvalidInstructionData);
    }
    let payload = &body[VAA_BODY_HEADER_LEN..];
    let action = payload[0];
    match action {
        ACTION_TRANSFER | ACTION_TRANSFER_WITH_PAYLOAD => {
            if payload.len() < TRANSFER_PAYLOAD_MIN {
                return Err(GlobalAccountantError::InvalidInstructionData);
            }
            let mut amount = [0u8; 32];
            amount.copy_from_slice(&payload[1..33]);
            let mut token_address = [0u8; 32];
            token_address.copy_from_slice(&payload[33..65]);
            let token_chain = u16::from_be_bytes([payload[65], payload[66]]);
            // payload[67..99] is recipient — ignored.
            let recipient_chain = u16::from_be_bytes([payload[99], payload[100]]);
            Ok(TokenBridgeAction::Transfer {
                amount: Uint256::from_be_bytes(amount),
                token_chain,
                token_address,
                recipient_chain,
            })
        }
        ACTION_ATTEST => Ok(TokenBridgeAction::Attest),
        _ => Ok(TokenBridgeAction::Other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 184-byte VAA body (51-byte header + 133-byte transfer payload)
    /// in a stack array.
    fn transfer_body(
        action: u8,
        amount: u128,
        token_address: [u8; 32],
        token_chain: u16,
        recipient_chain: u16,
    ) -> [u8; 184] {
        let mut body = [0u8; 184];
        // Header is zeroed; transfer payload starts at offset 51.
        body[51] = action;
        // amount: 32-byte BE, low 16 bytes hold the u128.
        body[52 + 16..52 + 32].copy_from_slice(&amount.to_be_bytes());
        body[84..116].copy_from_slice(&token_address);
        body[116..118].copy_from_slice(&token_chain.to_be_bytes());
        // recipient (118..150): recognisable bytes to catch off-by-one.
        body[118] = 0xAB;
        body[149] = 0xCD;
        body[150..152].copy_from_slice(&recipient_chain.to_be_bytes());
        body
    }

    #[test]
    fn parse_vaa_namespace_key_decodes_routing_tuple() {
        let mut body = [0u8; VAA_BODY_HEADER_LEN];
        body[8..10].copy_from_slice(&2u16.to_be_bytes());
        body[10] = 0xAA;
        body[41] = 0xBB;
        body[42..50].copy_from_slice(&0x0102_0304_0506_0708u64.to_be_bytes());

        let header = parse_vaa_namespace_key(&body).unwrap();
        assert_eq!(header.chain, 2);
        assert_eq!(header.emitter[0], 0xAA);
        assert_eq!(header.emitter[31], 0xBB);
        assert_eq!(header.sequence, 0x0102_0304_0506_0708);
    }

    #[test]
    fn parse_vaa_namespace_key_accepts_exact_header_len() {
        assert!(parse_vaa_namespace_key(&[0u8; VAA_BODY_HEADER_LEN]).is_ok());
    }

    #[test]
    fn parse_vaa_namespace_key_short_body_rejects() {
        assert_eq!(
            parse_vaa_namespace_key(&[0u8; VAA_BODY_HEADER_LEN - 1]),
            Err(GlobalAccountantError::InvalidInstructionData)
        );
    }

    #[test]
    fn parse_token_bridge_payload_transfer_decodes_amount_token_recipient() {
        let mut token_address = [0u8; 32];
        token_address[0] = 0x11;
        token_address[31] = 0x99;
        let body = transfer_body(0x01, 1_000_000_u128, token_address, 2, 10);
        let action = parse_token_bridge_payload(&body).expect("transfer parses");
        match action {
            TokenBridgeAction::Transfer {
                amount,
                token_chain,
                token_address: ta,
                recipient_chain,
            } => {
                assert_eq!(amount, Uint256::from_u128(1_000_000));
                assert_eq!(token_chain, 2);
                assert_eq!(ta, token_address);
                assert_eq!(recipient_chain, 10);
            }
            other => panic!("expected Transfer, got {other:?}"),
        }
    }

    #[test]
    fn parse_token_bridge_payload_transfer_with_payload_same_as_transfer() {
        // Action 0x03 must decode to the same Transfer variant as 0x01.
        let token_address = [0x42u8; 32];
        let body_01 = transfer_body(0x01, 99, token_address, 5, 7);
        let body_03 = transfer_body(0x03, 99, token_address, 5, 7);
        let a = parse_token_bridge_payload(&body_01).unwrap();
        let b = parse_token_bridge_payload(&body_03).unwrap();
        assert_eq!(a, b, "action 0x01 and 0x03 must decode identically");
    }

    #[test]
    fn parse_token_bridge_payload_attest() {
        // Action 0x02 only needs the one-byte action past the 51-byte header.
        let mut body = [0u8; 52];
        body[51] = 0x02;
        let action = parse_token_bridge_payload(&body).expect("attest parses");
        assert_eq!(action, TokenBridgeAction::Attest);
    }

    #[test]
    fn parse_token_bridge_payload_unknown_action() {
        // Any byte other than 0x01/0x02/0x03 decodes to Other.
        let mut body = [0u8; 52];
        body[51] = 0x77;
        let action = parse_token_bridge_payload(&body).expect("unknown action parses");
        assert_eq!(action, TokenBridgeAction::Other);
    }

    #[test]
    fn parse_token_bridge_payload_short_body_rejects() {
        // 51-byte body (no action byte) must reject.
        let body = [0u8; 51];
        let err = parse_token_bridge_payload(&body).unwrap_err();
        assert_eq!(err, GlobalAccountantError::InvalidInstructionData);
    }

    #[test]
    fn parse_token_bridge_payload_short_transfer_payload_rejects() {
        // Header + action 0x01 + 10 bytes — short of the 133-byte minimum.
        let mut body = [0u8; 62];
        body[51] = 0x01;
        let err = parse_token_bridge_payload(&body).unwrap_err();
        assert_eq!(err, GlobalAccountantError::InvalidInstructionData);
    }
}
