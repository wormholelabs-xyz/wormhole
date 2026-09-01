//! Governance VAA payload views: Token Bridge `RegisterChain` and accountant
//! `ModifyBalance`. Wire layouts follow `sdk/vaa/governance.go` and
//! `sdk/rust/vaas-serde` (`token::GovernancePacket`, `accountant::GovernancePacket`).

use bytemuck::{Pod, Zeroable};

use crate::constants::{
    ACCOUNTANT_GOVERNANCE_MODULE, GOVERNANCE_EMITTER, MODIFY_BALANCE_ACTION, REGISTER_CHAIN_ACTION,
    SOLANA_CHAIN_ID, TOKEN_BRIDGE_GOVERNANCE_MODULE, UPGRADE_CONTRACT_ACTION,
};
use crate::error::GlobalAccountantError;
use crate::primitives::Uint256;
use crate::state::ModificationKind;
use crate::vaa::VaaBodyHeader;

/// Reject a body whose emitter is not `(chain 1, GOVERNANCE_EMITTER)`.
///
/// SECURITY: every governance handler calls this before reading the payload.
pub fn require_governance_emitter(header: &VaaBodyHeader) -> Result<(), GlobalAccountantError> {
    if header.emitter_chain() != SOLANA_CHAIN_ID || header.emitter_address != GOVERNANCE_EMITTER {
        return Err(GlobalAccountantError::InvalidGovernanceEmitter);
    }
    Ok(())
}

/// Common governance prefix: `module ‖ action ‖ target_chain`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct GovernanceHeader {
    pub module: [u8; 32],
    pub action: u8,
    pub target_chain: [u8; 2],
}

impl GovernanceHeader {
    pub fn target_chain(&self) -> u16 {
        u16::from_be_bytes(self.target_chain)
    }

    /// Module and action must match; `target_chain` must be in `accepted_targets`.
    fn check(
        &self,
        module: &[u8; 32],
        action: u8,
        accepted_targets: &[u16],
    ) -> Result<(), GlobalAccountantError> {
        if self.module != *module {
            return Err(GlobalAccountantError::InvalidGovernanceModule);
        }
        if self.action != action {
            return Err(GlobalAccountantError::InvalidGovernanceAction);
        }
        if !accepted_targets.contains(&self.target_chain()) {
            return Err(GlobalAccountantError::GovernanceChainMismatch);
        }
        Ok(())
    }
}

/// Token Bridge `RegisterChain` payload (69 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct RegisterChainPayload {
    pub header: GovernanceHeader,
    pub chain: [u8; 2],
    pub emitter_address: [u8; 32],
}

impl RegisterChainPayload {
    pub const LEN: usize = core::mem::size_of::<Self>();

    /// Exact-length view of `payload`.
    ///
    /// SECURITY: precondition `payload.len() == 69`; anything else is
    /// `InvalidInstructionData`. Cannot panic.
    pub fn from_payload(payload: &[u8]) -> Result<&Self, GlobalAccountantError> {
        bytemuck::try_from_bytes(payload).map_err(|_| GlobalAccountantError::InvalidInstructionData)
    }

    /// Split a full VAA body into header view and payload view.
    pub fn from_body(body: &[u8]) -> Result<(&VaaBodyHeader, &Self), GlobalAccountantError> {
        let (header, payload) = VaaBodyHeader::split(body)?;
        Ok((header, Self::from_payload(payload)?))
    }

    pub fn chain(&self) -> u16 {
        u16::from_be_bytes(self.chain)
    }

    /// Governance emitter, `TokenBridge` module, `RegisterChain` action, target `Any` or Solana.
    pub fn validate(&self, header: &VaaBodyHeader) -> Result<(), GlobalAccountantError> {
        require_governance_emitter(header)?;
        self.header.check(
            &TOKEN_BRIDGE_GOVERNANCE_MODULE,
            REGISTER_CHAIN_ACTION,
            &[0, SOLANA_CHAIN_ID],
        )
    }
}

/// Accountant `ModifyBalance` payload (144 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct ModifyBalancePayload {
    pub header: GovernanceHeader,
    pub sequence: [u8; 8],
    pub chain_id: [u8; 2],
    pub token_chain: [u8; 2],
    pub token_address: [u8; 32],
    pub kind: u8,
    pub amount: [u8; 32],
    pub reason: [u8; 32],
}

impl ModifyBalancePayload {
    pub const LEN: usize = core::mem::size_of::<Self>();

    /// Exact-length view of `payload`.
    ///
    /// SECURITY: precondition `payload.len() == 144`; anything else is
    /// `InvalidInstructionData`. Cannot panic.
    pub fn from_payload(payload: &[u8]) -> Result<&Self, GlobalAccountantError> {
        bytemuck::try_from_bytes(payload).map_err(|_| GlobalAccountantError::InvalidInstructionData)
    }

    /// Split a full VAA body into header view and payload view.
    pub fn from_body(body: &[u8]) -> Result<(&VaaBodyHeader, &Self), GlobalAccountantError> {
        let (header, payload) = VaaBodyHeader::split(body)?;
        Ok((header, Self::from_payload(payload)?))
    }

    pub fn sequence(&self) -> u64 {
        u64::from_be_bytes(self.sequence)
    }

    pub fn chain_id(&self) -> u16 {
        u16::from_be_bytes(self.chain_id)
    }

    pub fn token_chain(&self) -> u16 {
        u16::from_be_bytes(self.token_chain)
    }

    pub fn amount(&self) -> Uint256 {
        Uint256::from_be_bytes(self.amount)
    }

    /// Governance emitter, `GlobalAccountant` module, `ModifyBalance` action, target Solana,
    /// known `kind`. Returns the parsed kind.
    pub fn validate(
        &self,
        header: &VaaBodyHeader,
    ) -> Result<ModificationKind, GlobalAccountantError> {
        require_governance_emitter(header)?;
        self.header.check(
            &ACCOUNTANT_GOVERNANCE_MODULE,
            MODIFY_BALANCE_ACTION,
            &[SOLANA_CHAIN_ID],
        )?;
        ModificationKind::from_u8(self.kind).ok_or(GlobalAccountantError::InvalidModificationKind)
    }
}

/// Accountant `UpgradeContract` payload (67 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct UpgradeContractPayload {
    pub header: GovernanceHeader,
    pub new_contract: [u8; 32],
}

impl UpgradeContractPayload {
    pub const LEN: usize = core::mem::size_of::<Self>();

    /// Exact-length view of `payload`.
    ///
    /// SECURITY: precondition `payload.len() == 67`; anything else is
    /// `InvalidInstructionData`. Cannot panic.
    pub fn from_payload(payload: &[u8]) -> Result<&Self, GlobalAccountantError> {
        bytemuck::try_from_bytes(payload).map_err(|_| GlobalAccountantError::InvalidInstructionData)
    }

    /// Split a full VAA body into header view and payload view.
    pub fn from_body(body: &[u8]) -> Result<(&VaaBodyHeader, &Self), GlobalAccountantError> {
        let (header, payload) = VaaBodyHeader::split(body)?;
        Ok((header, Self::from_payload(payload)?))
    }

    /// Governance emitter, `GlobalAccountant` module, `UpgradeContract` action, target Solana.
    pub fn validate(&self, header: &VaaBodyHeader) -> Result<(), GlobalAccountantError> {
        require_governance_emitter(header)?;
        self.header.check(
            &ACCOUNTANT_GOVERNANCE_MODULE,
            UPGRADE_CONTRACT_ACTION,
            &[SOLANA_CHAIN_ID],
        )
    }
}

const _: () = {
    use core::mem::{offset_of, size_of};
    assert!(size_of::<GovernanceHeader>() == 35);
    assert!(offset_of!(GovernanceHeader, action) == 32);
    assert!(offset_of!(GovernanceHeader, target_chain) == 33);

    assert!(RegisterChainPayload::LEN == 69);
    assert!(offset_of!(RegisterChainPayload, chain) == 35);
    assert!(offset_of!(RegisterChainPayload, emitter_address) == 37);

    assert!(UpgradeContractPayload::LEN == 67);
    assert!(offset_of!(UpgradeContractPayload, new_contract) == 35);

    assert!(ModifyBalancePayload::LEN == 144);
    assert!(offset_of!(ModifyBalancePayload, sequence) == 35);
    assert!(offset_of!(ModifyBalancePayload, chain_id) == 43);
    assert!(offset_of!(ModifyBalancePayload, token_chain) == 45);
    assert!(offset_of!(ModifyBalancePayload, token_address) == 47);
    assert!(offset_of!(ModifyBalancePayload, kind) == 79);
    assert!(offset_of!(ModifyBalancePayload, amount) == 80);
    assert!(offset_of!(ModifyBalancePayload, reason) == 112);
};

#[cfg(test)]
mod tests {
    use super::*;
    use wormhole_sdk::accountant_modification::ModificationKind as SdkKind;
    use wormhole_sdk::{accountant, token, Address, Amount, Chain};

    fn body_with(payload: &[u8]) -> std::vec::Vec<u8> {
        let header = VaaBodyHeader::new(0, 0, 1, [0x11; 32], 7, 0);
        let mut body = bytemuck::bytes_of(&header).to_vec();
        body.extend_from_slice(payload);
        body
    }

    fn governance_header(chain: u16, emitter: [u8; 32]) -> VaaBodyHeader {
        VaaBodyHeader::new(0, 0, chain, emitter, 0, 0)
    }

    fn register_chain_payload() -> RegisterChainPayload {
        let mut payload = RegisterChainPayload::zeroed();
        payload.header.module = TOKEN_BRIDGE_GOVERNANCE_MODULE;
        payload.header.action = REGISTER_CHAIN_ACTION;
        payload
    }

    fn upgrade_contract_payload() -> UpgradeContractPayload {
        let mut payload = UpgradeContractPayload::zeroed();
        payload.header.module = ACCOUNTANT_GOVERNANCE_MODULE;
        payload.header.action = UPGRADE_CONTRACT_ACTION;
        payload.header.target_chain = SOLANA_CHAIN_ID.to_be_bytes();
        payload
    }

    fn modify_balance_payload() -> ModifyBalancePayload {
        let mut payload = ModifyBalancePayload::zeroed();
        payload.header.module = ACCOUNTANT_GOVERNANCE_MODULE;
        payload.header.action = MODIFY_BALANCE_ACTION;
        payload.header.target_chain = SOLANA_CHAIN_ID.to_be_bytes();
        payload.kind = ModificationKind::Add as u8;
        payload
    }

    #[test]
    fn views_match_sdk_encoding() {
        let register = serde_wormhole::to_vec(&token::GovernancePacket {
            chain: Chain::Any,
            action: token::Action::RegisterChain {
                chain: Chain::Ethereum,
                emitter_address: Address([0xAB; 32]),
            },
        })
        .unwrap();
        assert_eq!(register.len(), RegisterChainPayload::LEN);
        let body = body_with(&register);
        let (header, view) = RegisterChainPayload::from_body(&body).unwrap();
        assert_eq!((header.emitter_chain(), header.sequence()), (1, 7));
        assert_eq!(view.header.module, TOKEN_BRIDGE_GOVERNANCE_MODULE);
        assert_eq!(view.header.action, REGISTER_CHAIN_ACTION);
        assert_eq!(view.header.target_chain(), 0);
        assert_eq!(view.chain(), u16::from(Chain::Ethereum));
        assert_eq!(view.emitter_address, [0xAB; 32]);

        let modify = serde_wormhole::to_vec(&accountant::GovernancePacket {
            chain: Chain::Solana,
            action: accountant::Action::ModifyBalance {
                sequence: 42,
                chain_id: 2,
                token_chain: 5,
                token_address: Address([0x22; 32]),
                kind: SdkKind::Subtract,
                amount: Amount([0x33; 32]),
                reason: "fix".into(),
            },
        })
        .unwrap();
        assert_eq!(modify.len(), ModifyBalancePayload::LEN);
        let body = body_with(&modify);
        let (_, view) = ModifyBalancePayload::from_body(&body).unwrap();
        assert_eq!(view.header.module, ACCOUNTANT_GOVERNANCE_MODULE);
        assert_eq!(view.header.action, MODIFY_BALANCE_ACTION);
        assert_eq!(view.header.target_chain(), 1);
        assert_eq!(view.sequence(), 42);
        assert_eq!(view.chain_id(), 2);
        assert_eq!(view.token_chain(), 5);
        assert_eq!(view.token_address, [0x22; 32]);
        assert_eq!(view.kind, SdkKind::Subtract as u8);
        assert_eq!(view.amount(), Uint256([0x33; 32]));
        assert_eq!(view.reason[..29], [0u8; 29]);
        assert_eq!(&view.reason[29..], b"fix");

        let upgrade = serde_wormhole::to_vec(&accountant::GovernancePacket {
            chain: Chain::Solana,
            action: accountant::Action::UpgradeContract {
                new_contract: Address([0xC4; 32]),
            },
        })
        .unwrap();
        assert_eq!(upgrade.len(), UpgradeContractPayload::LEN);
        let body = body_with(&upgrade);
        let (_, view) = UpgradeContractPayload::from_body(&body).unwrap();
        assert_eq!(view.header.module, ACCOUNTANT_GOVERNANCE_MODULE);
        assert_eq!(view.header.action, UPGRADE_CONTRACT_ACTION);
        assert_eq!(view.header.target_chain(), 1);
        assert_eq!(view.new_contract, [0xC4; 32]);
    }

    #[test]
    fn validate_table() {
        use GlobalAccountantError as E;
        let good = governance_header(SOLANA_CHAIN_ID, GOVERNANCE_EMITTER);
        let wrong_chain = governance_header(2, GOVERNANCE_EMITTER);
        let mut wrong_emitter = GOVERNANCE_EMITTER;
        wrong_emitter[0] ^= 1;
        let wrong_addr = governance_header(SOLANA_CHAIN_ID, wrong_emitter);

        let register = |f: fn(&mut RegisterChainPayload)| {
            let mut p = register_chain_payload();
            f(&mut p);
            p
        };
        let register_cases: [(&str, VaaBodyHeader, RegisterChainPayload, Result<(), E>); 7] = [
            ("register any target", good, register(|_| {}), Ok(())),
            (
                "register solana target",
                good,
                register(|p| p.header.target_chain = SOLANA_CHAIN_ID.to_be_bytes()),
                Ok(()),
            ),
            (
                "register wormchain target",
                good,
                register(|p| p.header.target_chain = 3104u16.to_be_bytes()),
                Err(E::GovernanceChainMismatch),
            ),
            (
                "register wrong emitter chain",
                wrong_chain,
                register(|_| {}),
                Err(E::InvalidGovernanceEmitter),
            ),
            (
                "register wrong emitter address",
                wrong_addr,
                register(|_| {}),
                Err(E::InvalidGovernanceEmitter),
            ),
            (
                "register wrong module",
                good,
                register(|p| p.header.module[31] ^= 1),
                Err(E::InvalidGovernanceModule),
            ),
            (
                "register wrong action",
                good,
                register(|p| p.header.action = 2),
                Err(E::InvalidGovernanceAction),
            ),
        ];
        for (name, header, payload, expected) in register_cases {
            assert_eq!(payload.validate(&header), expected, "{name}");
        }

        let modify = |f: fn(&mut ModifyBalancePayload)| {
            let mut p = modify_balance_payload();
            f(&mut p);
            p
        };
        let modify_cases: [(
            &str,
            VaaBodyHeader,
            ModifyBalancePayload,
            Result<ModificationKind, E>,
        ); 8] = [
            (
                "modify add",
                good,
                modify(|_| {}),
                Ok(ModificationKind::Add),
            ),
            (
                "modify subtract",
                good,
                modify(|p| p.kind = ModificationKind::Subtract as u8),
                Ok(ModificationKind::Subtract),
            ),
            (
                "modify kind 0",
                good,
                modify(|p| p.kind = 0),
                Err(E::InvalidModificationKind),
            ),
            (
                "modify kind 3",
                good,
                modify(|p| p.kind = 3),
                Err(E::InvalidModificationKind),
            ),
            (
                "modify any target",
                good,
                modify(|p| p.header.target_chain = [0; 2]),
                Err(E::GovernanceChainMismatch),
            ),
            (
                "modify wrong emitter chain",
                wrong_chain,
                modify(|_| {}),
                Err(E::InvalidGovernanceEmitter),
            ),
            (
                "modify token bridge module",
                good,
                modify(|p| p.header.module = TOKEN_BRIDGE_GOVERNANCE_MODULE),
                Err(E::InvalidGovernanceModule),
            ),
            (
                "modify wrong action",
                good,
                modify(|p| p.header.action = 2),
                Err(E::InvalidGovernanceAction),
            ),
        ];
        for (name, header, payload, expected) in modify_cases {
            assert_eq!(payload.validate(&header), expected, "{name}");
        }

        let upgrade = |f: fn(&mut UpgradeContractPayload)| {
            let mut p = upgrade_contract_payload();
            f(&mut p);
            p
        };
        let upgrade_cases: [(&str, VaaBodyHeader, UpgradeContractPayload, Result<(), E>); 5] = [
            ("upgrade solana target", good, upgrade(|_| {}), Ok(())),
            (
                "upgrade any target",
                good,
                upgrade(|p| p.header.target_chain = [0; 2]),
                Err(E::GovernanceChainMismatch),
            ),
            (
                "upgrade wrong module",
                good,
                upgrade(|p| p.header.module = TOKEN_BRIDGE_GOVERNANCE_MODULE),
                Err(E::InvalidGovernanceModule),
            ),
            (
                "upgrade modify_balance action",
                good,
                upgrade(|p| p.header.action = MODIFY_BALANCE_ACTION),
                Err(E::InvalidGovernanceAction),
            ),
            (
                "upgrade wrong emitter chain",
                wrong_chain,
                upgrade(|_| {}),
                Err(E::InvalidGovernanceEmitter),
            ),
        ];
        for (name, header, payload, expected) in upgrade_cases {
            assert_eq!(payload.validate(&header), expected, "{name}");
        }
    }

    #[test]
    fn upgrade_contract_view_matches_wire() {
        let mut wire = std::vec::Vec::new();
        wire.extend_from_slice(&ACCOUNTANT_GOVERNANCE_MODULE);
        wire.push(0x02);
        wire.extend_from_slice(&1u16.to_be_bytes());
        wire.extend_from_slice(&[0xC4; 32]);
        assert_eq!(wire.len(), UpgradeContractPayload::LEN);
        let body = body_with(&wire);
        let (header, view) = UpgradeContractPayload::from_body(&body).unwrap();
        assert_eq!((header.emitter_chain(), header.sequence()), (1, 7));
        assert_eq!(view.header.module, ACCOUNTANT_GOVERNANCE_MODULE);
        assert_eq!(view.header.action, UPGRADE_CONTRACT_ACTION);
        assert_eq!(view.header.target_chain(), SOLANA_CHAIN_ID);
        assert_eq!(view.new_contract, [0xC4; 32]);
        assert_eq!(
            view.validate(header),
            Err(GlobalAccountantError::InvalidGovernanceEmitter)
        );
        let governed = VaaBodyHeader::new(0, 0, SOLANA_CHAIN_ID, GOVERNANCE_EMITTER, 7, 0);
        assert_eq!(view.validate(&governed), Ok(()));
    }

    #[test]
    fn payload_length_is_exact() {
        let cases: [(&str, usize, bool); 9] = [
            ("register -1", RegisterChainPayload::LEN - 1, false),
            ("register ==", RegisterChainPayload::LEN, true),
            ("register +1", RegisterChainPayload::LEN + 1, false),
            ("modify -1", ModifyBalancePayload::LEN - 1, false),
            ("modify ==", ModifyBalancePayload::LEN, true),
            ("modify +1", ModifyBalancePayload::LEN + 1, false),
            ("upgrade -1", UpgradeContractPayload::LEN - 1, false),
            ("upgrade ==", UpgradeContractPayload::LEN, true),
            ("upgrade +1", UpgradeContractPayload::LEN + 1, false),
        ];
        for (name, len, ok) in cases {
            let buf = std::vec![0u8; len];
            let got = if name.starts_with("register") {
                RegisterChainPayload::from_payload(&buf).is_ok()
            } else if name.starts_with("modify") {
                ModifyBalancePayload::from_payload(&buf).is_ok()
            } else {
                UpgradeContractPayload::from_payload(&buf).is_ok()
            };
            assert_eq!(got, ok, "{name}");
        }
    }
}
