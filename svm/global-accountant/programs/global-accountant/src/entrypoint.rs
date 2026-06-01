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
        // `open_digest` is publicly callable only under
        // `test-only-open-digest`; in production it is reachable only from
        // inside `submit_observations` after the NoReplay check. A public
        // `open_digest` entrypoint would let any caller squat the PDA for any
        // `(chain, emitter, sequence)`, so the feature gate keeps the symbol
        // out of production builds.
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
        Some(Instruction::ClosePending) => {
            instructions::close_pending::process(program_id, accounts, rest)
        }
        Some(Instruction::SubmitVaas) => {
            instructions::submit_vaas::process(program_id, accounts, rest)
        }
        Some(Instruction::RegisterChain) => {
            instructions::register_chain::process(program_id, accounts, rest)
        }
        None => Err(err(GlobalAccountantError::InvalidInstruction)),
    }
}
