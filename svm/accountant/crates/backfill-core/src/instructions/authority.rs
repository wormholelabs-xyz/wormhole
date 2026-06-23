//! Constant-time signer check against the compile-time
//! [`crate::BACKFILL_AUTHORITY`] pubkey.

use pinocchio::{error::ProgramError, AccountView, ProgramResult};

use crate::{err, BackfillError, BACKFILL_AUTHORITY};

/// Reject the instruction unless `payer` both signed the tx AND its pubkey
/// matches [`BACKFILL_AUTHORITY`] byte-for-byte.
#[inline]
pub fn require_authority(payer: &AccountView) -> ProgramResult {
    if !payer.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if payer.address().as_array() != &BACKFILL_AUTHORITY {
        return Err(err(BackfillError::UnauthorizedCaller));
    }
    Ok(())
}
