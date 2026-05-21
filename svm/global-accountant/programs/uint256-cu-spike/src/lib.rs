//! Minimal Pinocchio program that exercises `Uint256::checked_add` and
//! `Uint256::checked_sub` so a mollusk test can read back the
//! `compute_units_consumed` and set a baseline for the balance-accounting
//! arithmetic.
//!
//! Instruction data wire format:
//!
//! | offset | size | field            |
//! |--------|------|------------------|
//! | 0      | 32   | operand a (BE)   |
//! | 32     | 32   | operand b (BE)   |
//! | 64     | 1    | op (0 add, 1 sub)|
//!
//! Returns `Ok(())` on success (both branches: `Some` and `None`). The test
//! reads CU consumption rather than program output. To force two operations in
//! a single tx — matching the balance-account flow where every observation may
//! flip a sign — we run **both** add and sub regardless of the op byte and
//! discard the unused result. The op byte selects only which path's result we
//! treat as a "success" condition for the `Err` branch.

#![cfg_attr(target_os = "solana", no_std)]
#![allow(unexpected_cfgs)]

#[cfg(feature = "bpf-entrypoint")]
use pinocchio::program_entrypoint;
use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

use global_accountant_definitions::Uint256;

#[cfg(feature = "bpf-entrypoint")]
program_entrypoint!(process_instruction);
#[cfg(feature = "bpf-entrypoint")]
pinocchio::default_allocator!();
#[cfg(feature = "bpf-entrypoint")]
pinocchio::nostd_panic_handler!();

const DATA_LEN: usize = 32 + 32 + 1;

pub fn process_instruction(
    _program_id: &Address,
    _accounts: &mut [AccountView],
    data: &[u8],
) -> ProgramResult {
    let data: &[u8; DATA_LEN] = data.try_into().map_err(|_| ProgramError::InvalidInstructionData)?;

    let mut a_bytes = [0u8; 32];
    a_bytes.copy_from_slice(&data[0..32]);
    let mut b_bytes = [0u8; 32];
    b_bytes.copy_from_slice(&data[32..64]);
    let op = data[64];

    let a = Uint256::from_be_bytes(a_bytes);
    let b = Uint256::from_be_bytes(b_bytes);

    // Run both unconditionally so the CU measurement covers add + sub. The
    // `core::hint::black_box` walls keep the optimiser from folding either
    // side away.
    let add = core::hint::black_box(a.checked_add(b));
    let sub = core::hint::black_box(a.checked_sub(b));

    match op {
        0 => {
            // Add path: treat overflow as an error so the runtime surfaces a
            // failure case in measurements.
            if add.is_none() {
                return Err(ProgramError::ArithmeticOverflow);
            }
        }
        1 => {
            if sub.is_none() {
                return Err(ProgramError::ArithmeticOverflow);
            }
        }
        _ => return Err(ProgramError::InvalidInstructionData),
    }

    Ok(())
}
