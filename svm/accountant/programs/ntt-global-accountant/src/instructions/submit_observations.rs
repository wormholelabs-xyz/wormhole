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

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

use accountant_operational_core::hash::double_keccak256;
use accountant_operational_core::instructions::quorum::{
    ParsedObservation, BODY_MIN_LEN, SUBMIT_FIXED_LEN,
};
use accountant_operational_core::instructions::{commit_log, noreplay, quorum};

use crate::definitions::GlobalAccountantError;
use crate::err;
use crate::instructions::ntt_transfer::apply_ntt_transfer;

/// NTT `submit_observations`. Instruction-data wire format is identical to WTT
/// (see [`accountant_operational_core::instructions::quorum::SUBMIT_FIXED_LEN`]):
/// `digest(32) ‖ guardian_set_index(u32 LE) ‖ guardian_index(1) ‖ signature(65)
/// ‖ body_len(u16 LE) ‖ body`.
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
pub fn process(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    // ----- Parse the instruction data -----
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

    // Verify the body matches the signed digest before any state mutation.
    let computed = double_keccak256(body_bytes);
    if computed != parsed.digest {
        return Err(err(GlobalAccountantError::BodyDigestMismatch));
    }

    // Routing tuple from the authenticated body header (never caller-supplied).
    parsed.populate_routing_from_body(body_bytes)?;

    let [submitter, pending_pda, guardian_set, noreplay_bucket, system_program_acc, noreplay_program, noreplay_authority, rent_recipient, transfer_accounts @ ..] =
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

    quorum::verify_signature(
        guardian_set,
        parsed.guardian_set_index,
        parsed.guardian_index,
        &parsed.digest,
        &parsed.signature,
    )?;

    let action = quorum::decide_pending_action(pending_pda, &parsed)?;
    let (layout, quorum_reached) =
        quorum::apply_action_and_accumulate(program_id, submitter, pending_pda, &parsed, action)?;

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
