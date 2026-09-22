//! Wormhole Global Accountant (WTT) Backfill Solana program, anchor-lang 1.1.2.
//!
//! One-shot migration `.so` that seeds NoReplay bits, Balance PDAs, ModifyBalance records and
//! ChainRegistration state from a wormchain `query_all_accounts` snapshot. It occupies the
//! operational program's account and is upgraded out via `solana program upgrade` once
//! `global-accountant` takes over.
//!
//! Wire-format constraints; keep these when you change the program:
//!
//! - Account discriminator is the 1-byte `AccountTag` at offset 0. Load state with
//!   `UncheckedAccount` + `bytemuck`; do not use `#[account(zero_copy)]`.
//! - Instruction discriminator is 1 byte (`0..=3`) through `#[instruction(discriminator = N)]`.
//! - PDA creation uses `CreateAccountAllowPrefund` through `pda_init`; `#[account(init)]`
//!   fails on a prefunded PDA.
//! - Errors map to `ProgramError::Custom(code)`; `#[error_code]` would add Anchor's `+6000` offset.
//!
//! `declare_id!` pins this `.so` to the operational program's address, so the two share one
//! program account across the upgrade.
//!
//! Migration-window program: remove this crate once the cutover to `global-accountant` is
//! complete.

#![allow(unexpected_cfgs)]

use anchor_lang::prelude::*;

pub mod contexts;
pub mod raw_ix_data;

pub use accountant_operational_core::err;
pub use global_accountant_definitions as definitions;

use accountant_backfill_core::instructions as backfill;
use definitions::{pubkey_eq, GLOBAL_ACCOUNTANT_PROGRAM_ID};

// `#[program]` codegen expects the `#[derive(Accounts)]` companion items at the crate root.
pub use contexts::*;
use raw_ix_data::RawIxData;

/// Wire discriminator, defined in `definitions::global_accountant_backfill::Instruction`.
/// Re-exported for off-chain callers building raw transactions.
pub use definitions::global_accountant_backfill::Instruction;

declare_id!("US517G5965aydkZ46HS38QLi7UQiSojurfbQfKCELFx");
const _: () = assert!(
    pubkey_eq(&ID.to_bytes(), &GLOBAL_ACCOUNTANT_PROGRAM_ID),
    "declare_id! does not match GLOBAL_ACCOUNTANT_PROGRAM_ID"
);

/// Pubkey that must sign every backfill instruction, from `BACKFILL_AUTHORITY` at compile
/// time. `env!` makes a missing variable a build error, so every artifact names an operator
/// key explicitly. Set per deploy in `justfile`. Read the key back out of a built `.so` with
/// `just verify-authority <so-path> <base58-pubkey>`.
pub const BACKFILL_AUTHORITY: [u8; 32] =
    const_crypto::bs58::decode_pubkey(env!("BACKFILL_AUTHORITY"));

/// Flatten an `Accounts` struct into the positional `Vec<AccountInfo>` the handlers take.
/// Field order must match the handler's account list. The `remaining` form appends
/// `ctx.remaining_accounts`, which carry the variadic PDAs.
macro_rules! flatten_accounts {
    ($accounts:expr, [$($field:ident),+ $(,)?]) => {
        vec![$($accounts.$field.to_account_info()),+]
    };
    ($ctx:expr, [$($field:ident),+ $(,)?], remaining) => {{
        let mut accounts: Vec<AccountInfo> =
            vec![$($ctx.accounts.$field.to_account_info()),+];
        accounts.extend($ctx.remaining_accounts.iter().cloned());
        accounts
    }};
}

#[program]
pub mod global_accountant_backfill {
    use super::*;

    /// See `accountant_backfill_core::instructions::backfill_noreplay`.
    ///
    /// Explicit `'info`: unifies `to_account_info()` and
    /// `remaining_accounts.iter().cloned()` in one `Vec<AccountInfo>`.
    #[instruction(discriminator = 0)]
    pub fn backfill_no_replay<'info>(
        ctx: Context<'info, BackfillNoReplayAccounts<'info>>,
        ix_data: RawIxData,
    ) -> Result<()> {
        let accounts = flatten_accounts!(
            ctx,
            [payer, noreplay_program, noreplay_authority, system_program],
            remaining
        );
        backfill::backfill_noreplay::process(
            ctx.program_id,
            &accounts,
            &ix_data.0,
            &BACKFILL_AUTHORITY,
        )?;
        Ok(())
    }

    /// See `accountant_backfill_core::instructions::backfill_balance`.
    #[instruction(discriminator = 1)]
    pub fn backfill_balance<'info>(
        ctx: Context<'info, BackfillBalanceAccounts<'info>>,
        ix_data: RawIxData,
    ) -> Result<()> {
        let accounts = flatten_accounts!(ctx, [payer, system_program], remaining);
        backfill::backfill_balance::process(
            ctx.program_id,
            &accounts,
            &ix_data.0,
            &BACKFILL_AUTHORITY,
        )?;
        Ok(())
    }

    /// See `accountant_backfill_core::instructions::backfill_modify_balance`.
    #[instruction(discriminator = 2)]
    pub fn backfill_modify_balance<'info>(
        ctx: Context<'info, BackfillModifyBalanceAccounts<'info>>,
        ix_data: RawIxData,
    ) -> Result<()> {
        let accounts = flatten_accounts!(ctx, [payer, system_program], remaining);
        backfill::backfill_modify_balance::process(
            ctx.program_id,
            &accounts,
            &ix_data.0,
            &BACKFILL_AUTHORITY,
        )?;
        Ok(())
    }

    /// See `accountant_backfill_core::instructions::backfill_chain_registration`.
    #[instruction(discriminator = 3)]
    pub fn backfill_chain_registration<'info>(
        ctx: Context<'info, BackfillChainRegistrationAccounts<'info>>,
        ix_data: RawIxData,
    ) -> Result<()> {
        let accounts = flatten_accounts!(ctx, [payer, system_program], remaining);
        backfill::backfill_chain_registration::process(
            ctx.program_id,
            &accounts,
            &ix_data.0,
            &BACKFILL_AUTHORITY,
        )?;
        Ok(())
    }
}
