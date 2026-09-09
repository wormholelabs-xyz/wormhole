//! Mollusk harness construction and in-memory account helpers.

use accountant_test_fixtures::{NOREPLAY_SO, VERIFY_VAA_SHIM_SO};
use mollusk_svm::program::{create_program_account_loader_v3, loader_keys::LOADER_V3};
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_pubkey::Pubkey;

use super::fixtures::{fixture_elf, PROGRAM_NAME};
use super::ids::{noreplay_program_id, program_id, shim_program_id, system_program_id};

pub fn mollusk() -> Mollusk {
    mollusk_with_fixtures(&program_id(), PROGRAM_NAME)
}

pub fn mollusk_with_fixtures(program_id: &Pubkey, program_name: &str) -> Mollusk {
    let mut mollusk = Mollusk::new(program_id, program_name);
    let noreplay_elf = fixture_elf(&NOREPLAY_SO, "solana_noreplay", "GA_NOREPLAY_SO");
    let shim_elf = fixture_elf(
        &VERIFY_VAA_SHIM_SO,
        "wormhole_verify_vaa_shim",
        "GA_VERIFY_VAA_SHIM_SO",
    );
    mollusk.add_program_with_loader_and_elf(&noreplay_program_id(), &LOADER_V3, &noreplay_elf);
    mollusk.add_program_with_loader_and_elf(&shim_program_id(), &LOADER_V3, &shim_elf);
    mollusk
}

pub fn keyed_account_for_noreplay_program() -> (Pubkey, Account) {
    let id = noreplay_program_id();
    (id, create_program_account_loader_v3(&id))
}

pub fn keyed_account_for_verify_vaa_shim_program() -> (Pubkey, Account) {
    let id = shim_program_id();
    (id, create_program_account_loader_v3(&id))
}

pub fn system_owned_account(lamports: u64) -> Account {
    Account {
        lamports,
        data: vec![],
        owner: system_program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

pub fn uninitialised_pda_account() -> Account {
    system_owned_account(0)
}

pub fn find_account<'a>(accounts: &'a [(Pubkey, Account)], key: &Pubkey) -> &'a Account {
    &accounts
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("account {key} not in result list"))
        .1
}

pub fn replace_account(accounts: &mut [(Pubkey, Account)], key: &Pubkey, account: Account) {
    let slot = accounts
        .iter_mut()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("account {key} not in account list"));
    slot.1 = account;
}
