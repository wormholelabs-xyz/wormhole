//! `submit_observations`: NTT quorum tracker over
//! [`accountant_operational_core::support::quorum`].
//!
//! A `(chain, emitter, sequence, guardian_set_index, digest)` pending PDA accumulates
//! guardian signatures. The quorum-completing observation marks NoReplay, emits the commit
//! log, applies balances keyed on the sender's hub, and closes the pending PDA. Another
//! guardian set or another digest uses its own sibling PDA; `close_pending` reclaims the
//! losers.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::accounts::chain_registration;
use accountant_operational_core::cpi::noreplay;
use accountant_operational_core::hash::{double_keccak256, observation_signing_digest};
use accountant_operational_core::support::quorum::{self, ParsedObservation};
use accountant_operational_core::support::{commit_log, pda};
use accountant_operational_core::ProgramResult;

use crate::definitions::{
    normalize_trimmed_amount, GlobalAccountantError, NoReplayNamespace,
    NttSubmitObservationsIxData, PendingObservationsLayout, TransceiverHubLayout, TransceiverKey,
    NTT_SUBMIT_OBSERVATION_PREFIX,
};
use crate::err;
use crate::instructions::ntt_transfer;

/// `data`: [`NttSubmitObservationsIxData`], 219 bytes fixed.
pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let ix = NttSubmitObservationsIxData::from_bytes(data).map_err(err)?;
    let fields = ix.fields_and_digest();

    let signing_digest =
        observation_signing_digest(NTT_SUBMIT_OBSERVATION_PREFIX, &ix.tx_hash, &fields);
    // Pending-PDA / commit-log key. Independent of `tx_hash`.
    let content_digest = double_keccak256(&fields);

    let parsed = ParsedObservation {
        content_digest,
        chain: ix.chain(),
        emitter: ix.emitter,
        sequence: ix.sequence(),
        guardian_set_index: ix.guardian_set_index(),
        guardian_index: ix.guardian_index,
        signature: ix.signature,
    };

    // Accounts:
    //   0. `[WRITE, SIGNER]` submitter (rent payer)
    //   1. `[WRITE]`         pending PDA
    //   2. `[]`              Core Bridge `GuardianSet` PDA
    //   3. `[WRITE]`         NoReplay bitmap PDA
    //   4. `[]`              system program
    //   5. `[]`              NoReplay program
    //   6. `[]`              NoReplay authority PDA
    //   7. `[WRITE]`         source-chain balance PDA for the hub token
    //   8. `[WRITE]`         recipient-chain balance PDA for the hub token
    //   9. `[WRITE]`         rent recipient; must equal the recorded payer
    //  10. `[]`              relayer `ChainRegistration` PDA for the emitter chain
    //  11. `[]`              `TransceiverHub` PDA at `(chain, sender)`
    //  12. `[]`              `TransceiverPeer` PDA at `(chain, sender, recipient_chain)`
    //  13. `[]`              `TransceiverPeer` PDA at `(recipient_chain, source_peer, chain)`
    let [submitter, pending_pda, guardian_set, noreplay_bucket, system_program_acc, _noreplay_program, noreplay_authority, source_balance, dest_balance, rent_recipient, relayer_registration_pda, hub_pda, peer_src_pda, peer_dst_pda] =
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

    // SECURITY: `sender` differs from `emitter` only for a registered Standard Relayer, as in
    // `submit_vaas`. Any other combination is a malformed or forged observation.
    let relayed = chain_registration::is_registered_emitter(
        program_id,
        relayer_registration_pda,
        parsed.chain,
        &parsed.emitter,
    )?;
    if relayed != (ix.sender != parsed.emitter) {
        return Err(err(if relayed {
            GlobalAccountantError::InvalidInstructionData
        } else {
            GlobalAccountantError::UnregisteredEmitter
        }));
    }

    // SECURITY: an observation from a transceiver with no hub must not move balances. Gated
    // before signature recovery, as `submit_vaas` gates before the balance move.
    let sender_key = TransceiverKey::new(parsed.chain, ix.sender);
    pda::check(program_id, hub_pda, &sender_key)?;
    let hub = pda::read_if_initialised::<TransceiverHubLayout>(program_id, hub_pda)?
        .ok_or_else(|| err(GlobalAccountantError::MissingTransceiverHub))?
        .hub();

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

    let amount = normalize_trimmed_amount(ix.trimmed_decimals, ix.trimmed_amount())
        .ok_or_else(|| err(GlobalAccountantError::InvalidInstructionData))?;
    ntt_transfer::apply_routed(
        program_id,
        submitter,
        hub,
        peer_src_pda,
        peer_dst_pda,
        source_balance,
        dest_balance,
        parsed.chain,
        ix.sender,
        ix.recipient_chain(),
        amount,
    )?;

    quorum::close_pending_pda(pending_pda, rent_recipient, &layout.payer)
}
