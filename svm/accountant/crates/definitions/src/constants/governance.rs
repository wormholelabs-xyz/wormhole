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

/// Accountant governance `UpgradeContract` action byte.
pub const UPGRADE_CONTRACT_ACTION: u8 = 0x02;

/// NTT relayer governance module — first 32 bytes of a `WormholeRelayer`
/// governance payload. "WormholeRelayer" (15 ASCII bytes) right-aligned in 32
/// bytes (17 leading zero bytes). The NTT `RegisterRelayerChain` handler
/// validates this module before initialising the relayer-chain registration.
pub const RELAYER_GOVERNANCE_MODULE: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, b'W', b'o', b'r', b'm', b'h', b'o', b'l', b'e', b'R', b'e', b'l', b'a', b'y', b'e', b'r',
];

/// NTT accountant governance module — first 32 bytes of an NTT `ModifyBalance`
/// payload. "NTTGlobalAccountant" (19 ASCII bytes) right-aligned in 32 bytes
/// (13 leading zero bytes). Same payload layout as the WTT
/// [`ACCOUNTANT_GOVERNANCE_MODULE`]; only the module string differs, which is
/// what scopes a `ModifyBalance` VAA to the NTT program.
pub const NTT_ACCOUNTANT_GOVERNANCE_MODULE: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, b'N', b'T', b'T',
    b'G', b'l', b'o', b'b', b'a', b'l', b'A', b'c', b'c', b'o', b'u', b'n', b't', b'a', b'n', b't',
];

#[cfg(test)]
mod tests {
    use super::*;
    use wormhole_sdk::accountant_modification::ModificationKind;
    use wormhole_sdk::{accountant, token, Address, Amount, Chain};

    #[test]
    fn matches_sdk() {
        assert_eq!(GOVERNANCE_EMITTER, wormhole_sdk::GOVERNANCE_EMITTER.0);
        assert_eq!(SOLANA_CHAIN_ID, wormhole_svm_definitions::solana::CHAIN_ID);
        assert_eq!(SOLANA_CHAIN_ID, u16::from(Chain::Solana));
        assert_eq!(TOKEN_BRIDGE_GOVERNANCE_MODULE, token::MODULE);
        assert_eq!(ACCOUNTANT_GOVERNANCE_MODULE, accountant::MODULE);
        assert_eq!(ModificationKind::Add as u8, 1);
        assert_eq!(ModificationKind::Subtract as u8, 2);

        let register = serde_wormhole::to_vec(&token::GovernancePacket {
            chain: Chain::Solana,
            action: token::Action::RegisterChain {
                chain: Chain::Ethereum,
                emitter_address: Address([0x11; 32]),
            },
        })
        .unwrap();
        assert_eq!(register.len(), 69);
        assert_eq!(register[..32], TOKEN_BRIDGE_GOVERNANCE_MODULE);
        assert_eq!(register[32], REGISTER_CHAIN_ACTION);
        assert_eq!(register[33..35], SOLANA_CHAIN_ID.to_be_bytes());
        assert_eq!(register[35..37], u16::from(Chain::Ethereum).to_be_bytes());
        assert_eq!(register[37..69], [0x11; 32]);

        let modify = serde_wormhole::to_vec(&accountant::GovernancePacket {
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
        })
        .unwrap();
        assert_eq!(modify[..32], ACCOUNTANT_GOVERNANCE_MODULE);
        assert_eq!(modify[32], MODIFY_BALANCE_ACTION);
        assert_eq!(modify[33..35], SOLANA_CHAIN_ID.to_be_bytes());

        let upgrade = serde_wormhole::to_vec(&accountant::GovernancePacket {
            chain: Chain::Solana,
            action: accountant::Action::UpgradeContract {
                new_contract: Address([0x44; 32]),
            },
        })
        .unwrap();
        assert_eq!(upgrade.len(), 67);
        assert_eq!(upgrade[..32], ACCOUNTANT_GOVERNANCE_MODULE);
        assert_eq!(upgrade[32], UPGRADE_CONTRACT_ACTION);
        assert_eq!(upgrade[33..35], SOLANA_CHAIN_ID.to_be_bytes());
        assert_eq!(upgrade[35..67], [0x44; 32]);
    }
}
