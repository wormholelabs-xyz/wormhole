//! Constant-time signer check against the compile-time
//! [`crate::BACKFILL_AUTHORITY`] pubkey.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use crate::{err, BackfillError, ProgramResult, BACKFILL_AUTHORITY};

/// Reject unless `payer` signed the tx and its pubkey matches
/// [`BACKFILL_AUTHORITY`] byte-for-byte.
#[inline]
pub fn require_authority(payer: &AccountInfo) -> ProgramResult {
    if !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if payer.key.to_bytes() != BACKFILL_AUTHORITY {
        return Err(err(BackfillError::UnauthorizedCaller));
    }
    Ok(())
}
