//! `Retire` — flip the backfill authority's `retired` flag. Belt-and-braces
//! kill switch before `solana program upgrade` to the operational program.
//!
//! Stub.

use pinocchio::{AccountView, Address, ProgramResult};

use crate::{err, BackfillError};

pub fn process(_program_id: &Address, _accounts: &mut [AccountView], _data: &[u8]) -> ProgramResult {
    Err(err(BackfillError::InvalidInstruction))
}
