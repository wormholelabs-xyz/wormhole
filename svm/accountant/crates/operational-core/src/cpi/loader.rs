//! BPF upgradeable loader `Upgrade` CPI, signed by this program's upgrade authority PDA
//! at `[UPGRADE_AUTHORITY_SEED_PREFIX]`.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::bpf_loader_upgradeable;
use anchor_lang::solana_program::program::invoke_signed;

use crate::definitions::{GlobalAccountantError, UPGRADE_AUTHORITY_SEED_PREFIX};
use crate::{err, ProgramResult};

/// Upgrade authority PDA `(address, bump)` for `program_id`.
pub fn derive_upgrade_authority(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[UPGRADE_AUTHORITY_SEED_PREFIX], program_id)
}

/// Replace `program_id`'s code with the buffer at `new_contract` via the loader.
///
/// SECURITY: the CPI target is the loader program id from the SDK, never a caller-supplied
/// account. `program`, `program_data`, `buffer`, and `upgrade_authority` addresses are
/// re-derived or compared against `program_id` / `new_contract` before the CPI.
///
/// An inner-program error aborts this program directly; `Err` here is a pre-CPI failure.
#[allow(clippy::too_many_arguments)]
pub fn upgrade_program<'info>(
    program: &AccountInfo<'info>,
    program_data: &AccountInfo<'info>,
    buffer: &AccountInfo<'info>,
    spill: &AccountInfo<'info>,
    upgrade_authority: &AccountInfo<'info>,
    rent: &AccountInfo<'info>,
    clock: &AccountInfo<'info>,
    program_id: &Pubkey,
    new_contract: &Pubkey,
) -> ProgramResult {
    let (expected_authority, authority_bump) = derive_upgrade_authority(program_id);
    if upgrade_authority.key != &expected_authority {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    if program.key != program_id {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    if program_data.key != &bpf_loader_upgradeable::get_program_data_address(program_id) {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    if buffer.key != new_contract {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    let instruction =
        bpf_loader_upgradeable::upgrade(program_id, new_contract, upgrade_authority.key, spill.key);
    // `invoke_signed` takes owned `AccountInfo`s; the clone is `Rc` refcount bumps, no data copy.
    invoke_signed(
        &instruction,
        &[
            program_data.clone(),
            program.clone(),
            buffer.clone(),
            spill.clone(),
            rent.clone(),
            clock.clone(),
            upgrade_authority.clone(),
        ],
        &[&[UPGRADE_AUTHORITY_SEED_PREFIX, &[authority_bump]]],
    )
}
