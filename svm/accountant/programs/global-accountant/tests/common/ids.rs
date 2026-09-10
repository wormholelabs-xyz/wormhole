//! Addresses shared by the mollusk and surfpool suites.

use global_accountant_definitions::{
    COMPUTE_BUDGET_PROGRAM_ID, CORE_BRIDGE_PROGRAM_ID,
    NOREPLAY_PROGRAM_ID as NOREPLAY_PROGRAM_ID_BYTES, VERIFY_VAA_SHIM_PROGRAM_ID,
};
use mollusk_svm::program::{keyed_account_for_system_program, loader_keys::LOADER_V3};
use solana_pubkey::Pubkey;

pub const NOREPLAY_PROGRAM_ID: Pubkey = Pubkey::new_from_array(NOREPLAY_PROGRAM_ID_BYTES);

/// The accountant's `declare_id!` address. Both suites load the program here.
pub fn program_id() -> Pubkey {
    Pubkey::new_from_array(global_accountant::ID.to_bytes())
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

pub fn compute_budget_program_id() -> Pubkey {
    Pubkey::new_from_array(COMPUTE_BUDGET_PROGRAM_ID)
}

pub fn loader_v3_id() -> Pubkey {
    LOADER_V3
}

pub fn rent_sysvar_id() -> Pubkey {
    Pubkey::from_str_const("SysvarRent111111111111111111111111111111111")
}

pub fn clock_sysvar_id() -> Pubkey {
    Pubkey::from_str_const("SysvarC1ock11111111111111111111111111111111")
}
