//! BPF entrypoint and instruction dispatch.

#[cfg(feature = "bpf-entrypoint")]
use pinocchio::program_entrypoint;
use pinocchio::{AccountView, Address, ProgramResult};

use crate::{err, instructions, BackfillError, Instruction, BACKFILL_AUTHORITY};

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
            instructions::backfill_noreplay::process(program_id, accounts, rest, &BACKFILL_AUTHORITY)
        }
        Some(Instruction::BackfillBalance) => {
            instructions::backfill_balance::process(program_id, accounts, rest, &BACKFILL_AUTHORITY)
        }
        None => Err(err(BackfillError::InvalidInstruction)),
    }
}
