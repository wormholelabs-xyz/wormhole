use anchor_lang::prelude::*;
use anchor_lang::solana_program::system_program;

use crate::definitions::{
    ChainRegistrationLayout, GlobalAccountantError, CHAIN_REGISTRATION_SEED_PREFIX,
};
use crate::{err, ProgramCoreResult, ProgramResult};

pub fn derive_pda(program_id: &Pubkey, chain: u16) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[CHAIN_REGISTRATION_SEED_PREFIX, &chain.to_be_bytes()],
        program_id,
    )
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
    let (expected, _) = derive_pda(program_id, body_chain);
    if registration_pda.key != &expected {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let layout = load(registration_pda)?;
    if layout.emitter_address != *body_emitter {
        return Err(err(GlobalAccountantError::UnregisteredEmitter));
    }
    Ok(())
}
