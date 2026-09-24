//! `#[derive(Accounts)]` structs for every instruction.
//!
//! Accounts are `UncheckedAccount`; the handlers check owner, address, and seeds by hand
//! because the seeds come from the VAA body. Field order is the on-wire account order.

use anchor_lang::prelude::*;

/// Accounts for `submit_observations` (11 accounts).
#[derive(Accounts)]
pub struct SubmitObservations<'info> {
    #[account(mut)]
    pub submitter: Signer<'info>,
    /// CHECK: address and lifecycle handled in `quorum`.
    #[account(mut)]
    pub pending_pda: UncheckedAccount<'info>,
    /// CHECK: owner and address checked in `quorum::verify_signature`.
    pub guardian_set: UncheckedAccount<'info>,
    /// CHECK: address checked in `noreplay::is_marked` / `noreplay::mark_used`.
    #[account(mut)]
    pub noreplay_bucket: UncheckedAccount<'info>,
    /// CHECK: passed through to the NoReplay CPI.
    pub system_program: UncheckedAccount<'info>,
    /// CHECK: CPI target is the constant `NOREPLAY_PROGRAM_ID`.
    pub noreplay_program: UncheckedAccount<'info>,
    /// CHECK: address checked in `noreplay::mark_used`.
    pub noreplay_authority: UncheckedAccount<'info>,
    /// CHECK: address checked and lazy-initialised in `transfer::apply_transfer`.
    #[account(mut)]
    pub source_account_pda: UncheckedAccount<'info>,
    /// CHECK: as `source_account_pda`.
    #[account(mut)]
    pub dest_account_pda: UncheckedAccount<'info>,
    /// CHECK: compared to the recorded payer in `quorum::close_pending_pda`.
    #[account(mut)]
    pub rent_recipient: UncheckedAccount<'info>,
    /// CHECK: address and emitter checked in `chain_registration::verify`.
    pub chain_registration_pda: UncheckedAccount<'info>,
}

/// Accounts for `submit_vaas` (11 accounts).
#[derive(Accounts)]
pub struct SubmitVaas<'info> {
    #[account(mut)]
    pub submitter: Signer<'info>,
    /// CHECK: CPI target is the constant `VERIFY_VAA_SHIM_PROGRAM_ID`.
    pub verify_vaa_shim_program: UncheckedAccount<'info>,
    /// CHECK: authenticated by the Shim.
    pub guardian_set: UncheckedAccount<'info>,
    /// CHECK: authenticated by the Shim.
    pub guardian_signatures: UncheckedAccount<'info>,
    /// CHECK: address checked in `noreplay::is_marked` / `noreplay::mark_used`.
    #[account(mut)]
    pub noreplay_bucket: UncheckedAccount<'info>,
    /// CHECK: CPI target is the constant `NOREPLAY_PROGRAM_ID`.
    pub noreplay_program: UncheckedAccount<'info>,
    /// CHECK: address checked in `noreplay::mark_used`.
    pub noreplay_authority: UncheckedAccount<'info>,
    /// CHECK: address checked and lazy-initialised in `transfer::apply_transfer`.
    #[account(mut)]
    pub source_account_pda: UncheckedAccount<'info>,
    /// CHECK: as `source_account_pda`.
    #[account(mut)]
    pub dest_account_pda: UncheckedAccount<'info>,
    /// CHECK: passed through to the NoReplay CPI.
    pub system_program: UncheckedAccount<'info>,
    /// CHECK: address and emitter checked in `chain_registration::verify`.
    pub chain_registration_pda: UncheckedAccount<'info>,
}

/// Accounts for `register_chain` (7 accounts).
#[derive(Accounts)]
pub struct RegisterChain<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: CPI target is the constant `VERIFY_VAA_SHIM_PROGRAM_ID`.
    pub verify_vaa_shim_program: UncheckedAccount<'info>,
    /// CHECK: authenticated by the Shim.
    pub guardian_set: UncheckedAccount<'info>,
    /// CHECK: authenticated by the Shim.
    pub guardian_signatures: UncheckedAccount<'info>,
    /// CHECK: address and bump checked in the handler.
    #[account(mut)]
    pub registration_pda: UncheckedAccount<'info>,
    /// CHECK: passed through to the system CPI.
    pub system_program: UncheckedAccount<'info>,
    /// CHECK: address checked in the handler
    #[account(mut)]
    pub register_chain_pda: UncheckedAccount<'info>,
}

/// Accounts for `modify_balance` (7 accounts).
#[derive(Accounts)]
pub struct ModifyBalance<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: CPI target is the constant `VERIFY_VAA_SHIM_PROGRAM_ID`.
    pub verify_vaa_shim_program: UncheckedAccount<'info>,
    /// CHECK: authenticated by the Shim.
    pub guardian_set: UncheckedAccount<'info>,
    /// CHECK: authenticated by the Shim.
    pub guardian_signatures: UncheckedAccount<'info>,
    /// CHECK: address checked and lazy-initialised in the handler.
    #[account(mut)]
    pub balance_pda: UncheckedAccount<'info>,
    /// CHECK: passed through to the system CPI.
    pub system_program: UncheckedAccount<'info>,
    /// CHECK: address checked in the handler; existence is the replay guard.
    #[account(mut)]
    pub modify_balance_pda: UncheckedAccount<'info>,
}

/// Accounts for `close_pending` (5 accounts). Handler:
/// `accountant_operational_core::instructions::close_pending`.
#[derive(Accounts)]
pub struct ClosePending<'info> {
    pub closer: Signer<'info>,
    /// CHECK: address re-derived in the handler.
    #[account(mut)]
    pub pending_pda: UncheckedAccount<'info>,
    /// CHECK: compared to the recorded payer in the handler.
    #[account(mut)]
    pub rent_recipient: UncheckedAccount<'info>,
    /// CHECK: owner checked in `is_guardian_set_expired`.
    pub guardian_set: UncheckedAccount<'info>,
    /// CHECK: address checked in `noreplay::is_marked`.
    pub noreplay_bucket: UncheckedAccount<'info>,
}

/// Accounts for `upgrade_contract` (16 accounts).
#[derive(Accounts)]
pub struct UpgradeContract<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: CPI target is the constant `VERIFY_VAA_SHIM_PROGRAM_ID`.
    pub verify_vaa_shim_program: UncheckedAccount<'info>,
    /// CHECK: authenticated by the Shim.
    pub guardian_set: UncheckedAccount<'info>,
    /// CHECK: authenticated by the Shim.
    pub guardian_signatures: UncheckedAccount<'info>,
    /// CHECK: address checked in `noreplay::is_marked` / `noreplay::mark_used`.
    #[account(mut)]
    pub noreplay_bucket: UncheckedAccount<'info>,
    /// CHECK: CPI target is the constant `NOREPLAY_PROGRAM_ID`.
    pub noreplay_program: UncheckedAccount<'info>,
    /// CHECK: address checked in `noreplay::mark_used`.
    pub noreplay_authority: UncheckedAccount<'info>,
    /// CHECK: passed through to the NoReplay CPI.
    pub system_program: UncheckedAccount<'info>,
    /// CHECK: address checked in `loader::upgrade_program`.
    pub upgrade_authority: UncheckedAccount<'info>,
    /// CHECK: receives the freed program-data lamports; unconstrained by design.
    #[account(mut)]
    pub spill: UncheckedAccount<'info>,
    /// CHECK: must equal the payload's `new_contract`; checked in `loader::upgrade_program`.
    #[account(mut)]
    pub buffer: UncheckedAccount<'info>,
    /// CHECK: address checked in `loader::upgrade_program`.
    #[account(mut)]
    pub program_data: UncheckedAccount<'info>,
    /// CHECK: must equal this program id; checked in `loader::upgrade_program`.
    #[account(mut)]
    pub program_account: UncheckedAccount<'info>,
    /// CHECK: rent sysvar, read by the loader.
    pub rent: UncheckedAccount<'info>,
    /// CHECK: clock sysvar, read by the loader.
    pub clock: UncheckedAccount<'info>,
    /// CHECK: the BPF upgradeable loader; the runtime validates the CPI target.
    pub bpf_loader_upgradeable_program: UncheckedAccount<'info>,
}
