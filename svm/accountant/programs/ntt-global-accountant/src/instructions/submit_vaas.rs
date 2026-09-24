//! `submit_vaas`: permissionless signed-VAA path for NTT transfers. Checks the VAA through
//! the Shim, resolves the sender through the relayer registry, gates on the sender's hub, and
//! applies balances keyed on that hub. Shares NoReplay state with `submit_observations`, so
//! each `(chain, emitter, sequence)` commits once on either path.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::cpi::{noreplay, shim};
use accountant_operational_core::hash::double_keccak256;
use accountant_operational_core::support::{commit_log, pda};
use accountant_operational_core::ProgramResult;

use crate::definitions::{
    parse_ntt_transfer, split_body, GlobalAccountantError, NoReplayNamespace, SubmitVaasIxData,
    TransceiverHubKey, TransceiverHubLayout, VaaBodyHeader,
};
use crate::err;
use crate::instructions::{ntt_transfer, sender};

/// Order: Shim check, NoReplay pre-check, sender resolution, transfer parse, hub gate,
/// NoReplay mark, commit log, peer checks and balance apply.
pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let (ix, body_bytes) = split_body::<SubmitVaasIxData>(data).map_err(err)?;
    if body_bytes.len() <= VaaBodyHeader::LEN {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let digest = double_keccak256(body_bytes);

    // Accounts:
    //   0. `[WRITE, SIGNER]` submitter (rent payer)
    //   1. `[]`              Verify VAA Shim program
    //   2. `[]`              Core Bridge `GuardianSet` PDA
    //   3. `[]`              `GuardianSignatures` PDA
    //   4. `[WRITE]`         NoReplay bitmap PDA
    //   5. `[]`              NoReplay program
    //   6. `[]`              NoReplay authority PDA
    //   7. `[WRITE]`         source-chain balance PDA for the hub token
    //   8. `[WRITE]`         recipient-chain balance PDA for the hub token
    //   9. `[]`              system program
    //  10. `[]`              relayer `ChainRegistration` PDA for the emitter chain
    //  11. `[]`              `TransceiverHub` PDA at `(chain, sender)`
    //  12. `[]`              `TransceiverPeer` PDA at `(chain, sender, recipient_chain)`
    //  13. `[]`              `TransceiverPeer` PDA at `(recipient_chain, source_peer, chain)`
    let [submitter, _verify_vaa_shim_program, guardian_set, guardian_signatures, noreplay_bucket, _noreplay_program, noreplay_authority, source_balance, dest_balance, system_program_acc, relayer_registration_pda, hub_pda, peer_src_pda, peer_dst_pda] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !submitter.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    let guardian_set_index = shim::verify_vaa_and_read_index(
        guardian_set,
        guardian_signatures,
        &digest,
        ix.guardian_set_bump,
    )?;

    let (header, payload) = VaaBodyHeader::split(body_bytes).map_err(err)?;
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
    let transfer = parse_ntt_transfer(message.payload).map_err(err)?;

    // SECURITY: a signed transfer from a transceiver with no hub must not move balances.
    let sender_key = TransceiverHubKey::new(vaa_chain, message.sender);
    pda::check(program_id, hub_pda, &sender_key)?;
    let hub = pda::read_if_initialised::<TransceiverHubLayout>(program_id, hub_pda)?
        .ok_or_else(|| err(GlobalAccountantError::MissingTransceiverHub))?
        .hub();

    // Mark before the balance change; a later failure rolls the mark back.
    noreplay::mark_used(
        submitter,
        noreplay_bucket,
        noreplay_authority,
        system_program_acc,
        program_id,
        &NoReplayNamespace::new(vaa_chain, vaa_emitter),
        vaa_sequence,
    )?;

    commit_log::emit(
        vaa_chain,
        &vaa_emitter,
        vaa_sequence,
        &digest,
        guardian_set_index,
    );

    ntt_transfer::apply_routed(
        program_id,
        submitter,
        hub,
        peer_src_pda,
        peer_dst_pda,
        source_balance,
        dest_balance,
        vaa_chain,
        message.sender,
        transfer.recipient_chain,
        transfer.amount,
    )
}
