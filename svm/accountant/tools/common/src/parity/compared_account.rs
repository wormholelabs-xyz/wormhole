//! Per-account-type facts the comparison needs: the keyed state an account belongs to,
//! its match key and compared bytes, and its re-encode check.

use bytemuck::Pod;
use global_accountant_definitions::{
    AccountLayout, BalanceAccountLayout, ChainRegistrationLayout, ModificationKind,
    ModifyBalanceLayout, PdaSeeds, RegisterChainLayout, TransceiverHubLayout,
    TransceiverPeerLayout,
};

use crate::layouts::PdaAccount;
use crate::parity::state::KeyedKind;

/// A program-owned account type that parity compares, row by row, on one keyed state.
pub trait ComparedAccount: PdaAccount + AccountLayout + Pod {
    const KIND: KeyedKind;

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
    const KIND: KeyedKind = KeyedKind::Balances;

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
    const KIND: KeyedKind = KeyedKind::ModifyBalanceRecords;

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
    const KIND: KeyedKind = KeyedKind::ChainRegistrations;

    fn reencode(&self) -> Option<Self> {
        Some(Self::new(
            self.chain,
            self.emitter_address,
            self.governance_sequence(),
        ))
    }
}

impl ComparedAccount for RegisterChainLayout {
    const KIND: KeyedKind = KeyedKind::RegisterChainRecords;

    fn reencode(&self) -> Option<Self> {
        Some(Self::new(self.chain, self.emitter_address, self.sequence))
    }
}

impl ComparedAccount for TransceiverHubLayout {
    const KIND: KeyedKind = KeyedKind::TransceiverHubs;

    fn reencode(&self) -> Option<Self> {
        Some(Self::new(self.key(), self.belongs_to_hub()))
    }
}

impl ComparedAccount for TransceiverPeerLayout {
    const KIND: KeyedKind = KeyedKind::TransceiverPeers;

    fn reencode(&self) -> Option<Self> {
        Some(Self::new(self.key(), self.peer_address))
    }
}
