//! BPF entrypoint and instruction dispatch.

#[cfg(feature = "bpf-entrypoint")]
use pinocchio::program_entrypoint;
use pinocchio::{AccountView, Address, ProgramResult};

use accountant_operational_core::instructions as core_instructions;

use crate::definitions::GlobalAccountantError;
use crate::{err, instructions, Instruction};

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
        // NTT quorum tracker + signed-VAA backfill over the shared core
        // primitives, running the NTT transfer flow on the committing branch.
        Some(Instruction::SubmitObservations) => {
            instructions::submit_observations::process(program_id, accounts, rest)
        }
        Some(Instruction::SubmitVaas) => {
            instructions::submit_vaas::process(program_id, accounts, rest)
        }
        // Product-neutral cleanup handler, reused as-is from the core crate.
        Some(Instruction::ClosePending) => {
            core_instructions::close_pending::process(program_id, accounts, rest)
        }
        // NTT governance handlers.
        Some(Instruction::RegisterRelayerChain) => {
            instructions::register_relayer_chain::process(program_id, accounts, rest)
        }
        Some(Instruction::ModifyBalance) => {
            instructions::modify_balance::process(program_id, accounts, rest)
        }
        // Hub/peer registration handlers built in a separate task.
        Some(Instruction::RegisterHub) | Some(Instruction::RegisterPeer) => {
            Err(err(GlobalAccountantError::NotEnabled))
        }
        None => Err(err(GlobalAccountantError::InvalidInstruction)),
    }
}
