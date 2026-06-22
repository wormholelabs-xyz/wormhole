#[cfg(feature = "bpf-entrypoint")]
use pinocchio::program_entrypoint;
use pinocchio::{AccountView, Address, ProgramResult};

use accountant_operational_core::instructions as core_instructions;

use crate::definitions::{
    parse_token_bridge_payload, GlobalAccountantError, Instruction, TokenBridgeAction,
};
use crate::instructions::transfer::apply_transfer;
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
        Some(Instruction::SubmitObservations) => core_instructions::submit_observations::process(
            program_id,
            accounts,
            rest,
            apply_token_bridge_transfer,
        ),
        Some(Instruction::ClosePending) => {
            core_instructions::close_pending::process(program_id, accounts, rest)
        }
        Some(Instruction::SubmitVaas) => core_instructions::submit_vaas::process(
            program_id,
            accounts,
            rest,
            apply_token_bridge_transfer,
        ),
        Some(Instruction::RegisterChain) => {
            instructions::register_chain::process(program_id, accounts, rest)
        }
        Some(Instruction::ModifyBalance) => {
            instructions::modify_balance::process(program_id, accounts, rest)
        }
        None => Err(err(GlobalAccountantError::InvalidInstruction)),
    }
}

/// WTT balance applicator injected into the core `submit_observations` /
/// `submit_vaas` handlers. Parses the verified VAA body's Token Bridge payload
/// and mutates the source / destination balance-account PDAs. Attest payloads
/// no-op the balance work; unknown payloads reject so the NoReplay mark rolls
/// back with the tx, leaving the slot unconsumed for a future upgrade.
fn apply_token_bridge_transfer(
    program_id: &Address,
    submitter: &mut AccountView,
    source_account_pda: &mut AccountView,
    dest_account_pda: &mut AccountView,
    source_chain: u16,
    body_bytes: &[u8],
) -> ProgramResult {
    match parse_token_bridge_payload(body_bytes).map_err(err)? {
        TokenBridgeAction::Transfer {
            amount,
            token_chain,
            token_address,
            recipient_chain,
        } => apply_transfer(
            program_id,
            submitter,
            source_account_pda,
            dest_account_pda,
            source_chain,
            recipient_chain,
            token_chain,
            &token_address,
            amount,
        ),
        TokenBridgeAction::Attest => {
            // No balance work; the source / dest slots are untouched.
            Ok(())
        }
        TokenBridgeAction::Other => Err(err(GlobalAccountantError::UnknownTokenBridgePayload)),
    }
}
