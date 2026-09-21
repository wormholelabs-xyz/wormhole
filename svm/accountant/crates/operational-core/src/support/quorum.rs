//! Quorum primitives for `submit_observations`: instruction parse, guardian signature
//! check, pending-PDA lifecycle, bitmap accumulation, and close.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use crate::account_util::{add_lamports, close_account};
use crate::accounts;
use crate::definitions::{
    GlobalAccountantError, PendingKey, PendingObservationsLayout, VaaBodyHeader,
};
use crate::err;
use crate::hash::{double_keccak256, keccak256, observation_signing_digest};
use crate::support::guardian_set::{self, GUARDIAN_PUBKEY_LEN};
use crate::support::pda;

/// The two digests of one observation over `fields = ix.fields_and_digest()`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservationDigests {
    /// `keccak256(prefix ‖ tx_hash ‖ fields)`; what the guardian signs.
    pub signing: [u8; 32],
    /// `keccak256(keccak256(fields))`; pending-PDA seed and commit-log key, independent of
    /// `tx_hash`.
    pub content: [u8; 32],
}

/// SECURITY: keep `signing` a single prefixed keccak and `content` a double keccak. The two
/// must differ from each other and from the VAA digest.
pub fn observation_digests(prefix: &[u8], tx_hash: &[u8; 32], fields: &[u8]) -> ObservationDigests {
    ObservationDigests {
        signing: observation_signing_digest(prefix, tx_hash, fields),
        content: double_keccak256(fields),
    }
}

/// `r (32) ‖ s (32) ‖ recovery_id (1)`.
pub const SECP256K1_SIGNATURE_LEN: usize = 65;

/// Header plus a non-empty payload; used to bound a staged/inline VAA body elsewhere.
pub const BODY_MIN_LEN: usize = VaaBodyHeader::LEN + 1;

/// Observation fields the quorum path reads; product fields stay on each program's ix.
#[derive(Clone, Copy)]
pub struct ParsedObservation {
    /// Pending-PDA seed and commit-log key.
    pub content_digest: [u8; 32],
    pub chain: u16,
    pub emitter: [u8; 32],
    pub sequence: u64,
    pub guardian_set_index: u32,
    pub guardian_index: u8,
    pub signature: [u8; SECP256K1_SIGNATURE_LEN],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingAction {
    /// PDA absent: allocate and write a fresh layout.
    Create,
    /// PDA present for this guardian set and digest: set the bitmap bit.
    Continue,
}

/// One record per `(chain, emitter, sequence, guardian_set_index, content_digest)`. A
/// rotation or a fork opens a sibling, keeping each guardian set's signatures in its own
/// record.
pub fn derive_pending_pda(
    program_id: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    guardian_set_index: u32,
    content_digest: &[u8; 32],
) -> (Pubkey, u8) {
    let key = PendingKey::new(
        chain,
        *emitter,
        sequence,
        guardian_set_index,
        *content_digest,
    );
    pda::derive(program_id, &key)
}

/// `InvalidPda` unless `pending_pda` is at the address for
/// `(chain, emitter, sequence, guardian_set_index, content_digest)`.
fn verify_pending_pda_address(
    program_id: &Pubkey,
    pending_pda: &AccountInfo,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    guardian_set_index: u32,
    content_digest: &[u8; 32],
) -> crate::ProgramResult {
    let key = PendingKey::new(
        chain,
        *emitter,
        sequence,
        guardian_set_index,
        *content_digest,
    );
    pda::check(program_id, pending_pda, &key)?;
    Ok(())
}

/// Choose the [`PendingAction`] for this observation.
pub fn decide_pending_action(
    program_id: &Pubkey,
    pending_pda: &AccountInfo,
    parsed: &ParsedObservation,
) -> crate::ProgramCoreResult<PendingAction> {
    if !pda::is_initialised(program_id, pending_pda)? {
        if pending_pda.data_len() != 0 {
            return Err(err(GlobalAccountantError::InvalidPda));
        }
        return Ok(PendingAction::Create);
    }

    verify_pending_pda_address(
        program_id,
        pending_pda,
        parsed.chain,
        &parsed.emitter,
        parsed.sequence,
        parsed.guardian_set_index,
        &parsed.content_digest,
    )?;

    // Redundant with the address check: the seeds already fix both fields.
    let existing = accounts::load::<PendingObservationsLayout>(pending_pda)?;
    if existing.guardian_set_index != parsed.guardian_set_index {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    if existing.content_digest != parsed.content_digest {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(PendingAction::Continue)
}

/// Apply `action`, then set this guardian's bit. Returns the layout and whether
/// the bit count reached `quorum_threshold`. Rejects a set bit with `AlreadySigned`
/// and an index >= `MAX_GUARDIANS` with `InvalidGuardianIndex`.
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
        PendingAction::Continue => {}
    }

    let mut layout = accounts::load::<PendingObservationsLayout>(pending_pda)?;
    let already_signed = layout
        .has_signature(parsed.guardian_index)
        .ok_or_else(|| err(GlobalAccountantError::InvalidGuardianIndex))?;
    if already_signed {
        return Err(err(GlobalAccountantError::AlreadySigned));
    }
    layout
        .set_signature(parsed.guardian_index)
        .ok_or_else(|| err(GlobalAccountantError::InvalidGuardianIndex))?;
    accounts::store(pending_pda, &layout)?;

    let quorum_reached = layout.num_signatures() >= quorum_threshold;
    Ok((layout, quorum_reached))
}

/// Allocate the pending PDA and write a fresh layout.
fn create_pending_pda<'info>(
    program_id: &Pubkey,
    submitter: &AccountInfo<'info>,
    pending_pda: &AccountInfo<'info>,
    parsed: &ParsedObservation,
) -> crate::ProgramResult {
    let key = PendingKey::new(
        parsed.chain,
        parsed.emitter,
        parsed.sequence,
        parsed.guardian_set_index,
        parsed.content_digest,
    );
    let (_expected, canonical_bump) = pda::derive(program_id, &key);
    let layout = PendingObservationsLayout::new(
        parsed.chain,
        parsed.guardian_set_index,
        parsed.content_digest,
        submitter.key.to_bytes(),
    );
    pda::create(
        program_id,
        submitter,
        pending_pda,
        &key,
        canonical_bump,
        &layout,
    )
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

/// Recover the signer with `secp256k1_recover` and compare to the key in `guardian_set`.
/// Returns `keys_len` for the quorum computation.
///
/// SECURITY: `guardian_set` is the only trust anchor on this path; see
/// [`guardian_set::verify_account`]. A set past its Core Bridge expiration is rejected,
/// as wormchain does (`x/wormhole/keeper/vaa.go`).
pub fn verify_signature(
    guardian_set: &AccountInfo,
    expected_guardian_set_index: u32,
    guardian_index: u8,
    digest: &[u8; 32],
    signature: &[u8; SECP256K1_SIGNATURE_LEN],
) -> crate::ProgramCoreResult<u32> {
    guardian_set::verify_account(guardian_set, expected_guardian_set_index)?;
    if guardian_set::is_expired(guardian_set)? {
        return Err(err(GlobalAccountantError::ExpiredGuardianSet));
    }

    let data = guardian_set.try_borrow_data()?;
    let num_guardians = guardian_set::keys_len(&data)?;
    // SECURITY: the pending bitmap holds MAX_GUARDIANS bits; a larger set could never commit.
    if num_guardians > PendingObservationsLayout::MAX_GUARDIANS {
        return Err(err(GlobalAccountantError::GuardianSetTooLarge));
    }
    let expected_key = read_guardian_key(&data, expected_guardian_set_index, guardian_index)?;

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

/// Read the guardian key at `guardian_index`; layout in [`guardian_set`].
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
