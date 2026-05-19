use pinocchio::{AccountView, Address, ProgramResult};
#[cfg(feature = "bpf-entrypoint")]
use pinocchio::program_entrypoint;

use crate::definitions::{GlobalAccountantError, Instruction};
use crate::{err, instructions};

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
        .ok_or_else(|| err(GlobalAccountantError::InvalidInstructionData))?;

    match Instruction::from_u8(*discriminator) {
        // `open_digest` is publicly callable in this Phase 1 slice; in
        // production it will only be invoked from inside `submit_observations`
        // after the NoReplay check. Until that lands, a public `open_digest`
        // entrypoint lets any caller squat the PDA for any
        // `(chain, emitter, sequence)`. The `test-only-open-digest` feature
        // gate keeps the symbol out of production builds.
        #[cfg(feature = "test-only-open-digest")]
        Some(Instruction::OpenDigest) => {
            instructions::open_digest::process(program_id, accounts, rest)
        }
        #[cfg(not(feature = "test-only-open-digest"))]
        Some(Instruction::OpenDigest) => Err(err(GlobalAccountantError::NotEnabled)),
        Some(Instruction::CloseDigest) => {
            instructions::close_digest::process(program_id, accounts, rest)
        }
        Some(Instruction::SubmitObservations) => {
            instructions::submit_observations::process(program_id, accounts, rest)
        }
        None => Err(err(GlobalAccountantError::InvalidInstruction)),
    }
}
