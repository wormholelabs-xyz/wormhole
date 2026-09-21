//! `register_hub`: a Locking-mode `WormholeTransceiverInfo` VAA registers its sender as a
//! hub pointing at itself, as the CosmWasm NTT accountant. The sender is the emitter, or the
//! `DeliveryInstruction` sender when the emitter is the chain's registered Standard Relayer.
//! Guardian quorum is the only authentication. NoReplay on the emitter's sequence guards
//! replay, and a Burning-mode message leaves the slot free.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::cpi::{noreplay, shim};
use accountant_operational_core::hash::double_keccak256;
use accountant_operational_core::{ProgramCoreResult, ProgramResult};

use accountant_operational_core::support::pda;

use crate::definitions::{
    split_body, GlobalAccountantError, ManagerMode, NoReplayNamespace, RegisterHubIxData,
    TransceiverHubLayout, TransceiverInfo, TransceiverKey, VaaBodyHeader, MAX_NTT_PAYLOAD_LEN,
};
use crate::err;
use crate::instructions::sender;

/// Header plus the largest payload the parsers accept.
const MAX_BODY_LEN: usize = VaaBodyHeader::LEN + MAX_NTT_PAYLOAD_LEN;

/// Order: instruction framing, signer, Shim signature check, NoReplay pre-check, sender
/// resolution, payload check, PDA checks, hub write, NoReplay mark.
pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let (ix, body) = parse_instruction(data)?;

    // Accounts:
    //   0. `[WRITE, SIGNER]` payer
    //   1. `[]`              Verify VAA Shim program
    //   2. `[]`              Core Bridge `GuardianSet` PDA
    //   3. `[]`              `GuardianSignatures` PDA
    //   4. `[]`              relayer `ChainRegistration` PDA for the emitter chain
    //   5. `[WRITE]`         `TransceiverHub` PDA for the sender
    //   6. `[WRITE]`         NoReplay bitmap PDA
    //   7. `[]`              NoReplay program
    //   8. `[]`              NoReplay authority PDA
    //   9. `[]`              system program
    let [payer, _verify_vaa_shim_program, guardian_set, guardian_signatures, relayer_registration_pda, hub_pda, noreplay_bucket, _noreplay_program, noreplay_authority, system_program_acc] =
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
    let chain = header.emitter_chain();
    let emitter = header.emitter_address;
    let sequence = header.sequence();

    if noreplay::is_marked(noreplay_bucket, program_id, chain, &emitter, sequence)? {
        return Err(err(GlobalAccountantError::AlreadyAccounted));
    }

    let message = sender::resolve(
        program_id,
        relayer_registration_pda,
        chain,
        &emitter,
        payload,
    )?;
    let info = TransceiverInfo::from_payload(message.payload).map_err(err)?;
    if info.mode != ManagerMode::Locking {
        return Err(err(GlobalAccountantError::NotLockingHub));
    }

    let hub = TransceiverKey::new(chain, message.sender);
    let bump = pda::check_uninitialised(
        program_id,
        hub_pda,
        &hub,
        GlobalAccountantError::DuplicateTransceiverHub,
    )?;
    pda::create(
        program_id,
        payer,
        hub_pda,
        &hub,
        bump,
        &TransceiverHubLayout::new(hub, hub),
    )?;

    noreplay::mark_used(
        payer,
        noreplay_bucket,
        noreplay_authority,
        system_program_acc,
        program_id,
        &NoReplayNamespace::new(chain, emitter),
        sequence,
    )
}

/// Instruction data: [`RegisterHubIxData`] prefix then a body of at most [`MAX_BODY_LEN`].
/// The payload views enforce the exact message length.
fn parse_instruction(data: &[u8]) -> ProgramCoreResult<(&RegisterHubIxData, &[u8])> {
    let (ix, body) = split_body::<RegisterHubIxData>(data).map_err(err)?;
    if body.len() > MAX_BODY_LEN {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    Ok((ix, body))
}
