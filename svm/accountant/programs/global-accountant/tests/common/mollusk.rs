use accountant_test_fixtures::{Program, NOREPLAY_SO, VERIFY_VAA_SHIM_SO};
use global_accountant_definitions::{
    CORE_BRIDGE_PROGRAM_ID, NOREPLAY_PROGRAM_ID as NOREPLAY_PROGRAM_ID_BYTES,
    VERIFY_VAA_SHIM_PROGRAM_ID,
};
use mollusk_svm::program::{
    create_program_account_loader_v3, keyed_account_for_system_program, loader_keys::LOADER_V3,
};
use mollusk_svm::Mollusk;
use sha2::{Digest, Sha256};
use solana_account::Account;
use solana_pubkey::Pubkey;

pub const PROGRAM_NAME: &str = "global_accountant";
pub const NOREPLAY_PROGRAM_ID: Pubkey = Pubkey::new_from_array(NOREPLAY_PROGRAM_ID_BYTES);

pub fn program_id() -> Pubkey {
    Pubkey::new_from_array([7u8; 32])
}

pub fn system_program_id() -> Pubkey {
    keyed_account_for_system_program().0
}

pub fn core_bridge_program_id() -> Pubkey {
    Pubkey::new_from_array(CORE_BRIDGE_PROGRAM_ID)
}

pub fn shim_program_id() -> Pubkey {
    Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID)
}

pub fn noreplay_program_id() -> Pubkey {
    NOREPLAY_PROGRAM_ID
}

pub fn mollusk() -> Mollusk {
    mollusk_with_fixtures(&program_id(), PROGRAM_NAME)
}

pub fn mollusk_with_fixtures(program_id: &Pubkey, program_name: &str) -> Mollusk {
    let mut mollusk = Mollusk::new(program_id, program_name);
    let noreplay_elf = program_elf(&NOREPLAY_SO, "solana_noreplay", "GA_NOREPLAY_SO");
    let shim_elf = program_elf(
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

fn program_elf(program: &Program, label: &str, env_override: &str) -> Vec<u8> {
    if let Ok(path) = std::env::var(env_override) {
        return std::fs::read(&path)
            .unwrap_or_else(|e| panic!("read ${env_override}={path} for `{label}`: {e}"));
    }
    let actual = Sha256::digest(program.bytes);
    assert_eq!(
        actual[..],
        program.sha256,
        "fixture `{label}` SHA-256 mismatch; rebuild the sibling program, copy the .so over \
         `crates/test-fixtures/data/{label}.so`, update `sha256` in `crates/test-fixtures/src/lib.rs`"
    );
    program.bytes.to_vec()
}
