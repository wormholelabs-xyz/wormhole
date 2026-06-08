//! `BackfillBalance` — write a single `BalanceAccountLayout` PDA directly from
//! a wormchain `query_all_accounts` row. No VAA verification; auditability is
//! transitive via the `BackfillNoReplay` commit-log corpus.
//!
//! Stub.

use pinocchio::{AccountView, Address, ProgramResult};

use crate::{err, BackfillError};

pub fn process(_program_id: &Address, _accounts: &mut [AccountView], _data: &[u8]) -> ProgramResult {
    Err(err(BackfillError::InvalidInstruction))
}
