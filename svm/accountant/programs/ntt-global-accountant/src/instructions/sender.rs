//! NTT sender resolution, as CosmWasm `handle_ntt_vaa`: when the VAA emitter is the chain's
//! registered Standard Relayer, the transceiver is the `DeliveryInstruction` sender and the
//! message is its wrapped payload; otherwise the emitter is the transceiver.

use anchor_lang::prelude::*;

use accountant_operational_core::accounts::chain_registration;
use accountant_operational_core::ProgramCoreResult;

use crate::definitions::parse_delivery_instruction;
use crate::err;

/// The transceiver that authored `payload`, and `payload` itself.
pub struct NttMessage<'a> {
    pub sender: [u8; 32],
    pub payload: &'a [u8],
}

/// `relayer_registration_pda` must be the canonical `ChainRegistration` PDA for `chain`.
pub fn resolve<'a>(
    program_id: &Pubkey,
    relayer_registration_pda: &AccountInfo,
    chain: u16,
    emitter: &[u8; 32],
    payload: &'a [u8],
) -> ProgramCoreResult<NttMessage<'a>> {
    if chain_registration::is_registered_emitter(
        program_id,
        relayer_registration_pda,
        chain,
        emitter,
    )? {
        let delivery = parse_delivery_instruction(payload).map_err(err)?;
        return Ok(NttMessage {
            sender: delivery.sender,
            payload: delivery.inner_payload,
        });
    }
    Ok(NttMessage {
        sender: *emitter,
        payload,
    })
}
