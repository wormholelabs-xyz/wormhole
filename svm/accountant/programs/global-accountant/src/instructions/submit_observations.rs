//! `submit_observations` — WTT quorum tracker.
//!
//! A `(chain, emitter, sequence, digest)`-keyed `PendingObservationsLayout` PDA
//! accumulates guardian signatures. The quorum-completing observation atomically
//! flips the NoReplay slot, emits the canonical digest record via
//! `commit_log::emit`, applies balance effects, and closes the pending PDA,
//! refunding rent to its recorded payer.
//!
//! Sibling buckets at the same `(chain, emitter, sequence)` but different digests
//! (source-chain reorg) race independently; losers are reclaimed via
//! `close_pending`. Signatures are verified inline via the `secp256k1_recover`
//! syscall; only the bitmap is persisted.
//!
//! The product-neutral quorum machinery (instruction parse, signature verify,
//! pending-PDA lifecycle, bitmap accumulation, rent-refunding close) lives in
//! [`accountant_operational_core::instructions::quorum`]; this WTT orchestration
//! wires it to the WTT account layout + the Token Bridge chain-registration
//! check. NTT has its own orchestration over the same `quorum` helpers.

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

use accountant_operational_core::hash::double_keccak256;
use accountant_operational_core::instructions::quorum::{
    self, ParsedObservation, BODY_MIN_LEN, SUBMIT_FIXED_LEN,
};
use accountant_operational_core::instructions::{commit_log, noreplay};

use crate::definitions::{GlobalAccountantError, PendingObservationsLayout};
use crate::err;
use crate::instructions::transfer;
use crate::state::chain_registration;

/// Quorum tracker. The WTT-specific token-payload parse + balance mutation
/// is applied via `transfer::apply_from_body`, invoked once on the
/// quorum-completing branch after the NoReplay flip and commit-log emit.
/// A non-quorum-completing submission returns before the apply is reached.
pub fn process(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    // Split into fixed prefix + length-prefixed body. The body is required: the
    // signed digest and the routing tuple are both derived from it.
    if data.len() < SUBMIT_FIXED_LEN + 2 {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let (fixed_bytes, rest) = data.split_at(SUBMIT_FIXED_LEN);
    let fixed_bytes: &[u8; SUBMIT_FIXED_LEN] = fixed_bytes
        .try_into()
        .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
    let body_len = u16::from_le_bytes([rest[0], rest[1]]) as usize;
    if body_len < BODY_MIN_LEN || rest.len() < 2 + body_len {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let body_bytes = &rest[2..2 + body_len];

    let mut parsed = ParsedObservation::from_data(fixed_bytes)?;

    // Derive the signed digest from the supplied body. The signature is verified
    // against this value below, so the body authenticates itself — no separate
    // digest is carried in the instruction data.
    parsed.digest = double_keccak256(body_bytes);

    // Routing tuple is sourced from the now-authenticated body header, never
    // caller-supplied data — see the `quorum` module-level wire-format doc.
    parsed.populate_routing_from_body(body_bytes)?;

    // Accounts (WTT layout):
    //   0. `[WRITE, SIGNER]` submitter (fee + rent payer for all lazy PDAs).
    //   1. `[WRITE]`         pending PDA.
    //   2. `[]`              GuardianSet PDA (Core Bridge).
    //   3. `[WRITE]`         NoReplay bitmap PDA (read at pre-check, written at
    //                       commit; always WRITE per runtime declaration rules).
    //   4. `[]`              system program.
    //   5. `[]`              NoReplay program (CPI target).
    //   6. `[]`              NoReplay authority PDA owned by this program.
    //   7. `[WRITE]`         source-chain balance account PDA. Only touched on the
    //                       quorum-completing Transfer branch; sentinel otherwise.
    //   8. `[WRITE]`         destination-chain balance account PDA. Same semantics as slot 7.
    //   9. `[WRITE]`         rent recipient for the pending PDA close. Must equal
    //                       the bucket's recorded payer (rejected as `PayerMismatch`
    //                       otherwise); decoupled from submitter so any guardian
    //                       can complete quorum on the opener's behalf.
    //  10. `[]`              chain registration PDA. Read to verify the body's
    //                       `(emitter_chain, emitter_address)` is a registered
    //                       emitter; system-owned ⇒ `MissingChainRegistration`.
    //
    // The canonical digest record is emitted via `sol_log_data` rather than
    // stored in a PDA; off-chain indexers consume the program-log line carrying
    // the `ACCOUNTANT_DIGEST_LOG_TAG` prefix.
    let [submitter, pending_pda, guardian_set, noreplay_bucket, system_program_acc, noreplay_program, noreplay_authority, source_account_pda, dest_account_pda, rent_recipient, chain_registration_pda] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !submitter.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    // NoReplay pre-check rejects replays before any signature work.
    if noreplay::is_marked(
        noreplay_bucket,
        noreplay_authority.address(),
        parsed.chain,
        &parsed.emitter,
        parsed.sequence,
    )? {
        return Err(err(GlobalAccountantError::AlreadyAccounted));
    }

    // Chain-registration cross-check: reject valid sigs for an unregistered
    // (and therefore potentially fake) emitter. PDA address verified first.
    chain_registration::verify(
        program_id,
        chain_registration_pda,
        parsed.chain,
        &parsed.emitter,
    )?;

    // Verify the signature and learn the live guardian-set size, then derive the
    // quorum threshold from it — the set can be resized by governance, so a pinned
    // threshold would desync this path from the network (and from the shim path).
    let num_guardians = quorum::verify_signature(
        guardian_set,
        parsed.guardian_set_index,
        parsed.guardian_index,
        &parsed.digest,
        &parsed.signature,
    )?;
    let quorum_threshold = PendingObservationsLayout::quorum_for(num_guardians);

    let action = quorum::decide_pending_action(program_id, pending_pda, &parsed)?;
    let (layout, quorum_reached) = quorum::apply_action_and_accumulate(
        program_id,
        submitter,
        pending_pda,
        &parsed,
        action,
        quorum_threshold,
    )?;

    if !quorum_reached {
        return Ok(());
    }

    // Quorum reached. Commit atomically: NoReplay flip, canonical log emit,
    // balance accounting, pending close (tx-level rollback covers failures).
    noreplay::mark_used(
        submitter,
        noreplay_bucket,
        noreplay_program,
        noreplay_authority,
        system_program_acc,
        program_id,
        parsed.chain,
        &parsed.emitter,
        parsed.sequence,
    )?;

    commit_log::emit(
        parsed.chain,
        &parsed.emitter,
        parsed.sequence,
        &parsed.digest,
        parsed.guardian_set_index,
    );

    // WTT-specific balance work. Parse the token payload and mutate the two
    // balance-account PDAs; the authenticated emitter chain is the transfer
    // source chain. Any error (including an unknown payload) rolls back the
    // NoReplay mark with the tx, leaving the slot unconsumed for a future upgrade.
    transfer::apply_from_body(
        program_id,
        submitter,
        source_account_pda,
        dest_account_pda,
        parsed.chain,
        body_bytes,
    )?;

    let recorded_payer = layout.payer;
    quorum::close_pending_pda(pending_pda, rent_recipient, &recorded_payer)?;
    Ok(())
}
