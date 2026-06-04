//! Shared PDA initialisation helper used by `open_digest::open_digest_inner`
//! (and therefore every DigestAccount-open path) and by
//! `submit_observations`'s pending-PDA allocation.
//!
//! Defends against the dust-DoS grief vector: an attacker can
//! `system_program::transfer(1)` to the canonical PDA address before the
//! legitimate open. A naive `CreateAccount` CPI then fails ("account already
//! in use") and the key is effectively bricked. Three branches:
//!
//! 1. Empty + zero-lamport + system-owned -> `CreateAccount` (fast path).
//! 2. Pre-funded (lamports > 0) + system-owned + data-empty -> Transfer (top
//!    up to rent-exempt minimum if short) + Allocate + Assign. Equivalent to
//!    `pinocchio_system::create_account_with_minimum_balance_signed` but with
//!    an explicit owner check so we return our own `InvalidPda` instead of
//!    letting the system program error surface.
//! 3. Anything else (already-initialised, foreign owner) -> `InvalidPda`.

use pinocchio::{
    cpi::Signer,
    sysvars::{rent::Rent, Sysvar},
    AccountView, Address, ProgramResult,
};
use pinocchio_system::instructions::{Allocate, Assign, CreateAccount, Transfer};

use crate::definitions::GlobalAccountantError;
use crate::err;

pub fn init_or_upgrade_pda(
    payer: &AccountView,
    pda: &AccountView,
    program_id: &Address,
    signer: Signer,
    space: u64,
) -> ProgramResult {
    let rent_exempt_minimum = Rent::get()?.try_minimum_balance(space as usize)?;
    let initial_lamports = pda.lamports();
    let initial_data_len = pda.data_len();
    let initial_owner_is_system = pda.owner() == &pinocchio_system::ID;

    if initial_data_len != 0 || !initial_owner_is_system {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    if initial_lamports == 0 {
        CreateAccount {
            from: payer,
            to: pda,
            lamports: rent_exempt_minimum,
            space,
            owner: program_id,
        }
        .invoke_signed(core::slice::from_ref(&signer))?;
    } else {
        // Over-funded PDA is accepted as a gift to the protocol; `saturating_sub`
        // keeps `top_up` at 0 rather than aborting on the negative delta.
        let top_up = rent_exempt_minimum.saturating_sub(initial_lamports);
        if top_up > 0 {
            Transfer {
                from: payer,
                to: pda,
                lamports: top_up,
            }
            .invoke()?;
        }
        Allocate {
            account: pda,
            space,
        }
        .invoke_signed(core::slice::from_ref(&signer))?;
        Assign {
            account: pda,
            owner: program_id,
        }
        .invoke_signed(core::slice::from_ref(&signer))?;
    }

    Ok(())
}
