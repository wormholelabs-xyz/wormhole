//! Shared mollusk fixtures for the NTT backfill map-instruction tests. The
//! three handlers do no CPI, so a plain `Mollusk` with just the NTT `.so`
//! (resolved from `SBF_OUT_DIR`) is all that's needed.

#![allow(dead_code)]

use {
    mollusk_svm::{program::keyed_account_for_system_program, Mollusk},
    solana_account::Account,
    solana_keypair::Keypair,
    solana_pubkey::Pubkey,
    solana_signer::Signer,
};

/// Canonical test program id. Arbitrary but deterministic; distinct from the
/// WTT suite's id so the two never share derived PDAs.
pub fn program_id() -> Pubkey {
    Pubkey::new_from_array([9u8; 32])
}

/// Mollusk loaded with the NTT backfill `.so` from `SBF_OUT_DIR`.
pub fn mollusk() -> Mollusk {
    Mollusk::new(&program_id(), "ntt_global_accountant_backfill")
}

pub fn system_program_id() -> Pubkey {
    keyed_account_for_system_program().0
}

/// Deterministic test keypair matching the NTT `BACKFILL_AUTHORITY` const
/// (seed `[1u8; 32]`).
pub fn test_authority_keypair() -> Keypair {
    Keypair::new_from_array([1u8; 32])
}

pub fn test_authority_pubkey() -> Pubkey {
    test_authority_keypair().pubkey()
}

/// Empty, system-owned account with the given lamport balance.
pub fn system_owned_account(lamports: u64) -> Account {
    Account {
        lamports,
        data: vec![],
        owner: system_program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

pub fn signer_account(lamports: u64) -> Account {
    system_owned_account(lamports)
}

/// Uninitialised PDA stub: zero lamports, zero-length data, system-owned.
/// Drives the `CreateAccount` branch of `pda_init::init_or_upgrade_pda`.
pub fn uninitialised_pda_account() -> Account {
    system_owned_account(0)
}
