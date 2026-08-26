//! PDA creation through `CreateAccountAllowPrefund` (SIMD-0312).
//!
//! SECURITY: plain `CreateAccount` fails on a prefunded address. An attacker could block a
//! PDA with dust. `CreateAccountAllowPrefund` tops up `rent_minimum.saturating_sub(pda.lamports())`
//! and succeeds at any starting balance. Anchor `#[account(init)]` uses `CreateAccount`;
//! do not replace this helper with it.

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
