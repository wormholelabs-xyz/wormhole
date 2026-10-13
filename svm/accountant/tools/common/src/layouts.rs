//! The on-chain state each snapshot row maps to: the definitions layout the
//! backfill handler writes, and the PDA its key derives. svm-accountant-backfill's
//! verification and the parity expected state both read from here.

use bytemuck::Pod;
use solana_pubkey::Pubkey;

use global_accountant_definitions::{
    BalanceAccountKey, BalanceAccountLayout, BelongsToHub, ChainRegistrationKey,
    ChainRegistrationLayout, ModifyBalanceKey, ModifyBalanceLayout, PdaSeeds, RegisterChainKey,
    RegisterChainLayout, TransceiverHubKey, TransceiverHubLayout, TransceiverPeerKey,
    TransceiverPeerLayout,
};

use crate::snapshot::{
    BalanceRow, ChainRegistrationRow, ModifyBalanceRow, TransceiverHubRow, TransceiverPeerRow,
};

/// Address of a program-owned PDA from its definitions key.
pub fn derive_pda(program_id: &Pubkey, key: &impl PdaSeeds) -> Pubkey {
    Pubkey::find_program_address(key.seeds().as_slice(), program_id).0
}

/// Layouts with a definitions PDA key.
pub trait PdaAccount: Pod {
    type Key: PdaSeeds;
    fn pda_key(&self) -> Self::Key;

    /// The PDA this layout lives at under `program_id`.
    fn address(&self, program_id: &Pubkey) -> Pubkey {
        derive_pda(program_id, &self.pda_key())
    }
}

macro_rules! impl_pda_account {
    ($($layout:ty => $key:ty),* $(,)?) => {$(
        impl PdaAccount for $layout {
            type Key = $key;
            fn pda_key(&self) -> $key {
                self.key()
            }
        }
    )*};
}

impl_pda_account!(
    BalanceAccountLayout => BalanceAccountKey,
    ModifyBalanceLayout => ModifyBalanceKey,
    ChainRegistrationLayout => ChainRegistrationKey,
    RegisterChainLayout => RegisterChainKey,
    TransceiverHubLayout => TransceiverHubKey,
    TransceiverPeerLayout => TransceiverPeerKey,
);

pub fn balance_layout(a: &BalanceRow) -> BalanceAccountLayout {
    BalanceAccountLayout::new(a.chain.0, a.token_chain.0, a.token_address.0, a.balance)
}

pub fn modify_balance_layout(m: &ModifyBalanceRow) -> ModifyBalanceLayout {
    ModifyBalanceLayout::new(
        m.modify_kind,
        m.chain_id.0,
        m.token_chain.0,
        m.sequence.0,
        m.token_address.0,
        m.amount,
        m.reason,
    )
}

pub fn chain_registration_layout(r: &ChainRegistrationRow) -> ChainRegistrationLayout {
    ChainRegistrationLayout::new(r.chain.0, r.registered_emitter.0, r.sequence.0)
}

pub fn register_chain_layout(r: &ChainRegistrationRow) -> RegisterChainLayout {
    RegisterChainLayout::new(r.chain.0, r.registered_emitter.0, r.sequence.0)
}

pub fn transceiver_hub_layout(h: &TransceiverHubRow) -> TransceiverHubLayout {
    TransceiverHubLayout::new(
        TransceiverHubKey::new(h.chain.0, h.address.0),
        BelongsToHub(TransceiverHubKey::new(h.hub_chain.0, h.hub_address.0)),
    )
}

pub fn transceiver_peer_layout(p: &TransceiverPeerRow) -> TransceiverPeerLayout {
    TransceiverPeerLayout::new(
        TransceiverPeerKey::new(p.chain.0, p.address.0, p.dest_chain.0),
        p.peer_address.0,
    )
}
