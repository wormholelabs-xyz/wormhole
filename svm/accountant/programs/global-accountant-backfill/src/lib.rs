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
//!   `accountant_operational_core::support::pda_init`; `#[account(init)]` fails on a
//!   prefunded PDA.
//! - Errors map to `ProgramError::Custom(code)`; `#[error_code]` would add Anchor's `+6000` offset.
//!
//! `declare_id!` pins the program to the fixed address the mollusk/surfpool fixtures deploy at
//! (`Pubkey::new_from_array([8u8; 32])`, base58 `YMN9Qj5jPNp7j14VPcML1B6xGgcPWVZUGLFU3Mnyfaf`).

#![allow(unexpected_cfgs)]

use accountant_operational_core::flatten_accounts;
use anchor_lang::prelude::*;

pub mod contexts;

pub use accountant_operational_core::{definitions, err, raw_ix_data::RawIxData};
pub use global_accountant_definitions;

// `#[program]`'s codegen expects `#[derive(Accounts)]`'s companion items at
// the crate root; re-export `contexts::*` to place them there.
pub use contexts::*;

declare_id!("YMN9Qj5jPNp7j14VPcML1B6xGgcPWVZUGLFU3Mnyfaf");

/// Wire discriminator, defined in `global_accountant_definitions::BackfillInstruction`.
/// Re-exported for off-chain callers building raw transactions.
pub use global_accountant_definitions::BackfillInstruction as Instruction;

/// Pubkey that must sign every backfill ix. Replace before mainnet deploy;
/// this default is the test keypair `Keypair::new_from_array([1u8; 32])`,
/// guarded by `tests/artifact_authority_check.rs`.
pub const BACKFILL_AUTHORITY: [u8; 32] = [
    0x8a, 0x88, 0xe3, 0xdd, 0x74, 0x09, 0xf1, 0x95, 0xfd, 0x52, 0xdb, 0x2d, 0x3c, 0xba, 0x5d, 0x72,
    0xca, 0x67, 0x09, 0xbf, 0x1d, 0x94, 0x12, 0x1b, 0xf3, 0x74, 0x88, 0x01, 0xb4, 0x0f, 0x6f, 0x5c,
];

#[program]
pub mod global_accountant_backfill {
    use super::*;

    /// See `accountant_operational_core::instructions::backfill_noreplay`.
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
        accountant_operational_core::instructions::backfill_noreplay::process(
            ctx.program_id,
            &accounts,
            &ix_data.0,
            &BACKFILL_AUTHORITY,
        )?;
        Ok(())
    }

    /// See `accountant_operational_core::instructions::backfill_balance`.
    #[instruction(discriminator = 1)]
    pub fn backfill_balance<'info>(
        ctx: Context<'info, BackfillBalanceAccounts<'info>>,
        ix_data: RawIxData,
    ) -> Result<()> {
        let accounts = flatten_accounts!(ctx, [payer, system_program], remaining);
        accountant_operational_core::instructions::backfill_balance::process(
            ctx.program_id,
            &accounts,
            &ix_data.0,
            &BACKFILL_AUTHORITY,
        )?;
        Ok(())
    }
}
