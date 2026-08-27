//! Verify VAA Shim `VerifyHash` CPI. All signed-VAA handlers route through [`verify_vaa`].

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke;

use crate::definitions::{VerifyHashData, VERIFY_VAA_SHIM_PROGRAM_ID};
use crate::ProgramResult;

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
