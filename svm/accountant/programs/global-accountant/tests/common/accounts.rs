use accountant_test_harness::{
    assert_balance_for, balance_account_for, chain_registration_account_for,
};
use global_accountant_definitions::Uint256;
use solana_account::Account;
use solana_pubkey::Pubkey;

use super::ids::program_id;

pub fn balance_account(
    chain: u16,
    token_chain: u16,
    token_address: [u8; 32],
    balance: Uint256,
) -> Account {
    balance_account_for(&program_id(), chain, token_chain, token_address, balance)
}

/// Registration as written by governance sequence 0; any other sequence overwrites it.
pub fn chain_registration_account(chain: u16, emitter_address: [u8; 32]) -> Account {
    chain_registration_account_for(&program_id(), chain, emitter_address)
}

pub fn assert_balance(accounts: &[(Pubkey, Account)], key: &Pubkey, expected: Uint256) {
    assert_balance_for(&program_id(), accounts, key, expected)
}
