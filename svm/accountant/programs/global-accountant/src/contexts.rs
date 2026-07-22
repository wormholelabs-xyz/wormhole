//! `#[derive(Accounts)]` context structs for every instruction.
//!
//! Every non-`Signer` account is a bare `UncheckedAccount` with no
//! `owner`/`seeds`/`address` constraints: every one of these accounts is
//! already validated by hand inside the (ported, unchanged) handler bodies in
//! `accountant_operational_core`/`crate::instructions`/`crate::state` — the
//! wipe-and-recreate pending-PDA lifecycle, the SIMD-0312 prefund-tolerant
//! create, and the dynamic (body-derived) PDA seeds are not expressible as
//! Anchor constraints in the first place (migration plan §2b/§2d). Adding
//! Anchor-side constraints on top would re-derive/re-check addresses Anchor
//! has no way to skip, doubling work already done manually and inflating the
//! CU budget for no additional safety (plan §7.1) — so Anchor is used here
//! purely for the `Context`/arity/discriminator ergonomics, not its
//! constraint system.
//!
//! Field order within each struct matches the original pinocchio positional
//! slice exactly, so the on-wire account-meta order is unchanged; see each
//! handler's own module doc for the authoritative account-list comment.

use anchor_lang::prelude::*;

/// Accounts for `submit_observations` (11 accounts).
#[derive(Accounts)]
pub struct SubmitObservations<'info> {
    #[account(mut)]
    pub submitter: Signer<'info>,
    /// CHECK: canonical `(chain, emitter, sequence, digest)` pending PDA;
    /// address re-derived and lifecycle (create/wipe-recreate/continue)
    /// managed manually in `accountant_operational_core::instructions::quorum`.
    #[account(mut)]
    pub pending_pda: UncheckedAccount<'info>,
    /// CHECK: Core Bridge `GuardianSet`; owner + canonical address checked in
    /// `quorum::verify_signature`.
    pub guardian_set: UncheckedAccount<'info>,
    /// CHECK: NoReplay bitmap PDA; address re-derived and checked in
    /// `noreplay::is_marked` / `noreplay::mark_used`.
    #[account(mut)]
    pub noreplay_bucket: UncheckedAccount<'info>,
    /// CHECK: System Program, passed through positionally to the NoReplay CPI
    /// exactly as the pre-migration code did (never itself address-checked).
    pub system_program: UncheckedAccount<'info>,
    /// CHECK: NoReplay program; the CPI target is always the hardcoded
    /// `NOREPLAY_PROGRAM_ID` constant, never this account's address.
    pub noreplay_program: UncheckedAccount<'info>,
    /// CHECK: this program's NoReplay authority PDA; address re-derived in
    /// `noreplay::mark_used`.
    pub noreplay_authority: UncheckedAccount<'info>,
    /// CHECK: source-chain `BalanceAccount` PDA; address re-derived and
    /// lazily initialised in `transfer::apply_transfer`.
    #[account(mut)]
    pub source_account_pda: UncheckedAccount<'info>,
    /// CHECK: destination-chain `BalanceAccount` PDA; same as `source_account_pda`.
    #[account(mut)]
    pub dest_account_pda: UncheckedAccount<'info>,
    /// CHECK: rent recipient for the pending-PDA close; checked against the
    /// PDA's recorded payer in `quorum::close_pending_pda`.
    #[account(mut)]
    pub rent_recipient: UncheckedAccount<'info>,
    /// CHECK: `ChainRegistration` PDA; address + emitter checked in
    /// `chain_registration::verify`.
    pub chain_registration_pda: UncheckedAccount<'info>,
}

/// Accounts for `submit_vaas` (11 accounts).
#[derive(Accounts)]
pub struct SubmitVaas<'info> {
    #[account(mut)]
    pub submitter: Signer<'info>,
    /// CHECK: Verify VAA Shim program; the CPI target is always the hardcoded
    /// `VERIFY_VAA_SHIM_PROGRAM_ID` constant, never this account's address.
    pub verify_vaa_shim_program: UncheckedAccount<'info>,
    /// CHECK: Core Bridge `GuardianSet`; authenticated by the Shim itself (see
    /// `shim::verify_vaa` doc).
    pub guardian_set: UncheckedAccount<'info>,
    /// CHECK: Shim `GuardianSignatures` PDA; authenticated by the Shim itself.
    pub guardian_signatures: UncheckedAccount<'info>,
    /// CHECK: NoReplay bitmap PDA; address re-derived and checked in
    /// `noreplay::is_marked` / `noreplay::mark_used`.
    #[account(mut)]
    pub noreplay_bucket: UncheckedAccount<'info>,
    /// CHECK: NoReplay program; the CPI target is always the hardcoded
    /// `NOREPLAY_PROGRAM_ID` constant.
    pub noreplay_program: UncheckedAccount<'info>,
    /// CHECK: this program's NoReplay authority PDA; address re-derived in
    /// `noreplay::mark_used`.
    pub noreplay_authority: UncheckedAccount<'info>,
    /// CHECK: source-chain `BalanceAccount` PDA; address re-derived and
    /// lazily initialised in `transfer::apply_transfer`.
    #[account(mut)]
    pub source_account_pda: UncheckedAccount<'info>,
    /// CHECK: destination-chain `BalanceAccount` PDA; same as `source_account_pda`.
    #[account(mut)]
    pub dest_account_pda: UncheckedAccount<'info>,
    /// CHECK: System Program, passed through positionally to the NoReplay CPI.
    pub system_program: UncheckedAccount<'info>,
    /// CHECK: `ChainRegistration` PDA; address + emitter checked in
    /// `chain_registration::verify`.
    pub chain_registration_pda: UncheckedAccount<'info>,
}

/// Accounts for `register_chain` (9 accounts).
#[derive(Accounts)]
pub struct RegisterChain<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: Verify VAA Shim program; CPI target is the hardcoded constant.
    pub verify_vaa_shim_program: UncheckedAccount<'info>,
    /// CHECK: Core Bridge `GuardianSet`; authenticated by the Shim.
    pub guardian_set: UncheckedAccount<'info>,
    /// CHECK: Shim `GuardianSignatures` PDA; authenticated by the Shim.
    pub guardian_signatures: UncheckedAccount<'info>,
    /// CHECK: `ChainRegistration` PDA; address + canonical bump checked
    /// in-handler before init/overwrite.
    #[account(mut)]
    pub registration_pda: UncheckedAccount<'info>,
    /// CHECK: NoReplay bitmap PDA; checked in `noreplay::is_marked` /
    /// `noreplay::mark_used`.
    #[account(mut)]
    pub noreplay_bucket: UncheckedAccount<'info>,
    /// CHECK: NoReplay program; CPI target is the hardcoded constant.
    pub noreplay_program: UncheckedAccount<'info>,
    /// CHECK: this program's NoReplay authority PDA; address re-derived in
    /// `noreplay::mark_used`.
    pub noreplay_authority: UncheckedAccount<'info>,
    /// CHECK: System Program, passed through positionally to the NoReplay CPI.
    pub system_program: UncheckedAccount<'info>,
}

/// Accounts for `modify_balance` (7 accounts).
#[derive(Accounts)]
pub struct ModifyBalance<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: Verify VAA Shim program; CPI target is the hardcoded constant.
    pub verify_vaa_shim_program: UncheckedAccount<'info>,
    /// CHECK: Core Bridge `GuardianSet`; authenticated by the Shim.
    pub guardian_set: UncheckedAccount<'info>,
    /// CHECK: Shim `GuardianSignatures` PDA; authenticated by the Shim.
    pub guardian_signatures: UncheckedAccount<'info>,
    /// CHECK: `BalanceAccount` PDA; address checked and lazily initialised
    /// in-handler.
    #[account(mut)]
    pub balance_pda: UncheckedAccount<'info>,
    /// CHECK: System Program; not itself address-checked (matches pre-migration).
    pub system_program: UncheckedAccount<'info>,
    /// CHECK: per-sequence `Modification` PDA; address checked and lazily
    /// initialised in-handler; existence enforces governance-path replay
    /// protection.
    #[account(mut)]
    pub modification_pda: UncheckedAccount<'info>,
}

/// Accounts for `close_pending` (5 accounts). The handler itself lives in
/// `accountant_operational_core::instructions::close_pending` (shared with
/// the NTT accountant program).
#[derive(Accounts)]
pub struct ClosePending<'info> {
    pub closer: Signer<'info>,
    /// CHECK: pending PDA being closed; canonical address re-derived from
    /// the loaded layout's own `chain`/`digest` fields in-handler.
    #[account(mut)]
    pub pending_pda: UncheckedAccount<'info>,
    /// CHECK: rent recipient; checked against the pending PDA's recorded
    /// payer in-handler.
    #[account(mut)]
    pub rent_recipient: UncheckedAccount<'info>,
    /// CHECK: Core Bridge `GuardianSet`; owner checked in-handler
    /// (`guardian_set_expired`).
    pub guardian_set: UncheckedAccount<'info>,
    /// CHECK: NoReplay bitmap PDA; checked in `noreplay::is_marked`.
    pub noreplay_bucket: UncheckedAccount<'info>,
}
