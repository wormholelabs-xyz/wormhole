//! Verify VAA Shim `VerifyHash` CPI. All signed-VAA handlers route through [`verify_vaa`].

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke;

use crate::definitions::{VerifyHashData, VERIFY_VAA_SHIM_PROGRAM_ID};
use crate::support::guardian_set;
use crate::{ProgramCoreResult, ProgramResult};

/// Check `digest` through the Shim's `VerifyHash`.
///
/// The Shim authenticates both accounts: `guardian_signatures` by owner, `guardian_set`
/// by Core Bridge PDA address. It then checks set expiry, quorum, and each signature.
///
/// SECURITY: the CPI target is the constant `VERIFY_VAA_SHIM_PROGRAM_ID`, never a
/// caller-supplied account.
pub fn verify_vaa<'info>(
    guardian_set: &AccountInfo<'info>,
    guardian_signatures: &AccountInfo<'info>,
    digest: &[u8; 32],
    guardian_set_bump: u8,
) -> ProgramResult {
    let shim_program_id = Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID);

    let ix_data = VerifyHashData::new(guardian_set_bump, *digest);

    let ix_accounts = vec![
        AccountMeta::new_readonly(*guardian_set.key, false),
        AccountMeta::new_readonly(*guardian_signatures.key, false),
    ];

    let instruction = Instruction {
        program_id: shim_program_id,
        accounts: ix_accounts,
        data: ix_data.as_bytes().to_vec(),
    };

    invoke(
        &instruction,
        &[guardian_set.clone(), guardian_signatures.clone()],
    )
}

/// [`verify_vaa`], then the signing set's index for the commit log.
///
/// SECURITY: the index comes from the account after the Shim CPI validated it;
/// `verify_account` then proves the account is the canonical Core Bridge PDA for that index.
#[inline(always)]
pub fn verify_vaa_and_read_index<'info>(
    guardian_set: &AccountInfo<'info>,
    guardian_signatures: &AccountInfo<'info>,
    digest: &[u8; 32],
    guardian_set_bump: u8,
) -> ProgramCoreResult<u32> {
    verify_vaa(guardian_set, guardian_signatures, digest, guardian_set_bump)?;
    let guardian_set_index = guardian_set::read_index(guardian_set)?;
    guardian_set::verify_account(guardian_set, guardian_set_index)?;
    Ok(guardian_set_index)
}
