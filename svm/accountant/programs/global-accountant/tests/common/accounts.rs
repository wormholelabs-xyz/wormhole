use global_accountant_definitions::{
    BalanceAccountLayout, ChainRegistrationLayout, NoReplayBitmapAccount, Uint256,
    NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITS_PER_BUCKET,
};
use solana_account::Account;
use solana_pubkey::Pubkey;

use super::ids::{noreplay_program_id, program_id, system_program_id};
use super::mollusk::system_owned_account;

pub fn balance_account(
    chain: u16,
    token_chain: u16,
    token_address: [u8; 32],
    balance: Uint256,
) -> Account {
    let layout = BalanceAccountLayout::new(chain, token_chain, token_address, balance);
    Account {
        lamports: 1_500_000_000,
        data: bytemuck::bytes_of(&layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

pub fn chain_registration_account(chain: u16, emitter_address: [u8; 32]) -> Account {
    let layout = ChainRegistrationLayout::new(chain, emitter_address);
    Account {
        lamports: 1_000_000,
        data: bytemuck::bytes_of(&layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

pub fn noreplay_bucket_unmarked() -> Account {
    system_owned_account(0)
}

pub fn noreplay_bucket_marked(sequence: u64) -> Account {
    let mut account: NoReplayBitmapAccount = bytemuck::Zeroable::zeroed();
    let bit_index = (sequence % NOREPLAY_BITS_PER_BUCKET) as usize;
    account.bitmap[bit_index / 8] |= 1u8 << (bit_index % 8);
    Account {
        lamports: 1_500_000_000,
        data: bytemuck::bytes_of(&account).to_vec(),
        owner: noreplay_program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

pub fn assert_bucket_marked(bucket: &Account, sequence: u64) {
    assert_eq!(bucket.owner, noreplay_program_id(), "bucket owner");
    let account = NoReplayBitmapAccount::from_bytes(&bucket.data).expect("bucket layout");
    assert!(account.is_marked(sequence), "bit {sequence} set");
}

pub fn assert_bucket_unmarked(bucket: &Account) {
    assert_eq!(bucket.owner, system_program_id(), "bucket owner");
    assert!(bucket.data.is_empty(), "bucket uninitialised");
}

pub fn balance_of(account: &Account) -> Uint256 {
    let layout: &BalanceAccountLayout = bytemuck::from_bytes(&account.data);
    layout.balance
}

pub fn assert_balance(accounts: &[(Pubkey, Account)], key: &Pubkey, expected: Uint256) {
    let account = super::mollusk::find_account(accounts, key);
    assert_eq!(account.owner, program_id(), "balance PDA owner {key}");
    assert_eq!(balance_of(account), expected, "balance {key}");
}

/// The accountant's NoReplay authority PDA for `program_id`.
pub fn noreplay_authority_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], program_id).0
}
