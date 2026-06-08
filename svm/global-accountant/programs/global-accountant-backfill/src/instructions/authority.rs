//! Backfill authority PDA: gating helper. Stub.

use pinocchio::{AccountView, Address, ProgramResult};

use crate::{err, BackfillError};

/// Verify (or lazy-init) the backfill authority PDA. Implementation follows.
pub fn require_authority(
    _program_id: &Address,
    _payer: &AccountView,
    _authority_pda: &AccountView,
    _system_program: &AccountView,
) -> ProgramResult {
    Err(err(BackfillError::InvalidInstruction))
}
