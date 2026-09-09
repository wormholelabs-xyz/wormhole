//! `#[derive(Accounts)]` contexts for the two backfill instructions. Accounts
//! are bare `Signer`/`UncheckedAccount`; handlers in
//! `accountant_operational_core` validate them by hand, since bulk PDA seeds
//! derive from the parsed payload. Variadic bucket/balance PDAs ride in
//! `ctx.remaining_accounts`.

use anchor_lang::prelude::*;

/// Fixed accounts for `BackfillNoReplay`: 4 accounts plus variadic bucket
/// PDAs in `ctx.remaining_accounts`.
#[derive(Accounts)]
pub struct BackfillNoReplayAccounts<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: do not use this account's address as the CPI target; use `NOREPLAY_PROGRAM_ID`.
    pub noreplay_program: UncheckedAccount<'info>,
    /// CHECK: NoReplay authority PDA, re-derived in `accountant_backfill_core::cpi::noreplay::mark_used_bulk`.
    pub noreplay_authority: UncheckedAccount<'info>,
    /// CHECK: System Program, passed through positionally to the NoReplay CPI.
    pub system_program: UncheckedAccount<'info>,
}

/// Fixed accounts for `BackfillBalance`: 2 accounts plus variadic balance
/// PDAs in `ctx.remaining_accounts`.
#[derive(Accounts)]
pub struct BackfillBalanceAccounts<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: System Program, required by `pda_init::create_pda_allow_prefund`'s CPI.
    pub system_program: UncheckedAccount<'info>,
}

/// Fixed accounts for `BackfillModifyBalance`: 2 accounts plus variadic record
/// PDAs in `ctx.remaining_accounts`.
#[derive(Accounts)]
pub struct BackfillModifyBalanceAccounts<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: System Program, required by `pda_init::create_pda_allow_prefund`'s CPI.
    pub system_program: UncheckedAccount<'info>,
}
