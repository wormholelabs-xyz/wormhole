//! `submit_observations`: WTT quorum tracker over
//! [`accountant_operational_core::support::quorum`].
//!
//! A `(chain, emitter, sequence, guardian_set_index, digest)` pending PDA accumulates
//! guardian signatures. The quorum-completing observation marks NoReplay, emits the
//! commit log, applies balances, and closes the pending PDA. Another guardian set or
//! another digest uses its own sibling PDA; `close_pending` reclaims the losers.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::cpi::noreplay;
use accountant_operational_core::hash::{double_keccak256, observation_signing_digest};
use accountant_operational_core::support::commit_log;
use accountant_operational_core::support::quorum::{self, ParsedObservation};
use accountant_operational_core::ProgramResult;

use crate::definitions::{
    is_attest_action, is_transfer_action, GlobalAccountantError, NoReplayNamespace,
    PendingObservationsLayout, SubmitObservationsIxData, SUBMIT_OBSERVATION_PREFIX,
};
use crate::err;
use accountant_operational_core::accounts::chain_registration;
use accountant_operational_core::transfer;

/// `data`: `SubmitObservationsIxData`, 245 bytes fixed.
///
/// ```text
/// 0    4   guardian_set_index (LE u32)
/// 4    1   guardian_index
/// 5    65  signature (r ‖ s ‖ recovery_id)
/// 70   32  tx_hash
/// 102  1   action
/// 103  2   chain (BE u16)
/// 105  32  emitter
/// 137  8   sequence (BE u64)
/// 145  2   token_chain (BE u16)
/// 147  32  token_address
/// 179  2   recipient_chain (BE u16)
/// 181  32  amount (BE Uint256)
/// 213  32  digest
/// ```
pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let ix = SubmitObservationsIxData::from_bytes(data).map_err(err)?;
    let tx_hash = &ix.tx_hash;
    let fields = ix.fields_and_digest();

    let signing_digest = observation_signing_digest(SUBMIT_OBSERVATION_PREFIX, tx_hash, &fields);
    // Pending-PDA / commit-log key. Independent of `tx_hash`.
    let content_digest = double_keccak256(&fields);

    let parsed = ParsedObservation::from_ix(ix, content_digest);

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
    let [submitter, pending_pda, guardian_set, noreplay_bucket, system_program_acc, _noreplay_program, noreplay_authority, source_account_pda, dest_account_pda, rent_recipient, chain_registration_pda] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !submitter.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    if noreplay::is_marked(
        noreplay_bucket,
        program_id,
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
        noreplay_authority,
        system_program_acc,
        program_id,
        &NoReplayNamespace::new(parsed.chain, parsed.emitter),
        parsed.sequence,
    )?;

    commit_log::emit(
        parsed.chain,
        &parsed.emitter,
        parsed.sequence,
        &parsed.content_digest,
        parsed.guardian_set_index,
    );

    // An unknown action fails here, rolling back the NoReplay mark above.
    if is_transfer_action(parsed.action) {
        transfer::apply_transfer(
            program_id,
            submitter,
            source_account_pda,
            dest_account_pda,
            parsed.chain,
            parsed.recipient_chain,
            parsed.token_chain,
            &parsed.token_address,
            parsed.amount,
        )?;
    } else if !is_attest_action(parsed.action) {
        return Err(err(GlobalAccountantError::UnknownTokenBridgePayload));
    }

    let recorded_payer = layout.payer;
    quorum::close_pending_pda(pending_pda, rent_recipient, &recorded_payer)?;
    Ok(())
}
