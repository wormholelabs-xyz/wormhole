//! Program-owned PDA toolkit: derive, check, probe and create from a [`PdaSeeds`] key.
//!
//! SECURITY: only this program can sign for its PDA addresses, so an account at a canonical
//! address is either system-owned (absent) or written by this program. Every probe and read
//! here must run after `check` on the same account.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::system_program;

use crate::accounts;
use crate::definitions::{AccountLayout, GlobalAccountantError, PdaSeeds, MAX_SEEDS};
use crate::support::pda_init::create_pda_allow_prefund;
use crate::{err, ProgramCoreResult, ProgramResult};

pub fn derive(program_id: &Pubkey, key: &impl PdaSeeds) -> (Pubkey, u8) {
    Pubkey::find_program_address(key.seeds().as_slice(), program_id)
}

/// `account` must sit at `key`'s canonical address, else `InvalidPda`; returns the bump.
pub fn check(
    program_id: &Pubkey,
    account: &AccountInfo,
    key: &impl PdaSeeds,
) -> ProgramCoreResult<u8> {
    check_or(program_id, account, key, GlobalAccountantError::InvalidPda)
}

/// [`check`] with a caller-chosen error.
pub fn check_or(
    program_id: &Pubkey,
    account: &AccountInfo,
    key: &impl PdaSeeds,
    error: GlobalAccountantError,
) -> ProgramCoreResult<u8> {
    let (expected, bump) = derive(program_id, key);
    if account.key != &expected {
        return Err(err(error));
    }
    Ok(bump)
}

/// [`check`], then the account must be uninitialised, else `on_duplicate`.
pub fn check_uninitialised(
    program_id: &Pubkey,
    account: &AccountInfo,
    key: &impl PdaSeeds,
    on_duplicate: GlobalAccountantError,
) -> ProgramCoreResult<u8> {
    let bump = check(program_id, account, key)?;
    if is_initialised(program_id, account)? {
        return Err(err(on_duplicate));
    }
    Ok(bump)
}

/// `false` for a system-owned account, `true` for one owned by this program, `InvalidPda` for
/// any other owner.
pub fn is_initialised(program_id: &Pubkey, account: &AccountInfo) -> ProgramCoreResult<bool> {
    if account.owner == &system_program::ID {
        return Ok(false);
    }
    if account.owner != program_id {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(true)
}

/// The layout, or `None` when the account is uninitialised.
pub fn read_if_initialised<L: AccountLayout>(
    program_id: &Pubkey,
    account: &AccountInfo,
) -> ProgramCoreResult<Option<L>> {
    if !is_initialised(program_id, account)? {
        return Ok(None);
    }
    accounts::load(account).map(Some)
}

/// Allocate `account` at `key`'s address with `bump` and write `layout`.
pub fn create<'info, L: AccountLayout>(
    program_id: &Pubkey,
    payer: &AccountInfo<'info>,
    account: &AccountInfo<'info>,
    key: &impl PdaSeeds,
    bump: u8,
    layout: &L,
) -> ProgramResult {
    let seeds = key.seeds();
    let seeds = seeds.as_slice();
    let bump_seed = [bump];
    let mut signer: [&[u8]; MAX_SEEDS + 1] = [&[]; MAX_SEEDS + 1];
    signer[..seeds.len()].copy_from_slice(seeds);
    signer[seeds.len()] = &bump_seed;
    create_pda_allow_prefund(
        payer,
        account,
        program_id,
        &signer[..=seeds.len()],
        L::LEN as u64,
    )?;
    accounts::store(account, layout)
}
