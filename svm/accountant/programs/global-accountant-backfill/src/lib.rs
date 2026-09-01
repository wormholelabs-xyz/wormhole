//! Wormhole Global Accountant Backfill, anchor-lang 1.1.2.
//!
//! One-shot migration `.so` that seeds NoReplay bits and Balance PDAs from a
//! wormchain `query_all_accounts` snapshot, then is upgraded out via
//! `solana program upgrade` once `global-accountant` takes over.
//!
//! Wire-format constraints; keep these when you change the program:
//!
//! - Account discriminator is the 1-byte `AccountTag` at offset 0. Load state with
//!   `UncheckedAccount` + `bytemuck`; do not use `#[account(zero_copy)]`.
//! - Instruction discriminator is 1 byte (`0`/`1`) through `#[instruction(discriminator = N)]`.
//! - PDA creation uses `CreateAccountAllowPrefund` through
//!   `accountant_backfill_core::instructions::pda_init`; `#[account(init)]` fails on a
//!   prefunded PDA.
//! - Errors map to `ProgramError::Custom(code)`; `#[error_code]` would add Anchor's `+6000` offset.
//!
//! `declare_id!` pins the program to the fixed address the mollusk/surfpool fixtures deploy at
//! (`Pubkey::new_from_array([8u8; 32])`, base58 `YMN9Qj5jPNp7j14VPcML1B6xGgcPWVZUGLFU3Mnyfaf`).

#![allow(unexpected_cfgs)]

use anchor_lang::prelude::*;

pub mod contexts;
pub mod instructions;
pub mod raw_ix_data;

pub use accountant_backfill_core::{definitions, err, BackfillError, BACKFILL_AUTHORITY};
pub use global_accountant_definitions;

// `#[derive(Accounts)]`'s macro-generated companion items (e.g.
// `__client_accounts_backfill_no_replay_accounts`) land in `crate::contexts::`.
// The `#[program]` macro's codegen expects them at the crate root, so
// re-export the whole module.
pub use contexts::*;
use raw_ix_data::RawIxData;

declare_id!("YMN9Qj5jPNp7j14VPcML1B6xGgcPWVZUGLFU3Mnyfaf");

/// Instruction discriminators. Single-byte prefix on instruction data,
/// mirrored by `#[instruction(discriminator = N)]` on the handlers below.
/// Exposed as a convenience for off-chain callers building raw transactions;
/// Anchor's own generated dispatch drives on-chain routing.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Instruction {
    BackfillNoReplay = 0,
    BackfillBalance = 1,
}

impl Instruction {
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::BackfillNoReplay),
            1 => Some(Self::BackfillBalance),
            _ => None,
        }
    }
}

/// Flatten a `#[derive(Accounts)]` struct's fixed fields plus the variadic
/// `remaining_accounts` tail into a positionally ordered `Vec<AccountInfo>`,
/// matching the original pinocchio slice order that
/// `accountant_backfill_core::instructions::*::process` expects.
macro_rules! flatten_accounts {
    ($ctx:expr, [$($field:ident),+ $(,)?]) => {{
        let mut accounts: Vec<AccountInfo> =
            vec![$($ctx.accounts.$field.to_account_info()),+];
        accounts.extend($ctx.remaining_accounts.iter().cloned());
        accounts
    }};
}

#[program]
pub mod global_accountant_backfill {
    use super::*;

    /// Dispatch discriminator 0. See
    /// `accountant_backfill_core::instructions::backfill_noreplay`.
    ///
    /// The explicit `'info` lifetime is required: combining
    /// `ctx.accounts.*.to_account_info()` with
    /// `ctx.remaining_accounts.iter().cloned()` in one `Vec<AccountInfo>`
    /// type-checks only when both draw from the same named lifetime —
    /// `Signer` is invariant over `'info`, so two elided lifetimes fail to
    /// unify.
    #[instruction(discriminator = 0)]
    pub fn backfill_no_replay<'info>(
        ctx: Context<'info, BackfillNoReplayAccounts<'info>>,
        ix_data: RawIxData,
    ) -> Result<()> {
        let accounts = flatten_accounts!(
            ctx,
            [payer, noreplay_program, noreplay_authority, system_program]
        );
        accountant_backfill_core::instructions::backfill_noreplay::process(
            ctx.program_id,
            &accounts,
            &ix_data.0,
        )?;
        Ok(())
    }

    /// Dispatch discriminator 1. See
    /// `accountant_backfill_core::instructions::backfill_balance`.
    #[instruction(discriminator = 1)]
    pub fn backfill_balance<'info>(
        ctx: Context<'info, BackfillBalanceAccounts<'info>>,
        ix_data: RawIxData,
    ) -> Result<()> {
        let accounts = flatten_accounts!(ctx, [payer, system_program]);
        accountant_backfill_core::instructions::backfill_balance::process(
            ctx.program_id,
            &accounts,
            &ix_data.0,
        )?;
        Ok(())
    }
}
