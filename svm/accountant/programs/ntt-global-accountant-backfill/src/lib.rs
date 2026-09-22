//! Wormhole NTT Global Accountant Backfill Solana program, anchor-lang 1.1.2.
//!
//! One-shot migration `.so` that seeds NoReplay bits, Balance PDAs, ModifyBalance records,
//! relayer `ChainRegistration` state and the `TransceiverHub` and `TransceiverPeer` maps from
//! a wormchain `query_all_accounts` snapshot. It occupies the NTT operational program's
//! account and is upgraded out via `solana program upgrade` once `ntt-global-accountant`
//! takes over.
//!
//! Every handler is shared with the WTT backfill through `accountant-backfill-core`; only the
//! program id and the operator authority differ, so the bytes written here are the bytes the
//! NTT operational program writes.
//!
//! Wire-format constraints; keep these when you change the program:
//!
//! - Account discriminator is the 1-byte `AccountTag` at offset 0. Load state with
//!   `UncheckedAccount` + `bytemuck`; do not use `#[account(zero_copy)]`.
//! - Instruction discriminator is 1 byte (`0..=5`) through `#[instruction(discriminator = N)]`.
//! - PDA creation uses `CreateAccountAllowPrefund` through `pda_init`; `#[account(init)]`
//!   fails on a prefunded PDA.
//! - Errors map to `ProgramError::Custom(code)`; `#[error_code]` would add Anchor's `+6000` offset.
//!
//! `declare_id!` pins this `.so` to the NTT operational program's address, so the two share one
//! program account across the upgrade.

#![allow(unexpected_cfgs)]

use anchor_lang::prelude::*;

pub mod contexts;
pub mod raw_ix_data;

pub use accountant_operational_core::err;
pub use global_accountant_definitions as definitions;

use accountant_backfill_core::instructions as backfill;
use definitions::{pubkey_eq, NTT_GLOBAL_ACCOUNTANT_PROGRAM_ID};

// `#[program]` codegen expects the `#[derive(Accounts)]` companion items at the crate root.
pub use contexts::*;
use raw_ix_data::RawIxData;

/// Wire discriminator, defined in `definitions::ntt_global_accountant_backfill::Instruction`.
/// Re-exported for off-chain callers building raw transactions.
pub use definitions::ntt_global_accountant_backfill::Instruction;

declare_id!("cGfHiC6Kgg3FpFZvgwGcswsCRtp4aBP2fzuXRQPizuN");
const _: () = assert!(
    pubkey_eq(&ID.to_bytes(), &NTT_GLOBAL_ACCOUNTANT_PROGRAM_ID),
    "declare_id! does not match NTT_GLOBAL_ACCOUNTANT_PROGRAM_ID"
);

/// Pubkey that must sign every backfill instruction, from `NTT_BACKFILL_AUTHORITY` at compile
/// time. `env!` makes a missing variable a build error, so every artifact names an operator
/// key explicitly. Set per deploy in `justfile`. Distinct from the WTT backfill's
/// `BACKFILL_AUTHORITY`: the two migrations run under their own keys.
pub const NTT_BACKFILL_AUTHORITY: [u8; 32] =
    const_crypto::bs58::decode_pubkey(env!("NTT_BACKFILL_AUTHORITY"));

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
pub mod ntt_global_accountant_backfill {
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
            &NTT_BACKFILL_AUTHORITY,
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
            &NTT_BACKFILL_AUTHORITY,
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
            &NTT_BACKFILL_AUTHORITY,
        )?;
        Ok(())
    }

    /// See `accountant_backfill_core::instructions::backfill_chain_registration`. The
    /// registrations are the Standard Relayer's, written under the NTT program id.
    #[instruction(discriminator = 3)]
    pub fn backfill_relayer_chain_registration<'info>(
        ctx: Context<'info, BackfillRelayerChainRegistrationAccounts<'info>>,
        ix_data: RawIxData,
    ) -> Result<()> {
        let accounts = flatten_accounts!(ctx, [payer, system_program], remaining);
        backfill::backfill_chain_registration::process(
            ctx.program_id,
            &accounts,
            &ix_data.0,
            &NTT_BACKFILL_AUTHORITY,
        )?;
        Ok(())
    }

    /// See `accountant_backfill_core::instructions::backfill_transceiver_hub`.
    #[instruction(discriminator = 4)]
    pub fn backfill_transceiver_hub<'info>(
        ctx: Context<'info, BackfillTransceiverHubAccounts<'info>>,
        ix_data: RawIxData,
    ) -> Result<()> {
        let accounts = flatten_accounts!(ctx, [payer, system_program], remaining);
        backfill::backfill_transceiver_hub::process(
            ctx.program_id,
            &accounts,
            &ix_data.0,
            &NTT_BACKFILL_AUTHORITY,
        )?;
        Ok(())
    }

    /// See `accountant_backfill_core::instructions::backfill_transceiver_peer`.
    #[instruction(discriminator = 5)]
    pub fn backfill_transceiver_peer<'info>(
        ctx: Context<'info, BackfillTransceiverPeerAccounts<'info>>,
        ix_data: RawIxData,
    ) -> Result<()> {
        let accounts = flatten_accounts!(ctx, [payer, system_program], remaining);
        backfill::backfill_transceiver_peer::process(
            ctx.program_id,
            &accounts,
            &ix_data.0,
            &NTT_BACKFILL_AUTHORITY,
        )?;
        Ok(())
    }
}
