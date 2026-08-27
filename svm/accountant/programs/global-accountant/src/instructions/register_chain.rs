//! `register_chain`: Token Bridge `RegisterChain` governance. Writes or overwrites the
//! `ChainRegistration` PDA. NoReplay blocks replay of an older registration VAA.
//! Accepts target chain `0` (Any) or `SOLANA_CHAIN_ID`.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::hash::double_keccak256;
use accountant_operational_core::instructions::{noreplay, pda_init::init_or_upgrade_pda, shim};
use accountant_operational_core::ProgramResult;

use crate::definitions::{
    ChainRegistrationLayout, GlobalAccountantError, RegisterChainPayload, VaaBodyHeader,
    CHAIN_REGISTRATION_SEED_PREFIX, GOVERNANCE_EMITTER, REGISTER_CHAIN_ACTION, SOLANA_CHAIN_ID,
    TOKEN_BRIDGE_GOVERNANCE_MODULE,
};
use crate::err;
use crate::state::chain_registration;

/// Wire format after the 1-byte discriminator:
///
/// | offset | size     | field             |
/// |--------|----------|-------------------|
/// | 0      | 1        | guardian_set_bump |
/// | 1      | 1        | registration_bump |
/// | 2      | 2        | body_len (LE)     |
/// | 4      | body_len | body              |
const REGISTER_CHAIN_FIXED_LEN: usize = 1 + 1 + 2;

/// A `RegisterChain` body is exactly header + payload.
const REGISTER_CHAIN_BODY_LEN: usize = VaaBodyHeader::LEN + RegisterChainPayload::LEN;

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() < REGISTER_CHAIN_FIXED_LEN {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let guardian_set_bump = data[0];
    let registration_bump = data[1];
    let body_len = u16::from_le_bytes([data[2], data[3]]) as usize;
    if body_len != REGISTER_CHAIN_BODY_LEN || data.len() != REGISTER_CHAIN_FIXED_LEN + body_len {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let body_bytes = &data[REGISTER_CHAIN_FIXED_LEN..REGISTER_CHAIN_FIXED_LEN + body_len];

    let digest = double_keccak256(body_bytes);

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
    let [payer, _verify_vaa_shim_program, guardian_set, guardian_signatures, registration_pda, noreplay_bucket, noreplay_program, noreplay_authority, system_program_acc] =
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
        &digest,
        guardian_set_bump,
    )?;

    let (header, payload) = RegisterChainPayload::from_body(body_bytes).map_err(err)?;

    // SECURITY: the emitter must be `(chain=1, GOVERNANCE_EMITTER)`.
    if header.emitter_chain() != SOLANA_CHAIN_ID || header.emitter_address != GOVERNANCE_EMITTER {
        return Err(err(GlobalAccountantError::InvalidGovernanceEmitter));
    }
    let sequence = header.sequence();

    if noreplay::is_marked(
        noreplay_bucket,
        noreplay_authority.key,
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        sequence,
    )? {
        return Err(err(GlobalAccountantError::AlreadyAccounted));
    }

    if payload.header.module != TOKEN_BRIDGE_GOVERNANCE_MODULE {
        return Err(err(GlobalAccountantError::InvalidGovernanceModule));
    }
    if payload.header.action != REGISTER_CHAIN_ACTION {
        return Err(err(GlobalAccountantError::InvalidGovernanceAction));
    }
    let target_chain = payload.header.target_chain();
    if target_chain != 0 && target_chain != SOLANA_CHAIN_ID {
        return Err(err(GlobalAccountantError::GovernanceChainMismatch));
    }

    let chain_to_register = payload.chain();
    let emitter_to_register = payload.emitter_address;

    let chain_be = chain_to_register.to_be_bytes();
    let (expected_pda, canonical_bump) =
        Pubkey::find_program_address(&[CHAIN_REGISTRATION_SEED_PREFIX, &chain_be], program_id);
    if registration_pda.key != &expected_pda || registration_bump != canonical_bump {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    // First registration creates the PDA; rotation overwrites in place.
    let owner_is_system =
        registration_pda.owner == &anchor_lang::solana_program::system_program::ID;
    if owner_is_system {
        let bump_seed = [registration_bump];
        let seeds: &[&[u8]] = &[CHAIN_REGISTRATION_SEED_PREFIX, &chain_be, &bump_seed];
        init_or_upgrade_pda(
            payer,
            registration_pda,
            program_id,
            seeds,
            ChainRegistrationLayout::LEN as u64,
        )?;
    } else {
        if registration_pda.owner != program_id {
            return Err(err(GlobalAccountantError::InvalidPda));
        }
        if registration_pda.data_len() != ChainRegistrationLayout::LEN {
            return Err(err(GlobalAccountantError::InvalidPda));
        }
    }

    let mut layout: ChainRegistrationLayout = bytemuck::Zeroable::zeroed();
    layout.tag = ChainRegistrationLayout::TAG;
    layout.chain = chain_to_register;
    layout.emitter_address = emitter_to_register;
    chain_registration::store(registration_pda, &layout)?;

    noreplay::mark_used(
        payer,
        noreplay_bucket,
        noreplay_program,
        noreplay_authority,
        system_program_acc,
        program_id,
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        sequence,
    )?;

    Ok(())
}
