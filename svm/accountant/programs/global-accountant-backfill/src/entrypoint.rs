//! BPF entrypoint and instruction dispatch.

#[cfg(feature = "bpf-entrypoint")]
use pinocchio::program_entrypoint;
use pinocchio::{AccountView, Address, ProgramResult};

use accountant_backfill_core::{backfill_balance, backfill_noreplay, err, BackfillError};

use crate::{Instruction, BACKFILL_AUTHORITY};

#[cfg(feature = "bpf-entrypoint")]
program_entrypoint!(process_instruction);
#[cfg(feature = "bpf-entrypoint")]
pinocchio::default_allocator!();
#[cfg(feature = "bpf-entrypoint")]
pinocchio::nostd_panic_handler!();

pub fn process_instruction(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let (discriminator, rest) = instruction_data
        .split_first()
        .ok_or_else(|| err(BackfillError::InvalidInstructionData))?;

    match Instruction::from_u8(*discriminator) {
        Some(Instruction::BackfillNoReplay) => {
            backfill_noreplay::process(program_id, accounts, rest, &BACKFILL_AUTHORITY)
        }
        Some(Instruction::BackfillBalance) => {
            backfill_balance::process(program_id, accounts, rest, &BACKFILL_AUTHORITY)
        }
        None => Err(err(BackfillError::InvalidInstruction)),
    }
}
