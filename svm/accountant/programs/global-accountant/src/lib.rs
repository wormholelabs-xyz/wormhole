//! Wormhole Global Accountant — Solana port (anchor-lang 1.1.2).
//!
//! The product-neutral operational machinery (quorum tracker, signed-VAA
//! backfill, pending cleanup, NoReplay/Shim CPIs, PDA-init, commit-log, hashing,
//! and the zero-copy state layouts) lives in `accountant-operational-core`. This
//! crate keeps the WTT-specific governance handlers (`register_chain`,
//! `modify_balance`), the Token Bridge transfer applicator (`transfer`), and the
//! `#[program]` entrypoint that wires the core handlers to a Token Bridge
//! balance-application callback.
//!
//! Ported from pinocchio to anchor-lang 1.1.2 — see
//! `.claude/tasks/anchor-migration-plan-v1.1.2.md` for the full design record.
//! Anchor is adopted purely for the `#[program]`/`#[derive(Accounts)]`/
//! `Context` ergonomics; every wire-format-affecting decision from the
//! pinocchio version is preserved byte-for-byte:
//!
//! - The offset-0 1-byte `AccountTag` (not Anchor's 8-byte account
//!   discriminator) is still the account discriminator; state layouts stay
//!   `bytemuck::Pod` in the anchor-free `definitions` crate, loaded through
//!   `UncheckedAccount` + manual `bytemuck`, never `#[account(zero_copy)]`.
//! - The 1-byte instruction dispatch discriminator (`0..=4`) is preserved via
//!   `#[instruction(discriminator = N)]` on each handler below, instead of
//!   Anchor's default 8-byte sighash.
//! - `CreateAccountAllowPrefund` (SIMD-0312) is still a manual CPI (see
//!   `accountant_operational_core::instructions::pda_init`), never Anchor's
//!   `#[account(init)]`, which would regress the dust-DoS defense.
//! - `GlobalAccountantError` still maps to `ProgramError::Custom(code)` with
//!   the same stable numeric ABI, not Anchor's `#[error_code]` (which would
//!   renumber every code with anchor's own `+6000` offset).
//!
//! One new, unavoidable constraint Anchor does impose: `declare_id!` pins this
//! program to a single fixed address, checked on every entry
//! (`try_entry`/`ErrorCode::DeclaredProgramIdMismatch`). The pre-migration
//! pinocchio program had no such check — it derived every PDA from the
//! *runtime* `program_id` and so was deployable at any address. All existing
//! fixtures already used the same fixed test address
//! (`Pubkey::new_from_array([7u8; 32])`, base58
//! `US517G5965aydkZ46HS38QLi7UQiSojurfbQfKCELFx`) for the mollusk suite, so
//! `declare_id!` below reuses that value; the two surfpool E2E tests that
//! previously deployed at a fresh random keypair per run were updated to
//! deploy at this same fixed ID (see the migration report for the full list).

#![allow(unexpected_cfgs)]

use anchor_lang::prelude::*;

pub mod contexts;
pub mod instructions;
pub mod raw_ix_data;
pub mod state;

pub use global_accountant_definitions as definitions;
pub use accountant_operational_core::err;

// `#[derive(Accounts)]`'s macro-generated companion items (e.g.
// `__client_accounts_submit_vaas`) are emitted alongside each struct, i.e.
// under `crate::contexts::`. The `#[program]` macro's own codegen assumes
// they're reachable at the crate root, so re-export the whole module.
pub use contexts::*;
use raw_ix_data::RawIxData;

declare_id!("US517G5965aydkZ46HS38QLi7UQiSojurfbQfKCELFx");

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
pub mod global_accountant {
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
                source_account_pda,
                dest_account_pda,
                rent_recipient,
                chain_registration_pda,
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
                source_account_pda,
                dest_account_pda,
                system_program,
                chain_registration_pda,
            ]
        );
        crate::instructions::submit_vaas::process(ctx.program_id, &accounts, &ix_data.0)?;
        Ok(())
    }

    /// Dispatch discriminator 3. See `crate::instructions::register_chain`.
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
}
