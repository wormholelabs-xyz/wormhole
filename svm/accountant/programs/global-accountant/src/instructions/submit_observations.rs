//! `submit_observations`: WTT quorum tracker over
//! [`accountant_operational_core::instructions::quorum`].
//!
//! A `(chain, emitter, sequence, digest)` pending PDA accumulates guardian signatures.
//! The quorum-completing observation marks NoReplay, emits the commit log, applies
//! balances, and closes the pending PDA. Fork siblings with another digest use their
//! own PDA; `close_pending` reclaims the losers.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::hash::{double_keccak256, observation_signing_digest};
use accountant_operational_core::instructions::quorum::{self, ParsedObservation, BODY_MIN_LEN};
use accountant_operational_core::instructions::{commit_log, noreplay};
use accountant_operational_core::ProgramResult;

use crate::definitions::{
    split_body, GlobalAccountantError, PendingObservationsLayout, SubmitObservationsIxData,
    SUBMIT_OBSERVATION_PREFIX,
};
use crate::err;
use crate::instructions::transfer;
use accountant_operational_core::accounts::chain_registration;

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let (ix, body_bytes) = split_body::<SubmitObservationsIxData>(data).map_err(err)?;
    if body_bytes.len() < BODY_MIN_LEN {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let tx_hash = &ix.tx_hash;

    let mut parsed = ParsedObservation::from_ix(ix);

    // Dedup digest keys the pending PDA, NoReplay slot, and commit log.
    parsed.digest = double_keccak256(body_bytes);

    // SECURITY: routing tuple comes from the body header, never from caller data.
    parsed.populate_routing_from_body(body_bytes)?;

    // Signing digest differs from `parsed.digest`; see `observation_signing_digest`.
    let signing_digest = observation_signing_digest(SUBMIT_OBSERVATION_PREFIX, tx_hash, body_bytes);

    // Accounts:
    //   0. `[WRITE, SIGNER]` submitter (rent payer)
    //   1. `[WRITE]`         pending PDA
    //   2. `[]`              Core Bridge `GuardianSet` PDA
    //   3. `[WRITE]`         NoReplay bitmap PDA
    //   4. `[]`              system program
    //   5. `[]`              NoReplay program
    //   6. `[]`              NoReplay authority PDA
    //   7. `[WRITE]`         source-chain balance PDA (any account before quorum)
    //   8. `[WRITE]`         destination-chain balance PDA (as 7)
    //   9. `[WRITE]`         rent recipient; must equal the recorded payer
    //  10. `[]`              `ChainRegistration` PDA
    let [submitter, pending_pda, guardian_set, noreplay_bucket, system_program_acc, noreplay_program, noreplay_authority, source_account_pda, dest_account_pda, rent_recipient, chain_registration_pda] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !submitter.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    if noreplay::is_marked(
        noreplay_bucket,
        noreplay_authority.key,
        parsed.chain,
        &parsed.emitter,
        parsed.sequence,
    )? {
        return Err(err(GlobalAccountantError::AlreadyAccounted));
    }

    // SECURITY: a signed observation from an unregistered emitter must not move balances.
    chain_registration::verify(
        program_id,
        chain_registration_pda,
        parsed.chain,
        &parsed.emitter,
    )?;

    // Quorum derives from the live set size; governance can resize the set.
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

    // Commit: NoReplay mark, commit log, balances, pending close. Any error rolls back all.
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

    // The emitter chain is the transfer source chain.
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
