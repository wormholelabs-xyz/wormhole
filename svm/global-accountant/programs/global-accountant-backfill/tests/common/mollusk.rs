//! Shared mollusk fixtures used by both `backfill_noreplay.rs` and
//! `backfill_balance.rs`. Centralised so the canonical program id, payer,
//! and account stubs can't drift between the two suites.

#![allow(dead_code)] // Different test files use different subsets.

use {
    mollusk_svm::{program::keyed_account_for_system_program, Mollusk},
    solana_account::Account,
    solana_keypair::Keypair,
    solana_pubkey::Pubkey,
    solana_signer::Signer,
};

use super::mollusk_with_noreplay;

/// Canonical test program id (`[8u8; 32]`). Arbitrary but deterministic so
/// PDAs derive to the same addresses across the two test suites.
pub fn program_id() -> Pubkey {
    Pubkey::new_from_array([8u8; 32])
}

/// Mollusk loaded with the backfill `.so` + the pinned `solana_noreplay.so`.
pub fn mollusk() -> Mollusk {
    mollusk_with_noreplay(&program_id())
}

/// Pubkey of the system program (`11111111111111111111111111111111`).
pub fn system_program_id() -> Pubkey {
    keyed_account_for_system_program().0
}

/// Deterministic test keypair whose pubkey matches the compile-time
/// [`global_accountant_backfill::BACKFILL_AUTHORITY`] const. Seed `[1u8; 32]`
/// chosen because it's the smallest non-zero value and reproducible across
/// machines. Verified by
/// `tests/backfill_noreplay.rs::backfill_authority_const_matches_test_keypair`.
pub fn test_authority_keypair() -> Keypair {
    Keypair::new_from_array([1u8; 32])
}

pub fn test_authority_pubkey() -> Pubkey {
    test_authority_keypair().pubkey()
}

/// Empty, system-owned account with the given lamport balance. Used both as
/// a signing payer and (with `lamports = 0`) as the uninitialised PDA stub
/// that mollusk hands to the program for lazy-init.
pub fn system_owned_account(lamports: u64) -> Account {
    Account {
        lamports,
        data: vec![],
        owner: system_program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

/// Convenience: a freshly-airdropped signer with 10 SOL — enough for any
/// rent debit + fees the test suite incurs.
pub fn signer_account(lamports: u64) -> Account {
    system_owned_account(lamports)
}

/// Uninitialised PDA stub: zero lamports, zero-length data, system-owned.
/// Drives the `CreateAccount` (fresh) branch of `pda_init::init_or_upgrade_pda`.
pub fn uninitialised_pda_account() -> Account {
    system_owned_account(0)
}
