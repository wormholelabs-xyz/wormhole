//! Wormhole NTT Global Accountant — Solana port (anchor-lang 1.1.2).
//!
//! The product-neutral operational machinery (quorum tracker, signed-VAA
//! backfill, pending cleanup, NoReplay/Shim CPIs, PDA-init, commit-log, hashing,
//! and the zero-copy state layouts) lives in `accountant-operational-core`,
//! deployed under this program's own ID. This crate keeps the NTT-specific
//! governance handlers (`register_relayer_chain`, `modify_balance`), the
//! transceiver hub/peer registration handlers (`register_hub`,
//! `register_peer`), the NTT transfer flow (`ntt_transfer`, wired into
//! `submit_observations` / `submit_vaas`), and the `#[program]` entrypoint
//! that wires them all up.
//!
//! Ported from pinocchio to anchor-lang 1.1.2 — see
//! `.claude/tasks/anchor-migration-plan-v1.1.2.md` for the full design record.
//! Mirrors the sibling WTT `global-accountant` crate's migration exactly:
//! Anchor is adopted purely for the `#[program]`/`#[derive(Accounts)]`/
//! `Context` ergonomics; every wire-format-affecting decision from the
//! pinocchio version is preserved byte-for-byte:
//!
//! - The offset-0 1-byte `AccountTag` (values 5/6/7 for this program's NTT
//!   layouts) is still the account discriminator; state layouts stay
//!   `bytemuck::Pod` in the anchor-free `definitions` crate, loaded through
//!   `UncheckedAccount` + manual `bytemuck`, never `#[account(zero_copy)]`.
//! - The 1-byte instruction dispatch discriminator (`0..=6`) is preserved via
//!   `#[instruction(discriminator = N)]` on each handler below, instead of
//!   Anchor's default 8-byte sighash.
//! - `CreateAccountAllowPrefund` (SIMD-0312) is still a manual CPI via
//!   `accountant_operational_core::instructions::pda_init::init_or_upgrade_pda`,
//!   never Anchor's `#[account(init)]`.
//! - `GlobalAccountantError` still maps to `ProgramError::Custom(code)` with
//!   the same stable numeric ABI, not Anchor's `#[error_code]`.
//!
//! One new, unavoidable constraint Anchor does impose: `declare_id!` pins this
//! program to a single fixed address, checked on every entry. The
//! pre-migration pinocchio program had no such check — it derived every PDA
//! from the *runtime* `program_id` and had no `declare_id!` at all (mollusk
//! tests supplied an arbitrary `program_id()`). All existing mollusk fixtures
//! already used the same fixed test address (`Pubkey::new_from_array([9u8; 32])`,
//! base58 `cGfHiC6Kgg3FpFZvgwGcswsCRtp4aBP2fzuXRQPizuN`), so `declare_id!`
//! below reuses that value — this is a separate program ID from the WTT
//! `global-accountant`/`global-accountant-backfill` programs. The surfpool E2E
//! test, which previously deployed at a fresh random keypair per run, was
//! updated to deploy at this same fixed ID (see the migration report).

#![allow(unexpected_cfgs)]

use anchor_lang::prelude::*;

pub mod contexts;
pub mod instructions;
pub mod raw_ix_data;

pub use global_accountant_definitions as definitions;
pub use accountant_operational_core::err;

// `#[derive(Accounts)]`'s macro-generated companion items (e.g.
// `__client_accounts_submit_vaas`) are emitted alongside each struct, i.e.
// under `crate::contexts::`. The `#[program]` macro's own codegen assumes
// they're reachable at the crate root, so re-export the whole module.
pub use contexts::*;
use raw_ix_data::RawIxData;

declare_id!("cGfHiC6Kgg3FpFZvgwGcswsCRtp4aBP2fzuXRQPizuN");

/// Flatten a `#[derive(Accounts)]` struct's fields into a positionally
/// ordered `Vec<AccountInfo>` matching the original pinocchio slice order, so
/// the (unchanged) `instructions::*::process` / `accountant_operational_core`
/// handlers can be called exactly as before. A macro because each handler's
/// field list/count differs; see `contexts.rs` for the field order each
/// mirrors.
macro_rules! flatten_accounts {
    ($accounts:expr, [$($field:ident),+ $(,)?]) => {
        vec![$($accounts.$field.to_account_info()),+]
    };
}

#[program]
pub mod ntt_global_accountant {
    use super::*;

    /// Dispatch discriminator 0. See `crate::instructions::submit_observations`.
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
                rent_recipient,
                relayer_registration_pda,
                transceiver_hub_pda,
                transceiver_peer_src_pda,
                transceiver_peer_dst_pda,
                source_balance,
                dest_balance,
            ]
        );
        crate::instructions::submit_observations::process(ctx.program_id, &accounts, &ix_data.0)?;
        Ok(())
    }

    /// Dispatch discriminator 1. Shared handler; see
    /// `accountant_operational_core::instructions::close_pending`.
    #[instruction(discriminator = 1)]
    pub fn close_pending(ctx: Context<ClosePending>, ix_data: RawIxData) -> Result<()> {
        let accounts = flatten_accounts!(
            ctx.accounts,
            [closer, pending_pda, rent_recipient, guardian_set, noreplay_bucket]
        );
        accountant_operational_core::instructions::close_pending::process(
            ctx.program_id,
            &accounts,
            &ix_data.0,
        )?;
        Ok(())
    }

    /// Dispatch discriminator 2. See `crate::instructions::submit_vaas`.
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
                system_program,
                relayer_registration_pda,
                transceiver_hub_pda,
                transceiver_peer_src_pda,
                transceiver_peer_dst_pda,
                source_balance,
                dest_balance,
            ]
        );
        crate::instructions::submit_vaas::process(ctx.program_id, &accounts, &ix_data.0)?;
        Ok(())
    }

    /// Dispatch discriminator 3. See `crate::instructions::register_relayer_chain`.
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
                noreplay_bucket,
                noreplay_program,
                noreplay_authority,
                system_program,
            ]
        );
        crate::instructions::register_relayer_chain::process(ctx.program_id, &accounts, &ix_data.0)?;
        Ok(())
    }

    /// Dispatch discriminator 4. See `crate::instructions::modify_balance`.
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
                modification_pda,
            ]
        );
        crate::instructions::modify_balance::process(ctx.program_id, &accounts, &ix_data.0)?;
        Ok(())
    }

    /// Dispatch discriminator 5. See `crate::instructions::register_hub`.
    #[instruction(discriminator = 5)]
    pub fn register_hub(ctx: Context<RegisterHub>, ix_data: RawIxData) -> Result<()> {
        let accounts = flatten_accounts!(
            ctx.accounts,
            [
                payer,
                verify_vaa_shim_program,
                guardian_set,
                guardian_signatures,
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

    /// Dispatch discriminator 6. See `crate::instructions::register_peer`.
    #[instruction(discriminator = 6)]
    pub fn register_peer(ctx: Context<RegisterPeer>, ix_data: RawIxData) -> Result<()> {
        let accounts = flatten_accounts!(
            ctx.accounts,
            [
                payer,
                verify_vaa_shim_program,
                guardian_set,
                guardian_signatures,
                peer_hub_pda,
                own_hub_pda,
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
}
