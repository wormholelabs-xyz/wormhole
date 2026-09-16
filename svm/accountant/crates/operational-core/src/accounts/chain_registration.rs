use anchor_lang::prelude::*;
use anchor_lang::solana_program::system_program;

use crate::definitions::{
    ChainRegistrationLayout, GlobalAccountantError, CHAIN_REGISTRATION_SEED_PREFIX,
};
use crate::support::pda_init::create_pda_allow_prefund;
use crate::{err, ProgramCoreResult, ProgramResult};

pub fn derive_pda(program_id: &Pubkey, chain: u16) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[CHAIN_REGISTRATION_SEED_PREFIX, &chain.to_be_bytes()],
        program_id,
    )
}

/// Allocate the `ChainRegistration` PDA for `layout`'s chain and write `layout`. Seeds
/// derive from the layout so address and contents cannot disagree. The governance handler
/// and the backfill both call this, so the account they produce is byte-identical.
pub fn create<'info>(
    program_id: &Pubkey,
    payer: &AccountInfo<'info>,
    registration_pda: &AccountInfo<'info>,
    canonical_bump: u8,
    layout: &ChainRegistrationLayout,
) -> ProgramResult {
    let chain_be = layout.chain.to_be_bytes();
    let bump_seed = [canonical_bump];
    let seeds: &[&[u8]] = &[CHAIN_REGISTRATION_SEED_PREFIX, &chain_be, &bump_seed];
    create_pda_allow_prefund(
        payer,
        registration_pda,
        program_id,
        seeds,
        ChainRegistrationLayout::LEN as u64,
    )?;
    store(registration_pda, layout)
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
