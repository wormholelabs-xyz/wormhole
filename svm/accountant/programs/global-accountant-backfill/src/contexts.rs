//! `#[derive(Accounts)]` context structs for the two backfill instructions.
//!
//! Every account is a bare `Signer`/`UncheckedAccount`, validated by hand
//! inside the handler bodies in `accountant_backfill_core`. The
//! bulk-batched PDA seeds depend on the parsed instruction payload rather
//! than a fixed account field, which is why validation happens in the
//! handler rather than as an Anchor `owner`/`seeds`/`address` constraint.
//! Anchor here provides `Context`/arity/discriminator ergonomics.
//!
//! Only the fixed-position accounts are named fields; the variadic
//! bucket/balance-PDA tails ride in `ctx.remaining_accounts`, preserving
//! the on-wire account-meta order the pre-migration pinocchio slice relied
//! on.

use anchor_lang::prelude::*;

/// Fixed accounts for `BackfillNoReplay`: 4 accounts plus variadic bucket
/// PDAs in `ctx.remaining_accounts`.
#[derive(Accounts)]
pub struct BackfillNoReplayAccounts<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: NoReplay program; the CPI target is always the hardcoded
    /// `NOREPLAY_PROGRAM_ID` constant, never this account's address.
    pub noreplay_program: UncheckedAccount<'info>,
    /// CHECK: this program's NoReplay authority PDA; address re-derived in
    /// `accountant_backfill_core::instructions::noreplay::mark_used_bulk`.
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
    /// CHECK: System Program, required at the tx wire level for
    /// `pda_init::init_or_upgrade_pda`'s `CreateAccountAllowPrefund` CPI.
    pub system_program: UncheckedAccount<'info>,
}
