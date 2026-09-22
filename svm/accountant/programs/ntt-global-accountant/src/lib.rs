//! Wormhole NTT Global Accountant Solana program, anchor-lang 1.1.2.
//!
//! Shared machinery lives in `accountant-operational-core`, including the governance handlers.
//! This crate holds the NTT-specific instructions and the `#[program]` entry. Wire-format constraints
//! are those of the sibling `global-accountant` crate: 1-byte `AccountTag` at offset 0 with
//! `UncheckedAccount` + `bytemuck`, 1-byte instruction discriminators, PDA creation through
//! `pda_init`, and errors as `ProgramError::Custom(code)`.
//!
//! `declare_id!` pins the program address; PDAs still derive from the runtime `program_id`.

#![allow(unexpected_cfgs)]

use anchor_lang::prelude::*;

pub mod contexts;
pub mod instructions;
pub mod raw_ix_data;

pub use accountant_operational_core::err;
pub use global_accountant_definitions as definitions;

use accountant_operational_core::instructions as shared;
use definitions::{
    pubkey_eq, NTT_ACCOUNTANT_GOVERNANCE_MODULE, NTT_GLOBAL_ACCOUNTANT_PROGRAM_ID,
    RELAYER_GOVERNANCE_MODULE,
};

// `#[program]` codegen expects the `#[derive(Accounts)]` companion items at the crate root.
pub use contexts::*;
use raw_ix_data::RawIxData;

declare_id!("cGfHiC6Kgg3FpFZvgwGcswsCRtp4aBP2fzuXRQPizuN");
const _: () = assert!(
    pubkey_eq(&ID.to_bytes(), &NTT_GLOBAL_ACCOUNTANT_PROGRAM_ID),
    "declare_id! does not match NTT_GLOBAL_ACCOUNTANT_PROGRAM_ID"
);

/// Flatten an `Accounts` struct into the positional `Vec<AccountInfo>` the handlers take.
/// Field order must match the handler's account list.
macro_rules! flatten_accounts {
    ($accounts:expr, [$($field:ident),+ $(,)?]) => {
        vec![$($accounts.$field.to_account_info()),+]
    };
}

#[program]
pub mod ntt_global_accountant {
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
                relayer_registration_pda,
                hub_pda,
                peer_src_pda,
                peer_dst_pda,
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
        shared::close_pending::process(ctx.program_id, &accounts, &ix_data.0)?;
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
                relayer_registration_pda,
                hub_pda,
                peer_src_pda,
                peer_dst_pda,
            ]
        );
        crate::instructions::submit_vaas::process(ctx.program_id, &accounts, &ix_data.0)?;
        Ok(())
    }

    /// Standard Relayer `RegisterChain`; see
    /// `accountant_operational_core::instructions::register_chain`.
    #[instruction(discriminator = 3)]
    pub fn register_relayer_chain(
        ctx: Context<RegisterRelayerChain>,
        ix_data: RawIxData,
    ) -> Result<()> {
        let accounts = flatten_accounts!(
            ctx.accounts,
            [
                payer,
                verify_vaa_shim_program,
                guardian_set,
                guardian_signatures,
                registration_pda,
                system_program,
                register_chain_pda,
            ]
        );
        shared::register_chain::process(
            ctx.program_id,
            &accounts,
            &ix_data.0,
            &RELAYER_GOVERNANCE_MODULE,
        )?;
        Ok(())
    }

    /// See `accountant_operational_core::instructions::modify_balance`.
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
        shared::modify_balance::process(
            ctx.program_id,
            &accounts,
            &ix_data.0,
            &NTT_ACCOUNTANT_GOVERNANCE_MODULE,
        )?;
        Ok(())
    }

    /// See `crate::instructions::register_hub`.
    #[instruction(discriminator = 5)]
    pub fn register_hub(ctx: Context<RegisterHub>, ix_data: RawIxData) -> Result<()> {
        let accounts = flatten_accounts!(
            ctx.accounts,
            [
                payer,
                verify_vaa_shim_program,
                guardian_set,
                guardian_signatures,
                relayer_registration_pda,
                hub_pda,
                noreplay_bucket,
                noreplay_program,
                noreplay_authority,
                system_program,
            ]
        );
        crate::instructions::register_hub::process(ctx.program_id, &accounts, &ix_data.0)?;
        Ok(())
    }

    /// See `crate::instructions::register_peer`.
    #[instruction(discriminator = 6)]
    pub fn register_peer(ctx: Context<RegisterPeer>, ix_data: RawIxData) -> Result<()> {
        let accounts = flatten_accounts!(
            ctx.accounts,
            [
                payer,
                verify_vaa_shim_program,
                guardian_set,
                guardian_signatures,
                relayer_registration_pda,
                own_hub_pda,
                peer_hub_pda,
                hub_peer_pda,
                peer_pda,
                noreplay_bucket,
                noreplay_program,
                noreplay_authority,
                system_program,
            ]
        );
        crate::instructions::register_peer::process(ctx.program_id, &accounts, &ix_data.0)?;
        Ok(())
    }

    /// See `accountant_operational_core::instructions::upgrade_contract`.
    #[instruction(discriminator = 7)]
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
        shared::upgrade_contract::process(
            ctx.program_id,
            &accounts,
            &ix_data.0,
            &NTT_ACCOUNTANT_GOVERNANCE_MODULE,
        )?;
        Ok(())
    }
}
