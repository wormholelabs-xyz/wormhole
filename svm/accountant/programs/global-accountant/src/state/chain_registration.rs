//! Load / store / check helpers for `ChainRegistrationLayout`.

use anchor_lang::prelude::*;

use crate::definitions::{
    ChainRegistrationLayout, GlobalAccountantError, CHAIN_REGISTRATION_SEED_PREFIX,
};
use crate::err;

/// `MissingChainRegistration` if system-owned; `InvalidPda` on wrong length or tag.
/// Caller checks the address first.
pub fn load(account: &AccountInfo) -> accountant_operational_core::ProgramCoreResult<ChainRegistrationLayout> {
    if account.owner == &anchor_lang::solana_program::system_program::ID {
        return Err(err(GlobalAccountantError::MissingChainRegistration));
    }
    let data = account.try_borrow_data()?;
    if data.len() != ChainRegistrationLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let layout = bytemuck::from_bytes::<ChainRegistrationLayout>(&data);
    if layout.tag != ChainRegistrationLayout::TAG {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(*layout)
}

pub fn store(
    account: &AccountInfo,
    value: &ChainRegistrationLayout,
) -> accountant_operational_core::ProgramResult {
    let mut data = account.try_borrow_mut_data()?;
    if data.len() != ChainRegistrationLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    data.copy_from_slice(bytemuck::bytes_of(value));
    Ok(())
}

/// SECURITY: `registration_pda` must be at the address for `body_chain`, and its
/// `emitter_address` must equal `body_emitter`.
pub fn verify(
    program_id: &Pubkey,
    registration_pda: &AccountInfo,
    body_chain: u16,
    body_emitter: &[u8; 32],
) -> accountant_operational_core::ProgramResult {
    let chain_be = body_chain.to_be_bytes();
    let (expected, _bump) =
        Pubkey::find_program_address(&[CHAIN_REGISTRATION_SEED_PREFIX, &chain_be], program_id);
    if registration_pda.key != &expected {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let layout = load(registration_pda)?;
    if layout.emitter_address != *body_emitter {
        return Err(err(GlobalAccountantError::UnregisteredEmitter));
    }
    Ok(())
}
