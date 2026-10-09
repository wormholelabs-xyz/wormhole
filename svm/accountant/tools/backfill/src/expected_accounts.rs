//! The on-chain state each snapshot row maps to: the definitions layout the
//! backfill handler writes, and the PDA its key derives. ga-backfill's
//! verification and ga-parity's expected state both read from here.

use bytemuck::Pod;
use solana_pubkey::Pubkey;

use global_accountant_definitions::{
    BalanceAccountKey, BalanceAccountLayout, BelongsToHub, ChainRegistrationKey,
    ChainRegistrationLayout, ModifyBalanceKey, ModifyBalanceLayout, PdaSeeds, RegisterChainKey,
    RegisterChainLayout, TransceiverHubKey, TransceiverHubLayout, TransceiverPeerKey,
    TransceiverPeerLayout, Uint256,
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

macro_rules! keyed {
    ($($layout:ty => $key:ty),* $(,)?) => {$(
        impl PdaAccount for $layout {
            type Key = $key;
            fn pda_key(&self) -> $key {
                self.key()
            }
        }
    )*};
}

keyed!(
    BalanceAccountLayout => BalanceAccountKey,
    ModifyBalanceLayout => ModifyBalanceKey,
    ChainRegistrationLayout => ChainRegistrationKey,
    RegisterChainLayout => RegisterChainKey,
    TransceiverHubLayout => TransceiverHubKey,
    TransceiverPeerLayout => TransceiverPeerKey,
);

pub fn balance_account(a: &BalanceRow) -> BalanceAccountLayout {
    BalanceAccountLayout::new(
        a.chain,
        a.token_chain,
        a.token_address,
        Uint256::from_be_bytes(a.balance),
    )
}

pub fn modify_balance_account(m: &ModifyBalanceRow) -> ModifyBalanceLayout {
    ModifyBalanceLayout::new(
        m.modify_kind,
        m.chain_id,
        m.token_chain,
        m.sequence,
        m.token_address,
        Uint256::from_be_bytes(m.amount),
        m.reason,
    )
}

pub fn chain_registration_account(r: &ChainRegistrationRow) -> ChainRegistrationLayout {
    ChainRegistrationLayout::new(r.chain, r.registered_emitter, r.sequence)
}

pub fn register_chain_account(r: &ChainRegistrationRow) -> RegisterChainLayout {
    RegisterChainLayout::new(r.chain, r.registered_emitter, r.sequence)
}

pub fn transceiver_hub_account(h: &TransceiverHubRow) -> TransceiverHubLayout {
    TransceiverHubLayout::new(
        TransceiverHubKey::new(h.chain, h.address),
        BelongsToHub(TransceiverHubKey::new(h.hub_chain, h.hub_address)),
    )
}

pub fn transceiver_peer_account(p: &TransceiverPeerRow) -> TransceiverPeerLayout {
    TransceiverPeerLayout::new(
        TransceiverPeerKey::new(p.chain, p.address, p.dest_chain),
        p.peer_address,
    )
}
