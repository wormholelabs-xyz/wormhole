//! Wormhole Global Accountant (WTT) Solana program, anchor-lang 1.1.2.
//!
//! Shared machinery lives in `accountant-operational-core`. This crate holds the
//! governance handlers, the Token Bridge transfer applicator, and the `#[program]` entry.
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
//! `declare_id!` pins the program address; PDAs still derive from the runtime `program_id`.

#![allow(unexpected_cfgs)]

use accountant_operational_core::flatten_accounts;
use anchor_lang::prelude::*;

pub mod contexts;
pub mod instructions;

pub use accountant_operational_core::{err, raw_ix_data::RawIxData};
pub use global_accountant_definitions as definitions;

// `#[program]` codegen expects the `#[derive(Accounts)]` companion items at the crate root.
pub use contexts::*;

declare_id!("YMN9Qj5jPNp7j14VPcML1B6xGgcPWVZUGLFU3Mnyfaf");

const _: () = assert!(
    definitions::is_accountant_program_id(&ID.to_bytes()),
    "declare_id! does not match ACCOUNTANT_PROGRAM_ID"
);

#[program]
pub mod global_accountant {
    use super::*;

    /// See `crate::instructions::submit_observations`.
    #[instruction(discriminator = 0)]
    pub fn submit_observations(ctx: Context<SubmitObservations>, ix_data: RawIxData) -> Result<()> {
        let accounts = flatten_accounts!(
            ctx.accounts,
            [
                submitter,
                pending_pda,
                guardian_set,
                noreplay_bucket,
                system_program,
                noreplay_program,
                noreplay_authority,
                source_account_pda,
                dest_account_pda,
                rent_recipient,
                chain_registration_pda,
            ]
        );
        crate::instructions::submit_observations::process(ctx.program_id, &accounts, &ix_data.0)?;
        Ok(())
    }

    /// See `accountant_operational_core::instructions::close_pending`.
    #[instruction(discriminator = 1)]
    pub fn close_pending(ctx: Context<ClosePending>, ix_data: RawIxData) -> Result<()> {
        let accounts = flatten_accounts!(
            ctx.accounts,
            [
                closer,
                pending_pda,
                rent_recipient,
                guardian_set,
                noreplay_bucket
            ]
        );
        accountant_operational_core::instructions::close_pending::process(
            ctx.program_id,
            &accounts,
            &ix_data.0,
        )?;
        Ok(())
    }

    /// See `crate::instructions::submit_vaas`.
    #[instruction(discriminator = 2)]
    pub fn submit_vaas(ctx: Context<SubmitVaas>, ix_data: RawIxData) -> Result<()> {
        let accounts = flatten_accounts!(
            ctx.accounts,
            [
                submitter,
                verify_vaa_shim_program,
                guardian_set,
                guardian_signatures,
                noreplay_bucket,
                noreplay_program,
                noreplay_authority,
                source_account_pda,
                dest_account_pda,
                system_program,
                chain_registration_pda,
            ]
        );
        crate::instructions::submit_vaas::process(ctx.program_id, &accounts, &ix_data.0)?;
        Ok(())
    }

    /// See `crate::instructions::register_chain`.
    #[instruction(discriminator = 3)]
    pub fn register_chain(ctx: Context<RegisterChain>, ix_data: RawIxData) -> Result<()> {
        let accounts = flatten_accounts!(
            ctx.accounts,
            [
                payer,
                verify_vaa_shim_program,
                guardian_set,
                guardian_signatures,
                registration_pda,
                noreplay_bucket,
                noreplay_program,
                noreplay_authority,
                system_program,
            ]
        );
        crate::instructions::register_chain::process(ctx.program_id, &accounts, &ix_data.0)?;
        Ok(())
    }

    /// See `crate::instructions::modify_balance`.
    #[instruction(discriminator = 4)]
    pub fn modify_balance(ctx: Context<ModifyBalance>, ix_data: RawIxData) -> Result<()> {
        let accounts = flatten_accounts!(
            ctx.accounts,
            [
                payer,
                verify_vaa_shim_program,
                guardian_set,
                guardian_signatures,
                balance_pda,
                system_program,
                modify_balance_pda,
            ]
        );
        crate::instructions::modify_balance::process(ctx.program_id, &accounts, &ix_data.0)?;
        Ok(())
    }

    /// See `crate::instructions::upgrade_contract`.
    #[instruction(discriminator = 5)]
    pub fn upgrade_contract(ctx: Context<UpgradeContract>, ix_data: RawIxData) -> Result<()> {
        let accounts = flatten_accounts!(
            ctx.accounts,
            [
                payer,
                verify_vaa_shim_program,
                guardian_set,
                guardian_signatures,
                noreplay_bucket,
                noreplay_program,
                noreplay_authority,
                system_program,
                upgrade_authority,
                spill,
                buffer,
                program_data,
                program_account,
                rent,
                clock,
                bpf_loader_upgradeable_program,
            ]
        );
        crate::instructions::upgrade_contract::process(ctx.program_id, &accounts, &ix_data.0)?;
        Ok(())
    }
}
