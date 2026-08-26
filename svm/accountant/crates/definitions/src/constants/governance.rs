//! Wormhole governance identifiers: emitter, chain IDs, module and action bytes.

/// Wormhole governance emitter: chain 1, address `[0; 31] || 0x04`.
pub const GOVERNANCE_EMITTER: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04,
];

/// Wormhole chain ID for Solana. Governance VAAs target this or `0x0000` (Any).
pub const SOLANA_CHAIN_ID: u16 = 1;

/// Token Bridge governance module: "TokenBridge" right-aligned in 32 bytes.
pub const TOKEN_BRIDGE_GOVERNANCE_MODULE: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, b'T', b'o', b'k', b'e', b'n', b'B', b'r', b'i', b'd', b'g', b'e',
];

/// Token Bridge governance `RegisterChain` action byte.
pub const REGISTER_CHAIN_ACTION: u8 = 0x01;

/// Accountant governance module: "GlobalAccountant" right-aligned in 32 bytes.
/// The module, not the action byte, distinguishes `ModifyBalance` from `RegisterChain`.
pub const ACCOUNTANT_GOVERNANCE_MODULE: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    b'G', b'l', b'o', b'b', b'a', b'l', b'A', b'c', b'c', b'o', b'u', b'n', b't', b'a', b'n', b't',
];

/// Accountant governance `ModifyBalance` action byte.
pub const MODIFY_BALANCE_ACTION: u8 = 0x01;

#[cfg(test)]
mod tests {
    use super::*;
    use wormhole_sdk::accountant_modification::ModificationKind;
    use wormhole_sdk::{accountant, token, Address, Amount, Chain};

    #[test]
    fn governance_emitter_matches_sdk() {
        assert_eq!(GOVERNANCE_EMITTER, wormhole_sdk::GOVERNANCE_EMITTER.0);
    }

    #[test]
    fn solana_chain_id_matches_canonical_sources() {
        assert_eq!(SOLANA_CHAIN_ID, wormhole_svm_definitions::solana::CHAIN_ID);
        assert_eq!(SOLANA_CHAIN_ID, u16::from(Chain::Solana));
    }

    #[test]
    fn governance_modules_match_sdk() {
        assert_eq!(TOKEN_BRIDGE_GOVERNANCE_MODULE, token::MODULE);
        assert_eq!(ACCOUNTANT_GOVERNANCE_MODULE, accountant::MODULE);
    }

    /// SDK-encoded `RegisterChain` packet: `module ‖ action ‖ target_chain_be ‖ ...`.
    #[test]
    fn register_chain_wire_prefix_matches_sdk_encoding() {
        let packet = token::GovernancePacket {
            chain: Chain::Solana,
            action: token::Action::RegisterChain {
                chain: Chain::Ethereum,
                emitter_address: Address([0x11; 32]),
            },
        };
        let bytes = serde_wormhole::to_vec(&packet).expect("encode");
        assert_eq!(bytes[..32], TOKEN_BRIDGE_GOVERNANCE_MODULE);
        assert_eq!(bytes[32], REGISTER_CHAIN_ACTION);
        assert_eq!(bytes[33..35], SOLANA_CHAIN_ID.to_be_bytes());
        assert_eq!(bytes[35..37], u16::from(Chain::Ethereum).to_be_bytes());
        assert_eq!(bytes[37..69], [0x11; 32]);
        assert_eq!(bytes.len(), 69);
    }

    /// SDK-encoded `ModifyBalance` packet: `module ‖ action ‖ target_chain_be ‖ ...`.
    #[test]
    fn modify_balance_wire_prefix_matches_sdk_encoding() {
        let packet = accountant::GovernancePacket {
            chain: Chain::Solana,
            action: accountant::Action::ModifyBalance {
                sequence: 7,
                chain_id: 2,
                token_chain: 2,
                token_address: Address([0x22; 32]),
                kind: ModificationKind::Add,
                amount: Amount([0x33; 32]),
                reason: "test".into(),
            },
        };
        let bytes = serde_wormhole::to_vec(&packet).expect("encode");
        assert_eq!(bytes[..32], ACCOUNTANT_GOVERNANCE_MODULE);
        assert_eq!(bytes[32], MODIFY_BALANCE_ACTION);
        assert_eq!(bytes[33..35], SOLANA_CHAIN_ID.to_be_bytes());
    }

    /// `modify_balance` kind bytes: 1 = Add, 2 = Subtract.
    #[test]
    fn modification_kind_bytes_match_sdk() {
        assert_eq!(ModificationKind::Add as u8, 1);
        assert_eq!(ModificationKind::Subtract as u8, 2);
    }
}
