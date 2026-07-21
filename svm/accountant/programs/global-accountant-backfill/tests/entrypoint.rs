//! Integration tests for the BPF entrypoint's own dispatch logic
//! (`src/entrypoint.rs::process_instruction`), independent of either
//! instruction handler's internal wire format.
//!
//! Contract under test:
//! - Empty `instruction_data` (no discriminator byte at all) is rejected via
//!   `split_first()` failing, mapped to `InvalidInstructionData`.
//! - A discriminator byte outside `Instruction::from_u8`'s range (0 or 1) is
//!   rejected as `InvalidInstruction`.

use {
    global_accountant_backfill::BackfillError,
    mollusk_svm::result::ProgramResult,
    solana_account::Account,
    solana_instruction::{error::InstructionError, Instruction},
    solana_pubkey::Pubkey,
};

mod common;
use common::mollusk::{mollusk, program_id};

/// `split_first()` fails on empty `instruction_data`, mapped directly to
/// `InvalidInstructionData` before any account is inspected.
#[test]
fn entrypoint_empty_instruction_data_rejects() {
    let mollusk = mollusk();
    let ix = Instruction {
        program_id: program_id(),
        accounts: vec![],
        data: vec![],
    };
    let accounts: Vec<(Pubkey, Account)> = vec![];
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == BackfillError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData for empty instruction_data, got {:?}",
        result.raw_result
    );
}

/// A discriminator byte outside `{0, 1}` (`BackfillNoReplay`,
/// `BackfillBalance`) must be rejected as `InvalidInstruction` —
/// `Instruction::from_u8` returns `None` for anything else.
#[test]
fn entrypoint_invalid_discriminator_rejects() {
    let mollusk = mollusk();
    let ix = Instruction {
        program_id: program_id(),
        accounts: vec![],
        data: vec![0xFFu8],
    };
    let accounts: Vec<(Pubkey, Account)> = vec![];
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == BackfillError::InvalidInstruction as u32
        ),
        "expected InvalidInstruction for out-of-range discriminator, got {:?}",
        result.raw_result
    );
}

/// Sanity: another out-of-range discriminator value (`2`, the first value
/// past the last defined variant) is rejected the same way — guards against
/// an off-by-one in `Instruction::from_u8`'s match arms.
#[test]
fn entrypoint_discriminator_two_rejects() {
    let mollusk = mollusk();
    let ix = Instruction {
        program_id: program_id(),
        accounts: vec![],
        data: vec![2u8],
    };
    let accounts: Vec<(Pubkey, Account)> = vec![];
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == BackfillError::InvalidInstruction as u32
        ),
        "expected InvalidInstruction for discriminator == 2, got {:?}",
        result.raw_result
    );
}

/// `ProgramResult::Failure` is what mollusk reports for a returned
/// `ProgramError`; confirms the entrypoint's rejection is a clean `Err`
/// return.
#[test]
fn entrypoint_invalid_discriminator_is_clean_failure_not_abort() {
    let mollusk = mollusk();
    let ix = Instruction {
        program_id: program_id(),
        accounts: vec![],
        data: vec![0xFFu8],
    };
    let accounts: Vec<(Pubkey, Account)> = vec![];
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(result.program_result, ProgramResult::Failure(_)),
        "expected a clean Failure, got {:?}",
        result.program_result
    );
}
