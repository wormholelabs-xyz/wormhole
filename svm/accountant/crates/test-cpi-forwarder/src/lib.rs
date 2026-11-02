//! Test-only program. Invokes `accounts[0]` with `accounts[1..]` and the instruction data.
//! Do not deploy it.

#![allow(unexpected_cfgs)]

use anchor_lang::solana_program::account_info::AccountInfo;
use anchor_lang::solana_program::entrypoint;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke;
use anchor_lang::solana_program::program_error::ProgramError;
use anchor_lang::solana_program::pubkey::Pubkey;

entrypoint!(process_instruction);

pub fn process_instruction(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> Result<(), ProgramError> {
    let [target, rest @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let metas = rest
        .iter()
        .map(|account| AccountMeta {
            pubkey: *account.key,
            is_signer: account.is_signer,
            is_writable: account.is_writable,
        })
        .collect();
    let ix = Instruction {
        program_id: *target.key,
        accounts: metas,
        data: data.to_vec(),
    };
    invoke(&ix, rest)
}
