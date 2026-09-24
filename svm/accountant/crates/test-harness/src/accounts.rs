//! Account data builders and assertions for program-owned and NoReplay PDAs.

use global_accountant_definitions::{
    BalanceAccountLayout, ChainRegistrationLayout, NoReplayBitmapAccount,
    PendingObservationsLayout, Uint256, NOREPLAY_BITS_PER_BUCKET,
};
use solana_account::Account;
use solana_loader_v3_interface::state::UpgradeableLoaderState;
use solana_pubkey::Pubkey;

use crate::ids::{loader_v3_id, noreplay_program_id, system_program_id};
use crate::mollusk::{find_account, system_owned_account};

/// `<SBF_OUT_DIR>/<program_name>.so`, as `just build` wrote it.
pub fn deployed_elf(program_name: &str) -> Vec<u8> {
    let dir = std::env::var("SBF_OUT_DIR").expect("SBF_OUT_DIR");
    std::fs::read(format!("{dir}/{program_name}.so")).expect("built program .so")
}

/// Loader header of a program, buffer, or program-data account. `bincode` stops at the
/// header and skips the trailing ELF bytes.
pub fn loader_state(account: &Account) -> UpgradeableLoaderState {
    bincode::deserialize(&account.data).expect("upgradeable loader state")
}

/// Serialized `header ‖ payload`, the layout of buffer and program-data accounts.
pub fn loader_account_data(header: &UpgradeableLoaderState, payload: &[u8]) -> Vec<u8> {
    let mut data = bincode::serialize(header).expect("serialize loader state");
    data.extend_from_slice(payload);
    data
}

fn loader_owned(data: Vec<u8>, lamports: u64, executable: bool) -> Account {
    Account {
        lamports,
        data,
        owner: loader_v3_id(),
        executable,
        rent_epoch: 0,
    }
}

/// `Buffer` holding `elf`, with `authority` as its upgrade authority.
pub fn upgradeable_buffer_account(authority: &Pubkey, elf: &[u8]) -> Account {
    let header = UpgradeableLoaderState::Buffer {
        authority_address: Some(*authority),
    };
    loader_owned(loader_account_data(&header, elf), 10_000_000_000, false)
}

/// `ProgramData` last upgraded at slot 1, sized for `elf_len` bytes plus 1024 spare.
pub fn upgradeable_program_data_account(authority: &Pubkey, elf_len: usize) -> Account {
    let header = UpgradeableLoaderState::ProgramData {
        slot: 1,
        upgrade_authority_address: Some(*authority),
    };
    let code = vec![0u8; elf_len + 1024];
    loader_owned(loader_account_data(&header, &code), 10_000_000_000, false)
}

/// Executable `Program` pointing at `program_data`.
pub fn upgradeable_program_account(program_data: &Pubkey) -> Account {
    let header = UpgradeableLoaderState::Program {
        programdata_address: *program_data,
    };
    loader_owned(loader_account_data(&header, &[]), 1_000_000_000, true)
}

/// The upgradeable loader's own executable account.
pub fn keyed_account_for_loader_v3() -> (Pubkey, Account) {
    (
        loader_v3_id(),
        Account {
            lamports: 1,
            data: vec![],
            owner: Pubkey::from_str_const("NativeLoader1111111111111111111111111111111"),
            executable: true,
            rent_epoch: 0,
        },
    )
}

/// `BalanceAccount` PDA data owned by `program_id`.
pub fn balance_account_for(
    program_id: &Pubkey,
    chain: u16,
    token_chain: u16,
    token_address: [u8; 32],
    balance: Uint256,
) -> Account {
    let layout = BalanceAccountLayout::new(chain, token_chain, token_address, balance);
    Account {
        lamports: 1_500_000_000,
        data: bytemuck::bytes_of(&layout).to_vec(),
        owner: *program_id,
        executable: false,
        rent_epoch: 0,
    }
}

/// Registration as written by governance sequence 0; any other sequence overwrites it.
pub fn chain_registration_account_for(
    program_id: &Pubkey,
    chain: u16,
    emitter_address: [u8; 32],
) -> Account {
    let layout = ChainRegistrationLayout::new(chain, emitter_address, 0);
    Account {
        lamports: 1_000_000,
        data: bytemuck::bytes_of(&layout).to_vec(),
        owner: *program_id,
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

pub fn layout<T: bytemuck::Pod>(account: &Account) -> T {
    *bytemuck::from_bytes(&account.data)
}

pub fn balance_of(account: &Account) -> Uint256 {
    layout::<BalanceAccountLayout>(account).balance
}

pub fn pending_layout(account: &Account) -> PendingObservationsLayout {
    layout(account)
}

pub fn assert_closed(account: &Account, label: &str) {
    assert_eq!(account.lamports, 0, "{label}: lamports");
    assert_eq!(account.owner, system_program_id(), "{label}: owner");
    assert!(account.data.is_empty(), "{label}: data");
}

/// Balance PDA `key` is owned by `program_id` and holds `expected`.
pub fn assert_balance_for(
    program_id: &Pubkey,
    accounts: &[(Pubkey, Account)],
    key: &Pubkey,
    expected: Uint256,
) {
    let account = find_account(accounts, key);
    assert_eq!(account.owner, *program_id, "balance PDA owner {key}");
    assert_eq!(balance_of(account), expected, "balance {key}");
}

/// The accountant's NoReplay authority PDA for `program_id`.
pub fn noreplay_authority_pda(program_id: &Pubkey) -> Pubkey {
    accountant_operational_core::cpi::noreplay::derive_authority(program_id).0
}

pub fn program_data_address(program_id: &Pubkey) -> Pubkey {
    solana_loader_v3_interface::get_program_data_address(program_id)
}

pub fn program_data_metadata_len() -> usize {
    UpgradeableLoaderState::size_of_programdata_metadata()
}
