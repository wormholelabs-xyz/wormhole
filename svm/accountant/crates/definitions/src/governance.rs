//! Governance VAA payload views: Token Bridge `RegisterChain` and accountant
//! `ModifyBalance`. Wire layouts follow `sdk/vaa/governance.go` and
//! `sdk/rust/vaas-serde` (`token::GovernancePacket`, `accountant::GovernancePacket`).

use bytemuck::{Pod, Zeroable};

use crate::constants::{
    ACCOUNTANT_GOVERNANCE_MODULE, GOVERNANCE_EMITTER, MODIFY_BALANCE_ACTION, REGISTER_CHAIN_ACTION,
    SOLANA_CHAIN_ID, TOKEN_BRIDGE_GOVERNANCE_MODULE,
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

const _: () = {
    use core::mem::{offset_of, size_of};
    assert!(size_of::<GovernanceHeader>() == 35);
    assert!(offset_of!(GovernanceHeader, action) == 32);
    assert!(offset_of!(GovernanceHeader, target_chain) == 33);

    assert!(RegisterChainPayload::LEN == 69);
    assert!(offset_of!(RegisterChainPayload, chain) == 35);
    assert!(offset_of!(RegisterChainPayload, emitter_address) == 37);

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
        let mut body = std::vec![0u8; VaaBodyHeader::LEN];
        body[8..10].copy_from_slice(&1u16.to_be_bytes());
        body[10..42].copy_from_slice(&[0x11; 32]);
        body[42..50].copy_from_slice(&7u64.to_be_bytes());
        body.extend_from_slice(payload);
        body
    }

    /// Encode with the guardian SDK, view with ours, compare every field.
    #[test]
    fn register_chain_view_matches_sdk_encoding() {
        let packet = token::GovernancePacket {
            chain: Chain::Any,
            action: token::Action::RegisterChain {
                chain: Chain::Ethereum,
                emitter_address: Address([0xAB; 32]),
            },
        };
        let bytes = serde_wormhole::to_vec(&packet).expect("encode");
        assert_eq!(bytes.len(), RegisterChainPayload::LEN);

        let body = body_with(&bytes);
        let (header, view) = RegisterChainPayload::from_body(&body).expect("view");
        assert_eq!(header.emitter_chain(), 1);
        assert_eq!(header.sequence(), 7);
        assert_eq!(view.header.module, TOKEN_BRIDGE_GOVERNANCE_MODULE);
        assert_eq!(view.header.action, REGISTER_CHAIN_ACTION);
        assert_eq!(view.header.target_chain(), 0);
        assert_eq!(view.chain(), u16::from(Chain::Ethereum));
        assert_eq!(view.emitter_address, [0xAB; 32]);
    }

    #[test]
    fn modify_balance_view_matches_sdk_encoding() {
        let packet = accountant::GovernancePacket {
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
        };
        let bytes = serde_wormhole::to_vec(&packet).expect("encode");
        assert_eq!(bytes.len(), ModifyBalancePayload::LEN);

        let body = body_with(&bytes);
        let (_, view) = ModifyBalancePayload::from_body(&body).expect("view");
        assert_eq!(view.header.module, ACCOUNTANT_GOVERNANCE_MODULE);
        assert_eq!(view.header.action, MODIFY_BALANCE_ACTION);
        assert_eq!(view.header.target_chain(), 1);
        assert_eq!(view.sequence(), 42);
        assert_eq!(view.chain_id(), 2);
        assert_eq!(view.token_chain(), 5);
        assert_eq!(view.token_address, [0x22; 32]);
        assert_eq!(view.kind, SdkKind::Subtract as u8);
        assert_eq!(view.amount(), Uint256([0x33; 32]));
        // `arraystring`: right-aligned, zero-padded on the left.
        assert_eq!(view.reason[..29], [0u8; 29]);
        assert_eq!(&view.reason[29..], b"fix");
    }

    fn governance_header(chain: u16, emitter: [u8; 32]) -> VaaBodyHeader {
        let mut header = VaaBodyHeader::zeroed();
        header.emitter_chain = chain.to_be_bytes();
        header.emitter_address = emitter;
        header
    }

    fn register_chain_payload() -> RegisterChainPayload {
        let mut payload = RegisterChainPayload::zeroed();
        payload.header.module = TOKEN_BRIDGE_GOVERNANCE_MODULE;
        payload.header.action = REGISTER_CHAIN_ACTION;
        payload.header.target_chain = 0u16.to_be_bytes();
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
    fn register_chain_validate_table() {
        use GlobalAccountantError as E;
        let good = governance_header(SOLANA_CHAIN_ID, GOVERNANCE_EMITTER);
        let mut wrong_chain = good;
        wrong_chain.emitter_chain = 2u16.to_be_bytes();
        let mut wrong_addr = good;
        wrong_addr.emitter_address[0] ^= 1;

        let mut wrong_module = register_chain_payload();
        wrong_module.header.module[31] ^= 1;
        let mut wrong_action = register_chain_payload();
        wrong_action.header.action = 2;
        let mut target_solana = register_chain_payload();
        target_solana.header.target_chain = SOLANA_CHAIN_ID.to_be_bytes();
        let mut target_wormchain = register_chain_payload();
        target_wormchain.header.target_chain = 3104u16.to_be_bytes();

        let cases: [(&str, VaaBodyHeader, RegisterChainPayload, Result<(), E>); 7] = [
            ("any target", good, register_chain_payload(), Ok(())),
            ("solana target", good, target_solana, Ok(())),
            (
                "wormchain target",
                good,
                target_wormchain,
                Err(E::GovernanceChainMismatch),
            ),
            (
                "wrong emitter chain",
                wrong_chain,
                register_chain_payload(),
                Err(E::InvalidGovernanceEmitter),
            ),
            (
                "wrong emitter address",
                wrong_addr,
                register_chain_payload(),
                Err(E::InvalidGovernanceEmitter),
            ),
            (
                "wrong module",
                good,
                wrong_module,
                Err(E::InvalidGovernanceModule),
            ),
            (
                "wrong action",
                good,
                wrong_action,
                Err(E::InvalidGovernanceAction),
            ),
        ];
        for (name, header, payload, expected) in cases {
            assert_eq!(payload.validate(&header), expected, "{name}");
        }
    }

    #[test]
    fn modify_balance_validate_table() {
        use GlobalAccountantError as E;
        let good = governance_header(SOLANA_CHAIN_ID, GOVERNANCE_EMITTER);
        let mut wrong_chain = good;
        wrong_chain.emitter_chain = 2u16.to_be_bytes();

        let mut subtract = modify_balance_payload();
        subtract.kind = ModificationKind::Subtract as u8;
        let mut kind_zero = modify_balance_payload();
        kind_zero.kind = 0;
        let mut kind_three = modify_balance_payload();
        kind_three.kind = 3;
        let mut target_any = modify_balance_payload();
        target_any.header.target_chain = 0u16.to_be_bytes();
        let mut wrong_module = modify_balance_payload();
        wrong_module.header.module = TOKEN_BRIDGE_GOVERNANCE_MODULE;
        let mut wrong_action = modify_balance_payload();
        wrong_action.header.action = 2;

        let cases: [(
            &str,
            VaaBodyHeader,
            ModifyBalancePayload,
            Result<ModificationKind, E>,
        ); 8] = [
            (
                "add",
                good,
                modify_balance_payload(),
                Ok(ModificationKind::Add),
            ),
            ("subtract", good, subtract, Ok(ModificationKind::Subtract)),
            ("kind 0", good, kind_zero, Err(E::InvalidModificationKind)),
            ("kind 3", good, kind_three, Err(E::InvalidModificationKind)),
            (
                "any target rejected",
                good,
                target_any,
                Err(E::GovernanceChainMismatch),
            ),
            (
                "wrong emitter chain",
                wrong_chain,
                modify_balance_payload(),
                Err(E::InvalidGovernanceEmitter),
            ),
            (
                "token bridge module",
                good,
                wrong_module,
                Err(E::InvalidGovernanceModule),
            ),
            (
                "wrong action",
                good,
                wrong_action,
                Err(E::InvalidGovernanceAction),
            ),
        ];
        for (name, header, payload, expected) in cases {
            assert_eq!(payload.validate(&header), expected, "{name}");
        }
    }

    /// Exact length: one byte short or long is rejected.
    #[test]
    fn payload_length_is_exact() {
        let cases: [(&str, usize, bool); 6] = [
            ("register -1", RegisterChainPayload::LEN - 1, false),
            ("register ==", RegisterChainPayload::LEN, true),
            ("register +1", RegisterChainPayload::LEN + 1, false),
            ("modify -1", ModifyBalancePayload::LEN - 1, false),
            ("modify ==", ModifyBalancePayload::LEN, true),
            ("modify +1", ModifyBalancePayload::LEN + 1, false),
        ];
        for (name, len, ok) in cases {
            let buf = std::vec![0u8; len];
            let got = if name.starts_with("register") {
                RegisterChainPayload::from_payload(&buf).is_ok()
            } else {
                ModifyBalancePayload::from_payload(&buf).is_ok()
            };
            assert_eq!(got, ok, "{name}");
        }
    }
}
