//! Governance VAA payload views: Token Bridge `RegisterChain` and accountant
//! `ModifyBalance`. Wire layouts follow `sdk/vaa/governance.go` and
//! `sdk/rust/vaas-serde` (`token::GovernancePacket`, `accountant::GovernancePacket`).

use bytemuck::{Pod, Zeroable};

use crate::error::GlobalAccountantError;
use crate::primitives::Uint256;
use crate::vaa::VaaBodyHeader;

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
    use crate::constants::{
        ACCOUNTANT_GOVERNANCE_MODULE, MODIFY_BALANCE_ACTION, REGISTER_CHAIN_ACTION,
        TOKEN_BRIDGE_GOVERNANCE_MODULE,
    };
    use wormhole_sdk::accountant_modification::ModificationKind;
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
                kind: ModificationKind::Subtract,
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
        assert_eq!(view.kind, ModificationKind::Subtract as u8);
        assert_eq!(view.amount(), Uint256([0x33; 32]));
        // `arraystring`: right-aligned, zero-padded on the left.
        assert_eq!(view.reason[..29], [0u8; 29]);
        assert_eq!(&view.reason[29..], b"fix");
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
