//! `register_chain`: Token Bridge `RegisterChain` governance. Writes or overwrites the
//! `ChainRegistration` PDA. NoReplay blocks an exact replay; the stored governance sequence
//! blocks an older registration VAA that was never applied here.
//! Accepts target chain `0` (Any) or `SOLANA_CHAIN_ID`.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;
use anchor_lang::solana_program::system_program;

use accountant_operational_core::cpi::{noreplay, shim};
use accountant_operational_core::hash::double_keccak256;
use accountant_operational_core::support::pda_init::create_pda_allow_prefund;
use accountant_operational_core::{ProgramCoreResult, ProgramResult};

use crate::definitions::{
    split_body, ChainRegistrationLayout, GlobalAccountantError, NoReplayNamespace,
    RegisterChainIxData, RegisterChainPayload, VaaBodyHeader, CHAIN_REGISTRATION_SEED_PREFIX,
    GOVERNANCE_EMITTER, SOLANA_CHAIN_ID,
};
use crate::err;
use accountant_operational_core::accounts::chain_registration;

/// A `RegisterChain` body is exactly header + payload.
const REGISTER_CHAIN_BODY_LEN: usize = VaaBodyHeader::LEN + RegisterChainPayload::LEN;

/// Order: instruction framing, signer, Shim signature check, governance validation,
/// NoReplay pre-check, PDA check, sequence check, write registration, NoReplay mark.
pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let (ix, body) = parse_instruction(data)?;

    // Accounts:
    //   0. `[WRITE, SIGNER]` payer
    //   1. `[]`              Verify VAA Shim program
    //   2. `[]`              Core Bridge `GuardianSet` PDA
    //   3. `[]`              `GuardianSignatures` PDA
    //   4. `[WRITE]`         `ChainRegistration` PDA
    //   5. `[WRITE]`         NoReplay bitmap PDA
    //   6. `[]`              NoReplay program
    //   7. `[]`              NoReplay authority PDA
    //   8. `[]`              system program
    let [payer, _verify_vaa_shim_program, guardian_set, guardian_signatures, registration_pda, noreplay_bucket, _noreplay_program, noreplay_authority, system_program_acc] =
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

    let (header, payload) = RegisterChainPayload::from_body(body).map_err(err)?;
    payload.validate(header).map_err(err)?;
    let sequence = header.sequence();

    if noreplay::is_marked(
        noreplay_bucket,
        program_id,
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        sequence,
    )? {
        return Err(err(GlobalAccountantError::AlreadyAccounted));
    }

    let registration_bump = check_registration_pda(program_id, registration_pda, payload.chain())?;
    write_registration(
        program_id,
        payer,
        registration_pda,
        registration_bump,
        payload,
        sequence,
    )?;

    noreplay::mark_used(
        payer,
        noreplay_bucket,
        noreplay_authority,
        system_program_acc,
        program_id,
        &NoReplayNamespace::new(SOLANA_CHAIN_ID, GOVERNANCE_EMITTER),
        sequence,
    )?;

    Ok(())
}

/// Instruction data: [`RegisterChainIxData`] prefix then an exact-length body.
fn parse_instruction(data: &[u8]) -> ProgramCoreResult<(&RegisterChainIxData, &[u8])> {
    let (ix, body) = split_body::<RegisterChainIxData>(data).map_err(err)?;
    if body.len() != REGISTER_CHAIN_BODY_LEN {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    Ok((ix, body))
}

/// `registration_pda` must be the canonical account for `chain`; returns its bump.
fn check_registration_pda(
    program_id: &Pubkey,
    registration_pda: &AccountInfo,
    chain: u16,
) -> ProgramCoreResult<u8> {
    let (expected, bump) = chain_registration::derive_pda(program_id, chain);
    if registration_pda.key != &expected {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(bump)
}

/// First registration creates the PDA. A rotation overwrites it in place when `sequence`
/// is above the stored governance sequence.
///
/// SECURITY: `RegisterChain` VAAs for every chain come from one emitter, so `sequence`
/// orders them. A lower or equal sequence is an older registration and must not roll the
/// emitter back.
fn write_registration<'info>(
    program_id: &Pubkey,
    payer: &AccountInfo<'info>,
    registration_pda: &AccountInfo<'info>,
    registration_bump: u8,
    payload: &RegisterChainPayload,
    sequence: u64,
) -> ProgramResult {
    if registration_pda.owner == &system_program::ID {
        let bump_seed = [registration_bump];
        let seeds: &[&[u8]] = &[CHAIN_REGISTRATION_SEED_PREFIX, &payload.chain, &bump_seed]; // chain BE
        create_pda_allow_prefund(
            payer,
            registration_pda,
            program_id,
            seeds,
            ChainRegistrationLayout::LEN as u64,
        )?;
    } else if registration_pda.owner != program_id
        || registration_pda.data_len() != ChainRegistrationLayout::LEN
    {
        return Err(err(GlobalAccountantError::InvalidPda));
    } else {
        let existing = chain_registration::load(registration_pda)?;
        if sequence <= existing.governance_sequence() {
            return Err(err(GlobalAccountantError::StaleRegistration));
        }
    }

    let layout = ChainRegistrationLayout::new(payload.chain(), payload.emitter_address, sequence);
    chain_registration::store(registration_pda, &layout)
}
