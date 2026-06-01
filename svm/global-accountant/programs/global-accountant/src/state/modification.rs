//! Zero-copy store helper for `ModificationLogLayout`.
//!
//! The `modify_balance` governance instruction writes a per-payload-sequence
//! `ModificationLog` PDA recording the full modification fields for on-chain
//! queryability. Mirrors the CosmWasm `MODIFICATIONS: Map<u64, Modification>`
//! storage; the existence of this PDA at the canonical seed is what enforces
//! replay protection on the governance path.
//!
//! Only a `store` helper is needed today — no production code path loads the
//! layout back out. Off-chain indexers read it via getAccountInfo. A `load`
//! companion can be added if a future code path needs it.

use pinocchio::{error::ProgramError, AccountView};

use crate::definitions::{GlobalAccountantError, ModificationLogLayout};
use crate::err;

/// Write a [`ModificationLogLayout`] into the account's data buffer. Caller
/// is responsible for verifying the account's canonical address and having
/// already allocated the account to `LEN` bytes via `init_or_upgrade_pda`.
pub fn store(
    account: &mut AccountView,
    value: &ModificationLogLayout,
) -> Result<(), ProgramError> {
    let mut data = account.try_borrow_mut()?;
    if data.len() != ModificationLogLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    data.copy_from_slice(bytemuck::bytes_of(value));
    Ok(())
}
