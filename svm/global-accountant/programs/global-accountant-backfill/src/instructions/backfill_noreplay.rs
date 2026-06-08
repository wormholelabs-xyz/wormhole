//! `BackfillNoReplay` — bulk flip `solana-noreplay` bits for a batch of
//! `(chain, emitter, sequence, digest)` entries and emit one canonical
//! `ACCDGST\0` commit-log per entry.
//!
//! Stub. Implementation follows in the next commit, after the failing test pins
//! the contract.

use pinocchio::{AccountView, Address, ProgramResult};

use crate::{err, BackfillError};

pub fn process(_program_id: &Address, _accounts: &mut [AccountView], _data: &[u8]) -> ProgramResult {
    Err(err(BackfillError::InvalidInstruction))
}
