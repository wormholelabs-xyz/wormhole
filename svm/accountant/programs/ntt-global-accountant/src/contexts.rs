//! `#[derive(Accounts)]` structs for every instruction.
//!
//! Accounts are `UncheckedAccount`; the handlers check owner, address, and seeds by hand
//! because the seeds come from the VAA body. Field order is the on-wire account order.

use anchor_lang::prelude::*;

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
    /// CHECK: owner and address checked in `guardian_set::verify_account`.
    pub guardian_set: UncheckedAccount<'info>,
    /// CHECK: address checked in `noreplay::is_marked`.
    pub noreplay_bucket: UncheckedAccount<'info>,
}

/// Accounts for `register_relayer_chain` (7 accounts). The registration PDA holds the
/// Standard Relayer emitter for one chain.
#[derive(Accounts)]
pub struct RegisterRelayerChain<'info> {
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
    /// CHECK: address checked in the handler; existence is the replay guard.
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

/// Accounts for `register_hub` (10 accounts).
#[derive(Accounts)]
pub struct RegisterHub<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: CPI target is the constant `VERIFY_VAA_SHIM_PROGRAM_ID`.
    pub verify_vaa_shim_program: UncheckedAccount<'info>,
    /// CHECK: authenticated by the Shim.
    pub guardian_set: UncheckedAccount<'info>,
    /// CHECK: authenticated by the Shim.
    pub guardian_signatures: UncheckedAccount<'info>,
    /// CHECK: address checked in `chain_registration::is_registered_emitter`; may be uninitialised.
    pub relayer_registration_pda: UncheckedAccount<'info>,
    /// CHECK: address checked in `pda::check_uninitialised`.
    #[account(mut)]
    pub hub_pda: UncheckedAccount<'info>,
    /// CHECK: address checked in `noreplay::is_marked` / `noreplay::mark_used`.
    #[account(mut)]
    pub noreplay_bucket: UncheckedAccount<'info>,
    /// CHECK: CPI target is the constant `NOREPLAY_PROGRAM_ID`.
    pub noreplay_program: UncheckedAccount<'info>,
    /// CHECK: address checked in `noreplay::mark_used`.
    pub noreplay_authority: UncheckedAccount<'info>,
    /// CHECK: passed through to the system and NoReplay CPIs.
    pub system_program: UncheckedAccount<'info>,
}

/// Accounts for `register_peer` (13 accounts).
#[derive(Accounts)]
pub struct RegisterPeer<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: CPI target is the constant `VERIFY_VAA_SHIM_PROGRAM_ID`.
    pub verify_vaa_shim_program: UncheckedAccount<'info>,
    /// CHECK: authenticated by the Shim.
    pub guardian_set: UncheckedAccount<'info>,
    /// CHECK: authenticated by the Shim.
    pub guardian_signatures: UncheckedAccount<'info>,
    /// CHECK: address checked in `chain_registration::is_registered_emitter`; may be uninitialised.
    pub relayer_registration_pda: UncheckedAccount<'info>,
    /// CHECK: address checked in `pda::check`; written only when adopting.
    #[account(mut)]
    pub own_hub_pda: UncheckedAccount<'info>,
    /// CHECK: address checked in `pda::check`; may be uninitialised.
    pub peer_hub_pda: UncheckedAccount<'info>,
    /// CHECK: address checked in `pda::check`; may be uninitialised.
    pub hub_peer_pda: UncheckedAccount<'info>,
    /// CHECK: address checked in `pda::check_uninitialised`.
    #[account(mut)]
    pub peer_pda: UncheckedAccount<'info>,
    /// CHECK: address checked in `noreplay::is_marked` / `noreplay::mark_used`.
    #[account(mut)]
    pub noreplay_bucket: UncheckedAccount<'info>,
    /// CHECK: CPI target is the constant `NOREPLAY_PROGRAM_ID`.
    pub noreplay_program: UncheckedAccount<'info>,
    /// CHECK: address checked in `noreplay::mark_used`.
    pub noreplay_authority: UncheckedAccount<'info>,
    /// CHECK: passed through to the system and NoReplay CPIs.
    pub system_program: UncheckedAccount<'info>,
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
