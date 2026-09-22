//! `register_chain`: `RegisterChain` governance for the caller's module (Token Bridge for
//! WTT, Wormhole Relayer for NTT). Writes or overwrites the `ChainRegistration` PDA. A
//! per-sequence `RegisterChain` PDA is the replay guard.
//!
//! SECURITY: governance sequence numbers are assigned at random, not monotonically, so this
//! instruction accepts any VAA on an unused sequence and overwrites the registration
//! unconditionally. Registration recency rests entirely on the guardian network issuing one
//! valid `RegisterChain` VAA per registration event.
//! Accepts the target chains in `ACCEPTED_REGISTER_CHAIN_TARGETS`: `0` (Any), Solana,
//! and Wormchain for the migration window.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use crate::accounts::chain_registration;
use crate::cpi::shim;
use crate::definitions::{
    split_body, ChainRegistrationKey, ChainRegistrationLayout, GlobalAccountantError,
    GovernanceModule, RegisterChainIxData, RegisterChainKey, RegisterChainLayout,
    RegisterChainPayload, VaaBodyHeader,
};
use crate::hash::double_keccak256;
use crate::support::pda;
use crate::{err, ProgramCoreResult, ProgramResult};

/// A `RegisterChain` body is exactly header + payload.
const REGISTER_CHAIN_BODY_LEN: usize = VaaBodyHeader::LEN + RegisterChainPayload::LEN;

/// Order: instruction framing, signer, Shim signature check, governance validation against
/// `module`, PDA checks, write registration, registration record.
pub fn process(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
    module: &GovernanceModule,
) -> ProgramResult {
    let (ix, body) = parse_instruction(data)?;

    // Accounts:
    //   0. `[WRITE, SIGNER]` payer
    //   1. `[]`              Verify VAA Shim program
    //   2. `[]`              Core Bridge `GuardianSet` PDA
    //   3. `[]`              `GuardianSignatures` PDA
    //   4. `[WRITE]`         `ChainRegistration` PDA
    //   5. `[]`              system program
    //   6. `[WRITE]`         `RegisterChain` PDA
    let [payer, _verify_vaa_shim_program, guardian_set, guardian_signatures, registration_pda, _system_program, register_chain_pda] =
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
    payload.validate(header, module).map_err(err)?;
    let sequence = header.sequence();

    let register_bump = check_register_chain_pda(program_id, register_chain_pda, sequence)?;
    let registration_bump = check_registration_pda(program_id, registration_pda, payload.chain())?;

    write_registration(
        program_id,
        payer,
        registration_pda,
        registration_bump,
        payload,
        sequence,
    )?;

    record_register_chain(
        program_id,
        payer,
        register_chain_pda,
        register_bump,
        payload,
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
    pda::check(
        program_id,
        registration_pda,
        &ChainRegistrationKey::new(chain),
    )
}

/// `register_chain_pda` must be the canonical account for `sequence` and must not exist yet
/// (replay guard); returns its bump.
fn check_register_chain_pda(
    program_id: &Pubkey,
    register_chain_pda: &AccountInfo,
    sequence: u64,
) -> ProgramCoreResult<u8> {
    pda::check_uninitialised(
        program_id,
        register_chain_pda,
        &RegisterChainKey::new(sequence),
        GlobalAccountantError::DuplicateRegisterChain,
    )
}

/// `(b"register_chain", sequence_be)`.
pub fn derive_register_chain_pda(program_id: &Pubkey, sequence: u64) -> (Pubkey, u8) {
    pda::derive(program_id, &RegisterChainKey::new(sequence))
}

/// First registration creates the PDA; every later call overwrites it in place. Acceptance
/// is gated solely by `check_register_chain_pda`'s replay guard — see the module-level
/// `SECURITY` note.
fn write_registration<'info>(
    program_id: &Pubkey,
    payer: &AccountInfo<'info>,
    registration_pda: &AccountInfo<'info>,
    registration_bump: u8,
    payload: &RegisterChainPayload,
    sequence: u64,
) -> ProgramResult {
    let layout = ChainRegistrationLayout::new(payload.chain(), payload.emitter_address, sequence);
    if pda::is_initialised(program_id, registration_pda)? {
        return chain_registration::store(registration_pda, &layout);
    }
    pda::create(
        program_id,
        payer,
        registration_pda,
        &layout.key(),
        registration_bump,
        &layout,
    )
}

/// Create the `RegisterChain` PDA and write the audit record.
fn record_register_chain<'info>(
    program_id: &Pubkey,
    payer: &AccountInfo<'info>,
    register_chain_pda: &AccountInfo<'info>,
    register_bump: u8,
    payload: &RegisterChainPayload,
    sequence: u64,
) -> ProgramResult {
    let record = RegisterChainLayout::new(payload.chain(), payload.emitter_address, sequence);
    pda::create(
        program_id,
        payer,
        register_chain_pda,
        &record.key(),
        register_bump,
        &record,
    )
}
