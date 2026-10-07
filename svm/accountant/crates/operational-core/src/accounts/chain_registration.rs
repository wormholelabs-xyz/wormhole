use anchor_lang::prelude::*;
use anchor_lang::solana_program::system_program;

use crate::definitions::{ChainRegistrationKey, ChainRegistrationLayout, GlobalAccountantError};
use crate::support::pda;
use crate::{err, ProgramCoreResult, ProgramResult};

pub fn derive_pda(program_id: &Pubkey, chain: u16) -> (Pubkey, u8) {
    pda::derive(program_id, &ChainRegistrationKey::new(chain))
}

pub fn load(account: &AccountInfo) -> ProgramCoreResult<ChainRegistrationLayout> {
    if account.owner == &system_program::ID {
        return Err(err(GlobalAccountantError::MissingChainRegistration));
    }
    super::load(account)
}

pub fn store(account: &AccountInfo, value: &ChainRegistrationLayout) -> ProgramResult {
    super::store(account, value)
}

pub fn verify(
    program_id: &Pubkey,
    registration_pda: &AccountInfo,
    body_chain: u16,
    body_emitter: &[u8; 32],
) -> ProgramResult {
    pda::check(
        program_id,
        registration_pda,
        &ChainRegistrationKey::new(body_chain),
    )?;
    let layout = load(registration_pda)?;
    if layout.emitter_address != *body_emitter {
        return Err(err(GlobalAccountantError::UnregisteredEmitter));
    }
    Ok(())
}

/// Whether the registration for `chain` names `emitter`. `registration_pda` must be the
/// canonical address; an uninitialised PDA means no registration.
pub fn is_registered_emitter(
    program_id: &Pubkey,
    registration_pda: &AccountInfo,
    chain: u16,
    emitter: &[u8; 32],
) -> ProgramCoreResult<bool> {
    pda::check(
        program_id,
        registration_pda,
        &ChainRegistrationKey::new(chain),
    )?;
    let Some(layout) =
        pda::read_if_initialised::<ChainRegistrationLayout>(program_id, registration_pda)?
    else {
        return Ok(false);
    };
    Ok(layout.emitter_address == *emitter)
}
