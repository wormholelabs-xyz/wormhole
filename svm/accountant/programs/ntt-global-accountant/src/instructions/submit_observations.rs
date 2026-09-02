//! NTT `submit_observations` — quorum tracker over the shared core primitives.
//!
//! Reuses `accountant-operational-core`'s `quorum` helpers verbatim for the
//! digest verify, signature check, pending-PDA lifecycle, bitmap accumulation,
//! NoReplay flip, commit-log emit, and rent-refunding close. The digest path is
//! IDENTICAL to WTT (`keccak256(keccak256(body))`, routing tuple from the body
//! header `[8..50]`). The post-quorum balance work runs the NTT transfer flow
//! (`ntt_transfer::apply_ntt_transfer`) instead of the WTT Token Bridge flow,
//! and the account layout drops WTT's chain-registration slot in favour of the
//! six NTT transfer accounts.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::hash::{double_keccak256, observation_signing_digest};
use accountant_operational_core::cpi::noreplay;
use accountant_operational_core::support::quorum::{ParsedObservation, BODY_MIN_LEN};
use accountant_operational_core::support::{commit_log, quorum};
use accountant_operational_core::ProgramResult;

use crate::definitions::{
    split_body, GlobalAccountantError, PendingObservationsLayout, SubmitObservationsIxData,
    NTT_SUBMIT_OBSERVATION_PREFIX,
};
use crate::err;
use crate::instructions::ntt_transfer::apply_ntt_transfer;

/// NTT `submit_observations`. Instruction-data wire format is identical to WTT
/// ([`SubmitObservationsIxData`] ‖ body). Guardian signatures are verified
/// against `keccak256(NTT_SUBMIT_OBSERVATION_PREFIX ‖ tx_hash ‖ body)`; the
/// dedup/quorum key is `double_keccak256(body)`, derived from the body rather
/// than received over the wire.
///
/// Account layout (mirrors WTT slots 0..6, then drops the chain-registration
/// slot for the six NTT transfer accounts):
///
///   0. `[WRITE, SIGNER]` submitter (fee + rent payer for lazy PDAs).
///   1. `[WRITE]`         pending PDA.
///   2. `[]`              GuardianSet PDA (Core Bridge).
///   3. `[WRITE]`         NoReplay bitmap PDA.
///   4. `[]`              system program.
///   5. `[]`              NoReplay program (CPI target).
///   6. `[]`              NoReplay authority PDA owned by this program.
///   7. `[WRITE]`         rent recipient for the pending close (= recorded payer).
///   8. `[]`              relayer-registration PDA for `emitter_chain`.
///   9. `[]`              TransceiverHub PDA `(emitter_chain, sender)`.
///  10. `[]`              TransceiverPeer PDA `(emitter_chain, sender, recipient_chain)`.
///  11. `[]`              TransceiverPeer PDA `(recipient_chain, source_peer, emitter_chain)`.
///  12. `[WRITE]`         source balance `(emitter_chain, hub_chain, hub_address)`.
///  13. `[WRITE]`         dest balance `(recipient_chain, hub_chain, hub_address)`.
///
/// The transfer accounts are only touched on the quorum-completing branch.
pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    // `tx_hash` is the source-chain transaction id; with the body it reconstructs
    // the exact observation the guardian signed.
    let (ix, body_bytes) = split_body::<SubmitObservationsIxData>(data).map_err(err)?;
    if body_bytes.len() < BODY_MIN_LEN {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let tx_hash = &ix.tx_hash;

    let mut parsed = ParsedObservation::from_ix(ix);

    // Dedup/quorum identity: the VAA-body digest. Keys the pending PDA, the
    // NoReplay slot, and the commit-log record — unchanged by the signing scheme.
    parsed.digest = double_keccak256(body_bytes);

    // Routing tuple from the authenticated body header (never caller-supplied).
    parsed.populate_routing_from_body(body_bytes)?;

    // Signature digest: the domain-separated NTT observation digest the guardian
    // signed, keccak256(NTT prefix ‖ tx_hash ‖ body). Distinct from `parsed.digest`
    // and from the WTT prefix so signatures never cross products.
    let signing_digest =
        observation_signing_digest(NTT_SUBMIT_OBSERVATION_PREFIX, tx_hash, body_bytes);

    let [submitter, pending_pda, guardian_set, noreplay_bucket, system_program_acc, noreplay_program, noreplay_authority, rent_recipient, transfer_accounts @ ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !submitter.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    // NoReplay pre-check rejects replays before any signature work.
    if noreplay::is_marked(
        noreplay_bucket,
        noreplay_authority.key,
        parsed.chain,
        &parsed.emitter,
        parsed.sequence,
    )? {
        return Err(err(GlobalAccountantError::AlreadyAccounted));
    }

    // Verify the signature and learn the live guardian-set size, then derive the
    // quorum threshold from it — the set can be resized by governance, so a pinned
    // threshold would desync this path from the network (and from the shim path).
    let num_guardians = quorum::verify_signature(
        guardian_set,
        parsed.guardian_set_index,
        parsed.guardian_index,
        &signing_digest,
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

    // ----- Quorum reached: commit atomically -----
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

    // NTT transfer flow against the six trailing transfer accounts. Any error
    // (missing hub, peer mismatch, malformed payload) rolls back the NoReplay
    // mark with the tx, leaving the slot unconsumed for a future upgrade.
    apply_ntt_transfer(
        program_id,
        submitter,
        transfer_accounts,
        parsed.chain,
        &parsed.emitter,
        body_bytes,
    )?;

    let recorded_payer = layout.payer;
    quorum::close_pending_pda(pending_pda, rent_recipient, &recorded_payer)?;
    Ok(())
}
