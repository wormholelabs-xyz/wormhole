//! Mollusk harness and in-memory account stubs. Centralised so the canonical
//! program id, payer, and account stubs stay identical across the suites.

#![allow(dead_code)] // Different test files use different subsets.

use {
    accountant_test_fixtures::NOREPLAY_SO,
    global_accountant_definitions::NOREPLAY_PROGRAM_ID,
    mollusk_svm::{
        program::{
            create_program_account_loader_v3, keyed_account_for_system_program,
            loader_keys::LOADER_V3,
        },
        Mollusk,
    },
    solana_account::Account,
    solana_keypair::Keypair,
    solana_pubkey::Pubkey,
    solana_signer::Signer,
};

use super::fixtures::{program_elf, BACKFILL_PROGRAM_NAME};

/// Canonical test program id (`[8u8; 32]`), deterministic so PDAs derive to
/// the same addresses across both test suites.
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
/// [`global_accountant_backfill::BACKFILL_AUTHORITY`] const. Seed
/// `[1u8; 32]`: smallest non-zero value, reproducible across machines.
/// Verified by
/// `tests/backfill_noreplay.rs::backfill_authority_const_matches_test_keypair`.
pub fn test_authority_keypair() -> Keypair {
    Keypair::new_from_array([1u8; 32])
}

pub fn test_authority_pubkey() -> Pubkey {
    test_authority_keypair().pubkey()
}

/// Empty, system-owned account with the given lamport balance. Used as a
/// signing payer, or (with `lamports = 0`) as the uninitialised PDA stub
/// mollusk hands the program for lazy-init.
pub fn system_owned_account(lamports: u64) -> Account {
    Account {
        lamports,
        data: vec![],
        owner: system_program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

/// Alias of `system_owned_account`, for call sites using it as a signing payer.
pub fn signer_account(lamports: u64) -> Account {
    system_owned_account(lamports)
}

/// Uninitialised PDA stub: zero lamports, zero-length data, system-owned.
/// Drives the fresh-`CreateAccount` branch of `pda_init::create_pda_allow_prefund`.
pub fn uninitialised_pda_account() -> Account {
    system_owned_account(0)
}

/// Build a `Mollusk` with the backfill `.so` plus the real
/// `solana_noreplay.so` at its canonical ID.
pub fn mollusk_with_noreplay(program_id: &Pubkey) -> Mollusk {
    let mut mollusk = Mollusk::new(program_id, BACKFILL_PROGRAM_NAME);

    let noreplay_elf = program_elf(&NOREPLAY_SO, "solana_noreplay", "GA_NOREPLAY_SO");
    mollusk.add_program_with_loader_and_elf(
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        &LOADER_V3,
        &noreplay_elf,
    );

    mollusk
}

/// `(pubkey, Account)` for the noreplay program. Mollusk consumes the
/// account list verbatim; a system-owned stand-in fails at CPI time as
/// `UnsupportedProgramId`.
pub fn keyed_account_for_noreplay_program() -> (Pubkey, Account) {
    let id = Pubkey::new_from_array(NOREPLAY_PROGRAM_ID);
    let account = create_program_account_loader_v3(&id);
    (id, account)
}
