//! Shared PDA init helper. Mirror of the operational program's
//! `instructions::pda_init` (same dust-DoS handling). Duplicated for the
//! reasons documented in `commit_log.rs`.

use pinocchio::{
    cpi::Signer,
    sysvars::{rent::Rent, Sysvar},
    AccountView, Address, ProgramResult,
};
use pinocchio_system::instructions::{Allocate, Assign, CreateAccount, Transfer};

use crate::{err, BackfillError};

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
        return Err(err(BackfillError::InvalidPda));
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
