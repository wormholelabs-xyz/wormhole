//! Integration tests for the `Retire` ix.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_backfill::{BackfillError, Instruction as IxDiscriminator},
    global_accountant_backfill::state::{BackfillAuthorityLayout, BACKFILL_AUTHORITY_SEED_PREFIX},
    mollusk_svm::{result::ProgramResult, Mollusk},
    solana_account::Account,
    solana_instruction::{error::InstructionError, AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

mod common;
use common::mollusk_with_noreplay;

fn program_id() -> Pubkey {
    Pubkey::new_from_array([8u8; 32])
}

fn mollusk() -> Mollusk {
    mollusk_with_noreplay(&program_id())
}

fn derive_backfill_authority_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[BACKFILL_AUTHORITY_SEED_PREFIX], &program_id())
}

fn signer_account(lamports: u64) -> Account {
    Account {
        lamports,
        data: vec![],
        owner: mollusk_svm::program::keyed_account_for_system_program().0,
        executable: false,
        rent_epoch: 0,
    }
}

/// Build an authority PDA pre-initialised with `(authority, retired)`.
fn initialised_authority_account(authority: Pubkey, retired: u8) -> Account {
    let mut layout: BackfillAuthorityLayout = bytemuck::Zeroable::zeroed();
    layout.authority = *authority.as_array();
    layout.retired = retired;
    Account {
        lamports: 1_000_000,
        data: bytemuck::bytes_of(&layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn build_ix() -> Vec<u8> {
    vec![IxDiscriminator::Retire as u8]
}

// ============================================================================
// Tests
// ============================================================================

/// Happy path: current authority retires itself.
#[test]
fn retire_authority_flips_retired_flag() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([42u8; 32]);
    let (auth_pda, _) = derive_backfill_authority_pda();

    let accounts = vec![
        (signer, signer_account(10_000_000_000)),
        (auth_pda, initialised_authority_account(signer, 0)),
    ];
    let metas = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new(auth_pda, false),
    ];
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_ix(),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw_result={:?}",
        result.raw_result
    );

    let auth = result.get_account(&auth_pda).unwrap();
    let layout: &BackfillAuthorityLayout = bytemuck::from_bytes(&auth.data);
    assert_eq!(layout.retired, 1, "retired flag should be set");
    assert_eq!(
        layout.authority,
        *signer.as_array(),
        "authority pubkey unchanged"
    );
}

/// A non-authority signer cannot retire.
#[test]
fn retire_authority_by_wrong_signer_rejects() {
    let mollusk = mollusk();
    let real_authority = Pubkey::new_from_array([42u8; 32]);
    let imposter = Pubkey::new_from_array([99u8; 32]);
    let (auth_pda, _) = derive_backfill_authority_pda();

    let accounts = vec![
        (imposter, signer_account(10_000_000_000)),
        (auth_pda, initialised_authority_account(real_authority, 0)),
    ];
    let metas = vec![
        AccountMeta::new(imposter, true),
        AccountMeta::new(auth_pda, false),
    ];
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_ix(),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == BackfillError::AuthorityMismatch as u32
        ),
        "expected AuthorityMismatch, got {:?}",
        result.raw_result
    );
}

/// Re-retiring an already-retired authority surfaces `AuthorityRetired`.
#[test]
fn retire_authority_already_retired_rejects() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([42u8; 32]);
    let (auth_pda, _) = derive_backfill_authority_pda();

    let accounts = vec![
        (signer, signer_account(10_000_000_000)),
        (auth_pda, initialised_authority_account(signer, 1)),
    ];
    let metas = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new(auth_pda, false),
    ];
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_ix(),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == BackfillError::AuthorityRetired as u32
        ),
        "expected AuthorityRetired, got {:?}",
        result.raw_result
    );
}

/// Retire before init must reject — there's no authority record to flip.
#[test]
fn retire_uninitialised_authority_rejects() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([42u8; 32]);
    let (auth_pda, _) = derive_backfill_authority_pda();

    let accounts = vec![
        (signer, signer_account(10_000_000_000)),
        (
            auth_pda,
            Account {
                lamports: 0,
                data: vec![],
                owner: mollusk_svm::program::keyed_account_for_system_program().0,
                executable: false,
                rent_epoch: 0,
            },
        ),
    ];
    let metas = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new(auth_pda, false),
    ];
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_ix(),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == BackfillError::InvalidPda as u32
        ),
        "expected InvalidPda on uninit auth PDA, got {:?}",
        result.raw_result
    );
}

/// Spurious data payload rejects (Retire takes no args).
#[test]
fn retire_with_extra_data_rejects() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([42u8; 32]);
    let (auth_pda, _) = derive_backfill_authority_pda();

    let accounts = vec![
        (signer, signer_account(10_000_000_000)),
        (auth_pda, initialised_authority_account(signer, 0)),
    ];
    let metas = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new(auth_pda, false),
    ];
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: vec![IxDiscriminator::Retire as u8, 0xAB], // one extra byte
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == BackfillError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData, got {:?}",
        result.raw_result
    );
}
