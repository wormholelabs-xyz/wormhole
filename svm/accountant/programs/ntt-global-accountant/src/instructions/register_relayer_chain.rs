//! `register_relayer_chain` — NTT `WormholeRelayer` governance handler.
//!
//! Validates a `WormholeRelayer` `RegisterChain` governance VAA and writes (or
//! upgrades) the `RelayerChainRegistration` PDA that the NTT transfer flow
//! cross-checks the relayer emitter against (relayer-detection step of
//! `handle_observation`). Re-registration uses a fresh VAA at a higher
//! sequence; the NoReplay bit only prevents re-applying a sequence that has
//! already been consumed — it does not order VAAs, so an older, never-submitted
//! governance VAA can still be applied after a newer one and revert a rotation
//! (matches CosmWasm's existing behavior). Accepts `Any (0)` or
//! `SOLANA_CHAIN_ID` as target_chain.
//!
//! Near-identical to the WTT `register_chain` handler; the divergences are the
//! governance module (`RELAYER_GOVERNANCE_MODULE` rather than the Token Bridge
//! module) and the destination layout/seed (`RelayerChainRegistrationLayout` at
//! `RELAYER_CHAIN_REGISTRATION_SEED_PREFIX`).

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::hash::double_keccak256;
use accountant_operational_core::cpi::{noreplay, shim};
use accountant_operational_core::support::pda_init::init_or_upgrade_pda;
use accountant_operational_core::ProgramResult;

use crate::definitions::{
    GlobalAccountantError, RelayerChainRegistrationLayout, GOVERNANCE_EMITTER,
    REGISTER_CHAIN_ACTION, RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, RELAYER_GOVERNANCE_MODULE,
    SOLANA_CHAIN_ID,
};
use crate::err;

/// Wire format for the `register_relayer_chain` instruction data (after the
/// 1-byte dispatch discriminator):
///
/// | offset | size     | field             |
/// |--------|----------|-------------------|
/// | 0      | 1        | guardian_set_bump |
/// | 1      | 1        | registration_bump |
/// | 2      | 2        | body_len (LE)     |
/// | 4      | body_len | body              |
const REGISTER_CHAIN_FIXED_LEN: usize = 1 + 1 + 2;

/// Maximum VAA body size. Canonical RegisterChain body is 120 bytes; 256 leaves
/// headroom inside Solana's 1232-byte tx envelope.
const REGISTER_CHAIN_BODY_MAX: usize = 256;

/// Body header offsets (canonical Wormhole VAA layout, 51-byte header).
const BODY_EMITTER_CHAIN_OFFSET: usize = 8;
const BODY_EMITTER_ADDRESS_OFFSET: usize = 10;
const BODY_SEQUENCE_OFFSET: usize = 42;
const BODY_HEADER_LEN: usize = 51;

/// Payload byte offsets (relative to body start). `WormholeRelayer` governance
/// packet layout:
///
/// | offset | size | field             |
/// |--------|------|-------------------|
/// | 0      | 32   | module            |
/// | 32     | 1    | action            |
/// | 33     | 2    | target_chain      |
/// | 35     | 2    | chain_to_register |
/// | 37     | 32   | emitter_to_register
const PAYLOAD_MODULE_OFFSET: usize = BODY_HEADER_LEN;
const PAYLOAD_ACTION_OFFSET: usize = BODY_HEADER_LEN + 32;
const PAYLOAD_TARGET_CHAIN_OFFSET: usize = BODY_HEADER_LEN + 33;
const PAYLOAD_CHAIN_OFFSET: usize = BODY_HEADER_LEN + 35;
const PAYLOAD_EMITTER_OFFSET: usize = BODY_HEADER_LEN + 37;
const PAYLOAD_TOTAL_LEN: usize = 32 + 1 + 2 + 2 + 32;
const BODY_MIN_LEN: usize = BODY_HEADER_LEN + PAYLOAD_TOTAL_LEN;

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    // ----- (1) Parse wire data -----
    if data.len() < REGISTER_CHAIN_FIXED_LEN {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let guardian_set_bump = data[0];
    let registration_bump = data[1];
    let body_len = u16::from_le_bytes([data[2], data[3]]) as usize;
    if !(BODY_MIN_LEN..=REGISTER_CHAIN_BODY_MAX).contains(&body_len)
        || data.len() != REGISTER_CHAIN_FIXED_LEN + body_len
    {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let body_bytes = &data[REGISTER_CHAIN_FIXED_LEN..REGISTER_CHAIN_FIXED_LEN + body_len];

    // ----- (2) Compute digest -----
    let digest = double_keccak256(body_bytes);

    // Accounts:
    //   0. `[WRITE, SIGNER]` payer — rent for fresh PDAs.
    //   1. `[]`              Verify VAA Shim program (CPI target).
    //   2. `[]`              Core Bridge `GuardianSet` PDA.
    //   3. `[]`              `GuardianSignatures` PDA.
    //   4. `[WRITE]`         Relayer-chain registration PDA. Init on first call;
    //                       overwritten on emitter rotation.
    //   5. `[WRITE]`         NoReplay bitmap PDA. Pre-check then mark-used CPI.
    //   6. `[]`              NoReplay program (CPI target).
    //   7. `[]`              NoReplay authority PDA owned by this program.
    //   8. `[]`              system program.
    let [payer, _verify_vaa_shim_program, guardian_set, guardian_signatures, registration_pda, noreplay_bucket, noreplay_program, noreplay_authority, system_program_acc] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    // ----- (3) Shim CPI to verify the digest -----
    shim::verify_vaa(
        guardian_set,
        guardian_signatures,
        &digest,
        guardian_set_bump,
    )?;

    // ----- (4) Governance emitter check -----
    //
    // Pin the emitter to (chain=1, GOVERNANCE_EMITTER); reject any other even if
    // its signatures verify.
    let body_emitter_chain = u16::from_be_bytes([
        body_bytes[BODY_EMITTER_CHAIN_OFFSET],
        body_bytes[BODY_EMITTER_CHAIN_OFFSET + 1],
    ]);
    if body_emitter_chain != SOLANA_CHAIN_ID
        || body_bytes[BODY_EMITTER_ADDRESS_OFFSET..BODY_EMITTER_ADDRESS_OFFSET + 32]
            != GOVERNANCE_EMITTER
    {
        return Err(err(GlobalAccountantError::InvalidGovernanceEmitter));
    }

    // Extract sequence for NoReplay keying.
    let sequence_bytes: [u8; 8] = body_bytes[BODY_SEQUENCE_OFFSET..BODY_SEQUENCE_OFFSET + 8]
        .try_into()
        .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
    let sequence = u64::from_be_bytes(sequence_bytes);

    // ----- (5) NoReplay pre-check -----
    //
    // Reject a previously-applied governance VAA, preventing replay of an old
    // `RegisterChain` to undo a later emitter rotation.
    if noreplay::is_marked(
        noreplay_bucket,
        noreplay_authority.key,
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        sequence,
    )? {
        return Err(err(GlobalAccountantError::AlreadyAccounted));
    }

    // ----- (6) Payload checks -----
    if body_bytes[PAYLOAD_MODULE_OFFSET..PAYLOAD_MODULE_OFFSET + 32] != RELAYER_GOVERNANCE_MODULE {
        return Err(err(GlobalAccountantError::InvalidGovernanceModule));
    }
    if body_bytes[PAYLOAD_ACTION_OFFSET] != REGISTER_CHAIN_ACTION {
        return Err(err(GlobalAccountantError::InvalidGovernanceAction));
    }
    let target_chain = u16::from_be_bytes([
        body_bytes[PAYLOAD_TARGET_CHAIN_OFFSET],
        body_bytes[PAYLOAD_TARGET_CHAIN_OFFSET + 1],
    ]);
    // Accepts `Any (0)` or `Solana`.
    if target_chain != 0 && target_chain != SOLANA_CHAIN_ID {
        return Err(err(GlobalAccountantError::GovernanceChainMismatch));
    }

    // ----- (7) Extract registration payload -----
    let chain_to_register = u16::from_be_bytes([
        body_bytes[PAYLOAD_CHAIN_OFFSET],
        body_bytes[PAYLOAD_CHAIN_OFFSET + 1],
    ]);
    let mut emitter_to_register = [0u8; 32];
    emitter_to_register
        .copy_from_slice(&body_bytes[PAYLOAD_EMITTER_OFFSET..PAYLOAD_EMITTER_OFFSET + 32]);

    // ----- (8) Canonical PDA enforcement -----
    //
    // Verify the account is at the canonical address and the supplied bump is
    // canonical (surfaces our own error before the init CPI would fail).
    let chain_be = chain_to_register.to_be_bytes();
    let (expected_pda, canonical_bump) = Pubkey::find_program_address(
        &[RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, &chain_be],
        program_id,
    );
    if registration_pda.key != &expected_pda || registration_bump != canonical_bump {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    // ----- (9) Init-or-upgrade dispatch -----
    //
    // First registration (system-owned): Allocate + Assign. Rotation (already
    // program-owned, correct length): overwrite in place. Any other shape rejects.
    let owner_is_system = registration_pda.owner == &anchor_lang::solana_program::system_program::ID;
    if owner_is_system {
        let bump_seed = [registration_bump];
        let seeds: &[&[u8]] = &[RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, &chain_be, &bump_seed];
        init_or_upgrade_pda(
            payer,
            registration_pda,
            program_id,
            seeds,
            RelayerChainRegistrationLayout::LEN as u64,
        )?;
    } else {
        if registration_pda.owner != program_id {
            return Err(err(GlobalAccountantError::InvalidPda));
        }
        if registration_pda.data_len() != RelayerChainRegistrationLayout::LEN {
            return Err(err(GlobalAccountantError::InvalidPda));
        }
    }

    // ----- (10) Write the new registration layout -----
    let mut layout: RelayerChainRegistrationLayout = bytemuck::Zeroable::zeroed();
    layout.tag = RelayerChainRegistrationLayout::TAG;
    layout.chain = chain_to_register;
    layout.emitter_address = emitter_to_register;
    // `_pad0` / `_padding` left zero — crate-private to definitions.
    {
        let mut data_mut = registration_pda.try_borrow_mut_data()?;
        if data_mut.len() != RelayerChainRegistrationLayout::LEN {
            return Err(err(GlobalAccountantError::InvalidPda));
        }
        data_mut.copy_from_slice(bytemuck::bytes_of(&layout));
    }

    // ----- (11) NoReplay mark-used -----
    //
    // Claim the sequence slot; a racing tx surfaces as the inner noreplay
    // program's `AccountAlreadyInitialized` — the SBF runtime propagates it
    // directly (see `noreplay::mark_used` comment).
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
