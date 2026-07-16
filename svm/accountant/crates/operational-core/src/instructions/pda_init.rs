//! Shared PDA initialisation helper.
//!
//! Defends against the dust-DoS grief vector: a not-yet-created PDA is off the
//! ed25519 curve, so the only thing an attacker can do to its address is send
//! lamports. A naive `CreateAccount` fails on any prefunded balance, so this
//! uses the system program's `CreateAccountAllowPrefund` (SIMD-0312) instead,
//! which allocates + assigns + (optionally) tops up in a single CPI regardless
//! of the starting balance. `with_minimum_balance` computes the top-up as
//! `rent_minimum.saturating_sub(pda.lamports())`, so an under-, exactly-, or
//! over-funded address all converge on a correctly rent-exempt program account.
//!
//! The caller (`account::init_if_needed`) short-circuits an already-initialised
//! PDA; the explicit guard here surfaces a clean `InvalidPda` for the remaining
//! "not system-owned / not empty" cases rather than a downstream system error.

use pinocchio::{cpi::Signer, AccountView, Address, ProgramResult};
use pinocchio_system::instructions::CreateAccountAllowPrefund;

use crate::definitions::GlobalAccountantError;
use crate::err;

pub fn init_or_upgrade_pda(
    payer: &AccountView,
    pda: &AccountView,
    program_id: &Address,
    signer: Signer,
    space: u64,
) -> ProgramResult {
    if pda.data_len() != 0 || pda.owner() != &pinocchio_system::ID {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    // `None` rent sysvar => the helper fetches `Rent` via syscall and derives
    // the funding top-up itself; passing 0-topup when already prefunded.
    CreateAccountAllowPrefund::with_minimum_balance(payer, pda, space, program_id, None)?
        .invoke_signed(core::slice::from_ref(&signer))?;

    Ok(())
}
