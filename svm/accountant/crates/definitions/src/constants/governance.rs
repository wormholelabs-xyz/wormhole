//! Wormhole governance identifiers: emitter, chain IDs, module and action bytes.

use bytemuck::{Pod, Zeroable};

/// Wormhole governance emitter: chain 1, address `[0; 31] || 0x04`.
pub const GOVERNANCE_EMITTER: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04,
];

/// Wormhole chain ID for Solana. Governance VAAs target this or `0x0000` (Any).
pub const SOLANA_CHAIN_ID: u16 = 1;

/// Wormhole chain ID for Wormchain, home of the retiring cosmwasm accountant.
pub const WORMCHAIN_CHAIN_ID: u16 = 3104;

/// `RegisterChain` target chains accepted during the wormchain -> Solana migration window:
/// `0` (Any), Solana, Wormchain. Post-cutover the list becomes `[0, SOLANA_CHAIN_ID]`.
pub const ACCEPTED_REGISTER_CHAIN_TARGETS: &[u16] = &[0, SOLANA_CHAIN_ID, WORMCHAIN_CHAIN_ID];

/// `ModifyBalance` target chains accepted during the wormchain -> Solana migration window:
/// Solana, Wormchain. Post-cutover the list becomes `[SOLANA_CHAIN_ID]`.
pub const ACCEPTED_MODIFY_BALANCE_TARGETS: &[u16] = &[SOLANA_CHAIN_ID, WORMCHAIN_CHAIN_ID];

/// Governance module identifier: an ASCII name right-aligned in 32 zero-padded bytes,
/// the first field of every governance payload (`sdk/vaa/governance.go`).
///
/// Each accountant program validates governance VAAs against its own module, so a
/// VAA for one program can never act on another.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct GovernanceModule(pub [u8; 32]);

impl GovernanceModule {
    /// `name` right-aligned in 32 bytes. Compile-time error when `name` exceeds 32 bytes.
    pub const fn from_name(name: &str) -> Self {
        let bytes = name.as_bytes();
        assert!(bytes.len() <= 32);
        let mut out = [0u8; 32];
        let start = 32 - bytes.len();
        let mut i = 0;
        while i < bytes.len() {
            out[start + i] = bytes[i];
            i += 1;
        }
        Self(out)
    }
}

/// Token Bridge governance module.
pub const TOKEN_BRIDGE_GOVERNANCE_MODULE: GovernanceModule =
    GovernanceModule::from_name("TokenBridge");

/// Token Bridge governance `RegisterChain` action byte.
pub const REGISTER_CHAIN_ACTION: u8 = 0x01;

/// Accountant governance module.
/// The module, not the action byte, distinguishes `ModifyBalance` from `RegisterChain`.
pub const ACCOUNTANT_GOVERNANCE_MODULE: GovernanceModule =
    GovernanceModule::from_name("GlobalAccountant");

/// Standard Relayer governance module; its `RegisterChain` sets the relayer emitter per chain.
pub const RELAYER_GOVERNANCE_MODULE: GovernanceModule =
    GovernanceModule::from_name("WormholeRelayer");

/// NTT accountant governance module.
pub const NTT_ACCOUNTANT_GOVERNANCE_MODULE: GovernanceModule =
    GovernanceModule::from_name("NTTGlobalAccountant");

/// Accountant governance `ModifyBalance` action byte.
pub const MODIFY_BALANCE_ACTION: u8 = 0x01;

/// Accountant governance `UpgradeContract` action byte.
pub const UPGRADE_CONTRACT_ACTION: u8 = 0x02;

#[cfg(test)]
mod tests {
    use super::*;
    use wormhole_sdk::accountant_modification::ModificationKind;
    use wormhole_sdk::{accountant, ntt_accountant, relayer, token, Address, Amount, Chain};

    #[test]
    fn matches_sdk() {
        assert_eq!(GOVERNANCE_EMITTER, wormhole_sdk::GOVERNANCE_EMITTER.0);
        assert_eq!(SOLANA_CHAIN_ID, wormhole_svm_definitions::solana::CHAIN_ID);
        assert_eq!(SOLANA_CHAIN_ID, u16::from(Chain::Solana));
        assert_eq!(WORMCHAIN_CHAIN_ID, u16::from(Chain::Wormchain));
        assert_eq!(TOKEN_BRIDGE_GOVERNANCE_MODULE.0, token::MODULE);
        assert_eq!(ACCOUNTANT_GOVERNANCE_MODULE.0, accountant::MODULE);
        assert_eq!(RELAYER_GOVERNANCE_MODULE.0, relayer::MODULE);
        assert_eq!(NTT_ACCOUNTANT_GOVERNANCE_MODULE.0, ntt_accountant::MODULE);
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
        assert_eq!(register[..32], TOKEN_BRIDGE_GOVERNANCE_MODULE.0);
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
        assert_eq!(modify[..32], ACCOUNTANT_GOVERNANCE_MODULE.0);
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
        assert_eq!(upgrade[..32], ACCOUNTANT_GOVERNANCE_MODULE.0);
        assert_eq!(upgrade[32], UPGRADE_CONTRACT_ACTION);
        assert_eq!(upgrade[33..35], SOLANA_CHAIN_ID.to_be_bytes());
        assert_eq!(upgrade[35..67], [0x44; 32]);
    }
}
