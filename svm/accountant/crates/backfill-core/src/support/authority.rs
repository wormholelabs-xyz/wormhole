//! Signer check against an expected pubkey.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::{err, ProgramResult};

use crate::definitions::GlobalAccountantError;

/// Reject unless `payer` signed the tx and its pubkey equals `expected`.
#[inline]
pub fn require_authority(payer: &AccountInfo, expected: &[u8; 32]) -> ProgramResult {
    if !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if payer.key.to_bytes() != *expected {
        return Err(err(GlobalAccountantError::UnauthorizedCaller));
    }
    Ok(())
}
