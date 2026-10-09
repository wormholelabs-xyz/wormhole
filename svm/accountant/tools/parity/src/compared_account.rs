//! Per-account-type facts the comparison needs: the compared state an account belongs to,
//! its match key and compared bytes, and its re-encode check.

use bytemuck::Pod;
use ga_backfill::expected_accounts::PdaAccount;
use global_accountant_definitions::{
    AccountLayout, AccountTag, BalanceAccountLayout, ChainRegistrationLayout, ModificationKind,
    ModifyBalanceLayout, PdaSeeds, RegisterChainLayout, TransceiverHubLayout,
    TransceiverPeerLayout,
};

use crate::state::ComparedState;

/// A program-owned account type that parity compares, row by row, on one compared state.
pub trait ComparedAccount: PdaAccount + AccountLayout + Pod {
    const STATE: ComparedState;

    /// `Layout::new(..)` from this layout's own fields. `None` on a field no
    /// constructor accepts.
    fn reencode(&self) -> Option<Self>;

    /// Stored value: the full account bytes.
    fn compared_bytes(&self) -> Vec<u8> {
        bytemuck::bytes_of(self).to_vec()
    }

    /// Canonical key: the PDA seeds after the prefix, big-endian, so byte
    /// order equals numeric tuple order.
    fn match_key(&self) -> Vec<u8> {
        self.pda_key().seeds().as_slice()[1..].concat()
    }
}

impl ComparedAccount for BalanceAccountLayout {
    const STATE: ComparedState = ComparedState::Balances;

    fn reencode(&self) -> Option<Self> {
        Some(Self::new(
            self.chain,
            self.token_chain,
            self.token_address,
            self.balance,
        ))
    }

    /// The 32-byte big-endian balance.
    fn compared_bytes(&self) -> Vec<u8> {
        self.balance.0.to_vec()
    }
}

impl ComparedAccount for ModifyBalanceLayout {
    const STATE: ComparedState = ComparedState::ModifyBalanceRecords;

    fn reencode(&self) -> Option<Self> {
        let kind = ModificationKind::from_u8(self.kind)?;
        Some(Self::new(
            kind,
            self.chain_id,
            self.token_chain,
            self.sequence,
            self.token_address,
            self.amount,
            self.reason,
        ))
    }
}

impl ComparedAccount for ChainRegistrationLayout {
    const STATE: ComparedState = ComparedState::ChainRegistrations;

    fn reencode(&self) -> Option<Self> {
        Some(Self::new(
            self.chain,
            self.emitter_address,
            self.governance_sequence(),
        ))
    }
}

impl ComparedAccount for RegisterChainLayout {
    const STATE: ComparedState = ComparedState::RegisterChainRecords;

    fn reencode(&self) -> Option<Self> {
        Some(Self::new(self.chain, self.emitter_address, self.sequence))
    }
}

impl ComparedAccount for TransceiverHubLayout {
    const STATE: ComparedState = ComparedState::TransceiverHubs;

    fn reencode(&self) -> Option<Self> {
        Some(Self::new(self.key(), self.belongs_to_hub()))
    }
}

impl ComparedAccount for TransceiverPeerLayout {
    const STATE: ComparedState = ComparedState::TransceiverPeers;

    fn reencode(&self) -> Option<Self> {
        Some(Self::new(self.key(), self.peer_address))
    }
}

/// Every account tag, for decoding the tag byte.
const TAGS: [AccountTag; 7] = [
    AccountTag::PendingObservations,
    AccountTag::Balance,
    AccountTag::ChainRegistration,
    AccountTag::ModifyBalance,
    AccountTag::RegisterChain,
    AccountTag::TransceiverHub,
    AccountTag::TransceiverPeer,
];

pub fn account_tag(byte: u8) -> Option<AccountTag> {
    TAGS.into_iter().find(|tag| *tag as u8 == byte)
}
