use global_accountant_definitions::{
    TransceiverHubKey, TransceiverHubLayout, TransceiverPeerKey, TransceiverPeerLayout, Uint256,
};
use solana_account::Account;
use solana_pubkey::Pubkey;

use super::ids::program_id;
use super::*;

pub fn hub_layout(
    chain: u16,
    address: [u8; 32],
    hub_chain: u16,
    hub: [u8; 32],
) -> TransceiverHubLayout {
    TransceiverHubLayout::new(
        TransceiverHubKey::new(chain, address),
        TransceiverHubKey::new(hub_chain, hub),
    )
}

pub fn peer_layout(
    chain: u16,
    address: [u8; 32],
    dest_chain: u16,
    peer: [u8; 32],
) -> TransceiverPeerLayout {
    TransceiverPeerLayout::new(TransceiverPeerKey::new(chain, address, dest_chain), peer)
}

pub fn hub_account(layout: &TransceiverHubLayout) -> Account {
    Account {
        lamports: 1_000_000,
        data: bytemuck::bytes_of(layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

pub fn peer_account(layout: &TransceiverPeerLayout) -> Account {
    Account {
        lamports: 1_000_000,
        data: bytemuck::bytes_of(layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

/// `BalanceAccount` for `chain` holding `balance` of the hub token `(token_chain, token)`.
pub fn balance_account(chain: u16, token_chain: u16, token: [u8; 32], balance: Uint256) -> Account {
    balance_account_for(&program_id(), chain, token_chain, token, balance)
}

pub fn assert_balance(accounts: &[(Pubkey, Account)], key: &Pubkey, expected: Uint256) {
    assert_balance_for(&program_id(), accounts, key, expected)
}
