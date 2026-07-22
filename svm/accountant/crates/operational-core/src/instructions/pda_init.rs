//! Shared PDA initialisation helper.
//!
//! Defends against the dust-DoS grief vector: a not-yet-created PDA is off the
//! ed25519 curve, so the only thing an attacker can do to its address is send
//! lamports. A naive `CreateAccount` fails on any prefunded balance, so this
//! uses the system program's `CreateAccountAllowPrefund` (SIMD-0312) instead,
//! which allocates + assigns + (optionally) tops up in a single CPI regardless
//! of the starting balance. The top-up is computed as
//! `rent_minimum.saturating_sub(pda.lamports())`, so an under-, exactly-, or
//! over-funded address all converge on a correctly rent-exempt program account.
//!
//! No Anchor constraint models this (`#[account(init)]` uses plain
//! `CreateAccount`, which fails outright on a prefunded PDA — migration plan
//! §2d/§7.2), so this stays a manual CPI built directly against
//! `solana-system-interface`'s `create_account_allow_prefund` rather than any
//! Anchor account-initialisation sugar.
//!
//! The caller (`account::init_if_needed`) short-circuits an already-initialised
//! PDA; the explicit guard here surfaces a clean `InvalidPda` for the remaining
//! "not system-owned / not empty" cases rather than a downstream system error.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program::invoke_signed;

use crate::definitions::GlobalAccountantError;
use crate::{err, ProgramResult};

pub fn init_or_upgrade_pda<'info>(
    payer: &AccountInfo<'info>,
    pda: &AccountInfo<'info>,
    program_id: &Pubkey,
    seeds: &[&[u8]],
    space: u64,
) -> ProgramResult {
    if pda.data_len() != 0 || pda.owner != &anchor_lang::solana_program::system_program::ID {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    let rent = Rent::get()?;
    let minimum_balance = rent.minimum_balance(space as usize);
    let top_up = minimum_balance.saturating_sub(pda.lamports());

    let ix = if top_up > 0 {
        solana_system_interface::instruction::create_account_allow_prefund(
            pda.key,
            Some((payer.key, top_up)),
            space,
            program_id,
        )
    } else {
        solana_system_interface::instruction::create_account_allow_prefund(
            pda.key, None, space, program_id,
        )
    };

    let account_infos: Vec<AccountInfo> = if top_up > 0 {
        vec![pda.clone(), payer.clone()]
    } else {
        vec![pda.clone()]
    };

    invoke_signed(&ix, &account_infos, &[seeds])?;

    Ok(())
}
