//! `#[derive(Accounts)]` contexts for the backfill instructions. Accounts are bare
//! `Signer`/`UncheckedAccount`; the handlers in `accountant_backfill_core` validate them by
//! hand, since bulk PDA seeds derive from the parsed payload. Variadic bucket, balance and
//! record PDAs ride in `ctx.remaining_accounts`.

use anchor_lang::prelude::*;

/// Fixed accounts for `BackfillNoReplay`: 4 accounts plus variadic bucket PDAs in
/// `ctx.remaining_accounts`.
#[derive(Accounts)]
pub struct BackfillNoReplayAccounts<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: kept in the account list so the runtime loads the NoReplay program; the CPI
    /// target is `NOREPLAY_PROGRAM_ID`, applied in `backfill_core::cpi::noreplay::mark_used_bulk`.
    pub noreplay_program: UncheckedAccount<'info>,
    /// CHECK: NoReplay authority PDA, re-derived in `backfill_core::cpi::noreplay::mark_used_bulk`.
    pub noreplay_authority: UncheckedAccount<'info>,
    /// CHECK: System Program, passed through positionally to the NoReplay CPI.
    pub system_program: UncheckedAccount<'info>,
}

/// Fixed accounts for `BackfillBalance`: 2 accounts plus variadic balance PDAs in
/// `ctx.remaining_accounts`.
#[derive(Accounts)]
pub struct BackfillBalanceAccounts<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: System Program, required by `pda_init::create_pda_allow_prefund`'s CPI.
    pub system_program: UncheckedAccount<'info>,
}

/// Fixed accounts for `BackfillModifyBalance`: 2 accounts plus variadic record PDAs in
/// `ctx.remaining_accounts`.
#[derive(Accounts)]
pub struct BackfillModifyBalanceAccounts<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: System Program, required by `pda_init::create_pda_allow_prefund`'s CPI.
    pub system_program: UncheckedAccount<'info>,
}

/// Fixed accounts for `BackfillRelayerChainRegistration`: 2 accounts plus, per entry, a
/// `ChainRegistration` PDA and a `RegisterChain` PDA in `ctx.remaining_accounts`.
#[derive(Accounts)]
pub struct BackfillRelayerChainRegistrationAccounts<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: System Program, required by `pda_init::create_pda_allow_prefund`'s CPI.
    pub system_program: UncheckedAccount<'info>,
}

/// Fixed accounts for `BackfillTransceiverHub`: 2 accounts plus one `TransceiverHub` PDA per
/// entry in `ctx.remaining_accounts`.
#[derive(Accounts)]
pub struct BackfillTransceiverHubAccounts<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: System Program, required by `pda_init::create_pda_allow_prefund`'s CPI.
    pub system_program: UncheckedAccount<'info>,
}
