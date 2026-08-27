//! Quorum primitives for `submit_observations`: instruction parse, guardian signature
//! check, pending-PDA lifecycle, bitmap accumulation, and close.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use crate::account_util::{add_lamports, close_account};
use crate::definitions::{
    parse_vaa_namespace_key, GlobalAccountantError, PendingObservationsLayout, VaaBodyHeader,
    CORE_BRIDGE_PROGRAM_ID, GUARDIAN_SET_SEED, PENDING_OBSERVATIONS_SEED_PREFIX,
};
use crate::err;
use crate::hash::keccak256;
use crate::instructions::pda_init::init_or_upgrade_pda;
use crate::state::pending;

/// Fixed prefix of `submit_observations` data (after the 1-byte discriminator):
///
/// | offset | size | field                              |
/// |--------|------|------------------------------------|
/// | 0      | 4    | guardian_set_index (little-endian) |
/// | 4      | 1    | guardian_index                     |
/// | 5      | 65   | signature (r||s||recovery_id)      |
///
/// Then: `tx_hash: [u8; 32]`, `body_len: u16 LE`, `body_len` body bytes.
/// Derived on-chain: signing digest `keccak256(prefix ‖ tx_hash ‖ body)` and
/// dedup digest `keccak256(keccak256(body))`.
///
/// SECURITY: `(chain, emitter, sequence)` comes from body header `[8..50]` only.
pub const SUBMIT_FIXED_LEN: usize = 4 + 1 + 65;

/// `r (32) ‖ s (32) ‖ recovery_id (1)`.
pub const SECP256K1_SIGNATURE_LEN: usize = 65;

/// Guardian key: `keccak256(uncompressed_pk)[12..]`.
const GUARDIAN_PUBKEY_LEN: usize = 20;

/// Header plus a non-empty payload; `payload[0]` is the Token Bridge action byte.
pub const BODY_MIN_LEN: usize = VaaBodyHeader::LEN + 1;

/// Parsed prefix plus the body-header routing tuple. Build with [`Self::from_data`],
/// set `digest`, then call [`Self::populate_routing_from_body`].
#[derive(Clone, Copy)]
pub struct ParsedObservation {
    /// `keccak256(keccak256(body))`, set by the caller.
    pub digest: [u8; 32],
    /// Body header `[8..10]`.
    pub chain: u16,
    /// Body header `[10..42]`.
    pub emitter: [u8; 32],
    /// Body header `[42..50]`.
    pub sequence: u64,
    pub guardian_set_index: u32,
    pub guardian_index: u8,
    pub signature: [u8; SECP256K1_SIGNATURE_LEN],
}

impl ParsedObservation {
    /// Parse the fixed prefix. `digest` and routing fields stay zero.
    pub fn from_data(data: &[u8; SUBMIT_FIXED_LEN]) -> crate::ProgramCoreResult<Self> {
        let (gsi_bytes, rest) = data.split_at(4);
        let guardian_index = rest[0];
        let signature_bytes = &rest[1..1 + SECP256K1_SIGNATURE_LEN];

        let gsi: [u8; 4] = gsi_bytes
            .try_into()
            .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
        let signature: [u8; SECP256K1_SIGNATURE_LEN] = signature_bytes
            .try_into()
            .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;

        Ok(Self {
            digest: [0u8; 32],
            chain: 0,
            emitter: [0u8; 32],
            sequence: 0,
            guardian_set_index: u32::from_le_bytes(gsi),
            guardian_index,
            signature,
        })
    }

    /// Set the routing tuple from the body header. `self.digest` must derive from this `body`.
    pub fn populate_routing_from_body(&mut self, body: &[u8]) -> crate::ProgramResult {
        let header = parse_vaa_namespace_key(body).map_err(err)?;
        self.chain = header.chain;
        self.emitter = header.emitter;
        self.sequence = header.sequence;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingAction {
    /// PDA absent: allocate and write a fresh layout.
    Create,
    /// PDA holds an older guardian set: refund, wipe, re-create.
    WipeAndRecreate,
    /// PDA holds the same guardian set: set the bitmap bit.
    Continue,
}

/// `InvalidPda` unless `pending_pda` is at the address for `(chain, emitter, sequence, digest)`.
fn verify_pending_pda_address(
    program_id: &Pubkey,
    pending_pda: &AccountInfo,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    digest: &[u8; 32],
) -> crate::ProgramResult {
    let chain_be = chain.to_be_bytes();
    let sequence_be = sequence.to_be_bytes();
    let (expected, _bump) = Pubkey::find_program_address(
        &[
            PENDING_OBSERVATIONS_SEED_PREFIX,
            &chain_be,
            emitter,
            &sequence_be,
            digest,
        ],
        program_id,
    );
    if pending_pda.key != &expected {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(())
}

/// Choose the [`PendingAction`] for this observation.
pub fn decide_pending_action(
    program_id: &Pubkey,
    pending_pda: &AccountInfo,
    parsed: &ParsedObservation,
) -> crate::ProgramCoreResult<PendingAction> {
    let owner_is_system = pending_pda.owner == &anchor_lang::solana_program::system_program::ID;
    let data_len = pending_pda.data_len();

    if owner_is_system && data_len == 0 {
        return Ok(PendingAction::Create);
    }
    if owner_is_system {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    verify_pending_pda_address(
        program_id,
        pending_pda,
        parsed.chain,
        &parsed.emitter,
        parsed.sequence,
        &parsed.digest,
    )?;

    let existing = pending::load(pending_pda)?;
    if existing.guardian_set_index < parsed.guardian_set_index {
        return Ok(PendingAction::WipeAndRecreate);
    }
    if existing.guardian_set_index > parsed.guardian_set_index {
        return Err(err(GlobalAccountantError::StaleGuardianSet));
    }
    Ok(PendingAction::Continue)
}

/// Apply `action`, then set this guardian's bit. Returns the layout and whether
/// the bit count reached `quorum_threshold`. Rejects a set bit with `AlreadySigned`
/// and an index >= 32 with `InvalidGuardianIndex`.
pub fn apply_action_and_accumulate<'info>(
    program_id: &Pubkey,
    submitter: &AccountInfo<'info>,
    pending_pda: &AccountInfo<'info>,
    parsed: &ParsedObservation,
    action: PendingAction,
    quorum_threshold: u32,
) -> crate::ProgramCoreResult<(PendingObservationsLayout, bool)> {
    match action {
        PendingAction::Create => {
            create_pending_pda(program_id, submitter, pending_pda, parsed)?;
        }
        PendingAction::WipeAndRecreate => {
            wipe_pending_pda(pending_pda, submitter)?;
            create_pending_pda(program_id, submitter, pending_pda, parsed)?;
        }
        PendingAction::Continue => {}
    }

    let mut layout = pending::load(pending_pda)?;
    let bit = 1u32
        .checked_shl(parsed.guardian_index as u32)
        .ok_or_else(|| err(GlobalAccountantError::InvalidGuardianIndex))?;
    if layout.signatures & bit != 0 {
        return Err(err(GlobalAccountantError::AlreadySigned));
    }
    layout.signatures |= bit;
    pending::store(pending_pda, &layout)?;

    let quorum_reached = layout.signatures.count_ones() >= quorum_threshold;
    Ok((layout, quorum_reached))
}

/// Allocate the pending PDA and write a fresh layout.
fn create_pending_pda<'info>(
    program_id: &Pubkey,
    submitter: &AccountInfo<'info>,
    pending_pda: &AccountInfo<'info>,
    parsed: &ParsedObservation,
) -> crate::ProgramResult {
    let chain_be = parsed.chain.to_be_bytes();
    let sequence_be = parsed.sequence.to_be_bytes();
    let (_expected, canonical_bump) = Pubkey::find_program_address(
        &[
            PENDING_OBSERVATIONS_SEED_PREFIX,
            &chain_be,
            &parsed.emitter,
            &sequence_be,
            &parsed.digest,
        ],
        program_id,
    );

    let bump_seed = [canonical_bump];
    let seeds: &[&[u8]] = &[
        PENDING_OBSERVATIONS_SEED_PREFIX,
        &chain_be,
        &parsed.emitter,
        &sequence_be,
        &parsed.digest,
        &bump_seed,
    ];

    init_or_upgrade_pda(
        submitter,
        pending_pda,
        program_id,
        seeds,
        PendingObservationsLayout::LEN as u64,
    )?;

    let mut layout: PendingObservationsLayout = bytemuck::Zeroable::zeroed();
    layout.tag = PendingObservationsLayout::TAG;
    layout.digest = parsed.digest;
    layout.payer = submitter.key.to_bytes();
    layout.guardian_set_index = parsed.guardian_set_index;
    layout.signatures = 0;
    layout.chain = parsed.chain;
    pending::store(pending_pda, &layout)
}

/// Refund `recorded_payer` and close the account.
pub fn close_pending_pda(
    pending_pda: &AccountInfo,
    rent_recipient: &AccountInfo,
    recorded_payer: &[u8; 32],
) -> crate::ProgramResult {
    if rent_recipient.key.to_bytes() != *recorded_payer {
        return Err(err(GlobalAccountantError::PayerMismatch));
    }
    let lamports = pending_pda.lamports();
    add_lamports(rent_recipient, lamports)?;
    close_account(pending_pda)
}

/// Close on guardian-set rotation; lamports go to `new_submitter`, not the recorded payer.
fn wipe_pending_pda(
    pending_pda: &AccountInfo,
    new_submitter: &AccountInfo,
) -> crate::ProgramResult {
    let lamports = pending_pda.lamports();
    add_lamports(new_submitter, lamports)?;
    close_account(pending_pda)
}

/// Recover the signer with `secp256k1_recover` and compare to the key in `guardian_set`.
/// Returns `keys_len` for the quorum computation.
///
/// SECURITY: `guardian_set` is the only trust anchor on this path. Owner must be the
/// Core Bridge; a forged set would let an attacker reach quorum with own keys.
pub fn verify_signature(
    guardian_set: &AccountInfo,
    expected_guardian_set_index: u32,
    guardian_index: u8,
    digest: &[u8; 32],
    signature: &[u8; SECP256K1_SIGNATURE_LEN],
) -> crate::ProgramCoreResult<u32> {
    if guardian_set.owner.to_bytes() != CORE_BRIDGE_PROGRAM_ID {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    // Address check also rejects a wrong-index set early.
    let index_be = expected_guardian_set_index.to_be_bytes();
    let core_bridge_addr = Pubkey::new_from_array(CORE_BRIDGE_PROGRAM_ID);
    let (expected_address, _) =
        Pubkey::find_program_address(&[GUARDIAN_SET_SEED, &index_be], &core_bridge_addr);
    if guardian_set.key != &expected_address {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    let data = guardian_set.try_borrow_data()?;
    let expected_key = read_guardian_key(&data, expected_guardian_set_index, guardian_index)?;
    // `read_guardian_key` checked `data.len() >= 8`.
    let num_guardians = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);

    let recovery_id = signature[64];
    if recovery_id >= 4 {
        return Err(err(GlobalAccountantError::InvalidSignature));
    }

    let recovered =
        solana_secp256k1_recover::secp256k1_recover(digest, recovery_id, &signature[..64])
            .map_err(|_| err(GlobalAccountantError::InvalidSignature))?;

    let hash = keccak256(&recovered.0);
    if hash[12..] != expected_key[..] {
        return Err(err(GlobalAccountantError::InvalidSignature));
    }
    Ok(num_guardians)
}

/// Read the guardian key at `guardian_index` from a Core Bridge `GuardianSet` account.
///
/// Layout:
///
/// | offset | size | field              |
/// |--------|------|--------------------|
/// | 0      | 4    | guardian_set_index |
/// | 4      | 4    | keys_len           |
/// | 8      | 20*N | keys               |
/// | 8+20N  | 4    | creation_time      |
/// | 12+20N | 4    | expiration_time    |
pub fn read_guardian_key(
    data: &[u8],
    expected_index: u32,
    guardian_index: u8,
) -> crate::ProgramCoreResult<[u8; GUARDIAN_PUBKEY_LEN]> {
    if data.len() < 8 {
        return Err(ProgramError::InvalidAccountData);
    }
    let on_chain_index = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if on_chain_index != expected_index {
        return Err(err(GlobalAccountantError::InvalidGuardianIndex));
    }
    let keys_len = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    if (guardian_index as u32) >= keys_len {
        return Err(err(GlobalAccountantError::InvalidGuardianIndex));
    }
    let start = 8 + (guardian_index as usize) * GUARDIAN_PUBKEY_LEN;
    let end = start + GUARDIAN_PUBKEY_LEN;
    if data.len() < end {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut key = [0u8; GUARDIAN_PUBKEY_LEN];
    key.copy_from_slice(&data[start..end]);
    Ok(key)
}
