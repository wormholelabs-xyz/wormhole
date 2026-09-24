//! `register_peer`: a `WormholeTransceiverRegistration` VAA registers the sender's peer on
//! `dest_chain`, as the CosmWasm NTT accountant. The sender is the emitter, or the
//! `DeliveryInstruction` sender when the emitter is the chain's registered Standard Relayer.
//!
//! The peer must be on another chain and must not be registered yet. Then, on
//! `(sender_hub, peer_hub)`:
//!
//! - `(None, None)`: `MissingTransceiverHub`.
//! - `(Some, None)`: only a hub registers a hubless peer (`HublessPeerRequiresHub`).
//! - `(None, Some)`: the peer must be a hub itself (`PeerBeforeHub`) and must already list
//!   the sender as its peer on the sender's chain (`HubHasNotRegisteredPeer`); the sender
//!   then adopts that hub.
//! - `(Some, Some)`: the hubs must be equal (`PeerRegistrationMismatch`).
//!
//! SECURITY: the acknowledgement check in the adoption arm stops a rogue transceiver from
//! inheriting a legitimate hub and draining its balance through cross-registered peers.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::cpi::{noreplay, shim};
use accountant_operational_core::hash::double_keccak256;
use accountant_operational_core::{ProgramCoreResult, ProgramResult};

use accountant_operational_core::support::pda;

use crate::definitions::{
    split_body, BelongsToHub, GlobalAccountantError, NoReplayNamespace, RegisterPeerIxData,
    TransceiverHubKey, TransceiverHubLayout, TransceiverPeerKey, TransceiverPeerLayout,
    TransceiverRegistrationPayload, VaaBodyHeader, MAX_NTT_PAYLOAD_LEN,
};
use crate::err;
use crate::instructions::sender;

/// Header plus the largest payload the parsers accept.
const MAX_BODY_LEN: usize = VaaBodyHeader::LEN + MAX_NTT_PAYLOAD_LEN;

/// Order: instruction framing, signer, Shim signature check, NoReplay pre-check, sender
/// resolution, payload view, same-chain check, PDA checks, hub match, peer write, NoReplay
/// mark.
pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let (ix, body) = parse_instruction(data)?;

    // Accounts:
    //   0. `[WRITE, SIGNER]` payer
    //   1. `[]`              Verify VAA Shim program
    //   2. `[]`              Core Bridge `GuardianSet` PDA
    //   3. `[]`              `GuardianSignatures` PDA
    //   4. `[]`              relayer `ChainRegistration` PDA for the emitter chain
    //   5. `[WRITE]`         sender's `TransceiverHub` PDA at `(chain, sender)`; written only
    //                        when adopting
    //   6. `[]`              peer's `TransceiverHub` PDA at `(dest_chain, peer)`
    //   7. `[]`              peer's `TransceiverPeer` PDA at `(dest_chain, peer, chain)`
    //   8. `[WRITE]`         `TransceiverPeer` PDA at `(chain, sender, dest_chain)`
    //   9. `[WRITE]`         NoReplay bitmap PDA
    //  10. `[]`              NoReplay program
    //  11. `[]`              NoReplay authority PDA
    //  12. `[]`              system program
    let [payer, _verify_vaa_shim_program, guardian_set, guardian_signatures, relayer_registration_pda, own_hub_pda, peer_hub_pda, hub_peer_pda, peer_pda, noreplay_bucket, _noreplay_program, noreplay_authority, system_program_acc] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    shim::verify_vaa(
        guardian_set,
        guardian_signatures,
        &double_keccak256(body),
        ix.guardian_set_bump,
    )?;

    let (header, payload) = VaaBodyHeader::split(body).map_err(err)?;
    let vaa_chain = header.emitter_chain();
    let vaa_emitter = header.emitter_address;
    let vaa_sequence = header.sequence();

    noreplay::reject_if_marked(
        noreplay_bucket,
        program_id,
        vaa_chain,
        &vaa_emitter,
        vaa_sequence,
    )?;

    let message = sender::resolve(
        program_id,
        relayer_registration_pda,
        vaa_chain,
        &vaa_emitter,
        payload,
    )?;
    let sender = message.sender;
    let registration =
        TransceiverRegistrationPayload::from_payload(message.payload).map_err(err)?;
    let dest_chain = registration.dest_chain();
    let peer_address = registration.transceiver_address;
    if dest_chain == vaa_chain {
        return Err(err(GlobalAccountantError::SameChainPeer));
    }

    let sender_key = TransceiverHubKey::new(vaa_chain, sender);
    let peer_key = TransceiverHubKey::new(dest_chain, peer_address);
    let peer_entry_key = TransceiverPeerKey::new(vaa_chain, sender, dest_chain);
    let hub_entry_key = TransceiverPeerKey::new(dest_chain, peer_address, vaa_chain);

    let peer_bump = pda::check_uninitialised(
        program_id,
        peer_pda,
        &peer_entry_key,
        GlobalAccountantError::DuplicateTransceiverPeer,
    )?;
    let own_hub_bump = pda::check(program_id, own_hub_pda, &sender_key)?;
    pda::check(program_id, peer_hub_pda, &peer_key)?;
    pda::check(program_id, hub_peer_pda, &hub_entry_key)?;

    let sender_hub = pda::read_if_initialised::<TransceiverHubLayout>(program_id, own_hub_pda)?
        .map(|entry| entry.hub());
    let peer_hub = pda::read_if_initialised::<TransceiverHubLayout>(program_id, peer_hub_pda)?
        .map(|entry| entry.hub());
    match (sender_hub, peer_hub) {
        (None, None) => return Err(err(GlobalAccountantError::MissingTransceiverHub)),
        (Some(sender_hub), None) => {
            if sender_hub != sender_key {
                return Err(err(GlobalAccountantError::HublessPeerRequiresHub));
            }
        }
        (None, Some(peer_hub)) => {
            if peer_hub != peer_key {
                return Err(err(GlobalAccountantError::PeerBeforeHub));
            }
            let acknowledged =
                pda::read_if_initialised::<TransceiverPeerLayout>(program_id, hub_peer_pda)?
                    .is_some_and(|entry| entry.peer_address == sender);
            if !acknowledged {
                return Err(err(GlobalAccountantError::HubHasNotRegisteredPeer));
            }
            pda::create(
                program_id,
                payer,
                own_hub_pda,
                &sender_key,
                own_hub_bump,
                &TransceiverHubLayout::new(sender_key, BelongsToHub(peer_hub)),
            )?;
        }
        (Some(sender_hub), Some(peer_hub)) => {
            if sender_hub != peer_hub {
                return Err(err(GlobalAccountantError::PeerRegistrationMismatch));
            }
        }
    }

    pda::create(
        program_id,
        payer,
        peer_pda,
        &peer_entry_key,
        peer_bump,
        &TransceiverPeerLayout::new(peer_entry_key, peer_address),
    )?;

    noreplay::mark_used(
        payer,
        noreplay_bucket,
        noreplay_authority,
        system_program_acc,
        program_id,
        &NoReplayNamespace::new(vaa_chain, vaa_emitter),
        vaa_sequence,
    )
}

/// Instruction data: [`RegisterPeerIxData`] prefix then a body of at most [`MAX_BODY_LEN`].
/// The payload views enforce the exact message length.
fn parse_instruction(data: &[u8]) -> ProgramCoreResult<(&RegisterPeerIxData, &[u8])> {
    let (ix, body) = split_body::<RegisterPeerIxData>(data).map_err(err)?;
    if body.len() > MAX_BODY_LEN {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    Ok((ix, body))
}
