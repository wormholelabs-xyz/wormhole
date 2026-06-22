//! Constant-time signer check against the program's operator authority pubkey.
//!
//! The expected authority is supplied by the program shell — each backfill
//! program owns its own `BACKFILL_AUTHORITY` const — so this core stays
//! program-agnostic.

use pinocchio::{error::ProgramError, AccountView, ProgramResult};

use crate::definitions::Pubkey;
use crate::{err, BackfillError};

/// Reject the instruction unless `payer` both signed the tx AND its pubkey
/// matches `expected` byte-for-byte.
#[inline]
pub(crate) fn require_authority(payer: &AccountView, expected: &Pubkey) -> ProgramResult {
    if !payer.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if payer.address().as_array() != expected {
        return Err(err(BackfillError::UnauthorizedCaller));
    }
    Ok(())
}
