//! `close_pending` — permissionless cleanup of stranded
//! `PendingObservationsLayout` PDAs.
//!
//! Anyone may close a pending PDA when either trigger holds, refunding lamports
//! to the recorded `payer`:
//!
//! - (a) The recorded guardian set has expired.
//! - (b) NoReplay is already marked for `(chain, emitter, sequence)` — the entry
//!   was accounted via another path.
//!
//! Wire format (after the 1-byte dispatch discriminator):
//!
//! | offset | size | field                  |
//! |--------|------|------------------------|
//! | 0      | 32   | emitter                |
//! | 32     | 8    | sequence (big endian)  |
//!
//! The canonical pending-PDA and noreplay-bucket addresses are re-derived from
//! these (plus the layout's `chain` / `digest`) and any mismatch is rejected.

use pinocchio::{
    error::ProgramError,
    sysvars::{clock::Clock, Sysvar},
    AccountView, Address, ProgramResult,
};

use crate::definitions::{
    GlobalAccountantError, CORE_BRIDGE_PROGRAM_ID, NOREPLAY_AUTHORITY_SEED_PREFIX,
    PENDING_OBSERVATIONS_SEED_PREFIX,
};
use crate::err;
use crate::instructions::noreplay;
use crate::state::pending;

/// `close_pending` instruction-data size (after the discriminator). See module
/// doc for the field map.
const CLOSE_PENDING_DATA_LEN: usize = 32 + 8;

pub fn process(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let data: &[u8; CLOSE_PENDING_DATA_LEN] = data
        .try_into()
        .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
    let (emitter_bytes, sequence_bytes) = data.split_at(32);
    let emitter: [u8; 32] = emitter_bytes
        .try_into()
        .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
    let sequence_be: [u8; 8] = sequence_bytes
        .try_into()
        .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
    let sequence = u64::from_be_bytes(sequence_be);

    // Accounts:
    //   0. `[SIGNER]` closer (permissionless)
    //   1. `[WRITE]`  pending PDA
    //   2. `[WRITE]`  rent recipient — must equal the recorded payer
    //   3. `[]`       GuardianSet PDA — checks expiry (trigger a)
    //   4. `[]`       NoReplay bitmap PDA — checks the marked condition (trigger b)
    let [closer, pending_pda, rent_recipient, guardian_set, noreplay_bucket] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !closer.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    let layout = pending::load(pending_pda)?;
    let recorded_payer = layout.payer;
    if rent_recipient.address().as_array() != &recorded_payer {
        return Err(err(GlobalAccountantError::PayerMismatch));
    }

    // Canonical-pending-PDA enforcement: re-derive from `(b"pending", chain_be,
    // emitter, sequence_be, digest)` and reject mismatches — otherwise a spoofed
    // layout could trick the bitmap lookup into reading an unrelated bucket.
    let chain_be = layout.chain.to_be_bytes();
    let (expected_pending_pda, _) = Address::find_program_address(
        &[
            PENDING_OBSERVATIONS_SEED_PREFIX,
            &chain_be,
            &emitter,
            &sequence_be,
            &layout.digest,
        ],
        program_id,
    );
    if pending_pda.address() != &expected_pending_pda {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    // Trigger (a): GuardianSet expired.
    let expired = guardian_set_expired(guardian_set, layout.guardian_set_index)?;

    // Trigger (b): NoReplay-marked. Re-derive the authority PDA inline rather
    // than passing it in (cold path; keeps the account list small).
    let (noreplay_authority_addr, _) =
        Address::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], program_id);
    let already_accounted = noreplay::is_marked(
        noreplay_bucket,
        &noreplay_authority_addr,
        layout.chain,
        &emitter,
        sequence,
    )?;

    if !expired && !already_accounted {
        return Err(err(GlobalAccountantError::CannotCleanup));
    }

    let lamports = pending_pda.lamports();
    let recipient_lamports = rent_recipient.lamports();
    rent_recipient.set_lamports(
        recipient_lamports
            .checked_add(lamports)
            .ok_or(ProgramError::ArithmeticOverflow)?,
    );
    pending_pda.close()
}

/// Returns `Ok(true)` if the supplied `GuardianSet` is expired, or if its index
/// does not match the recorded one (a non-current set is a superset of expired).
///
/// The owner is checked against [`CORE_BRIDGE_PROGRAM_ID`] first: without it, a
/// forged account claiming expiry could DoS any pending PDA from reaching quorum.
fn guardian_set_expired(
    guardian_set: &AccountView,
    expected_index: u32,
) -> Result<bool, ProgramError> {
    if guardian_set.owner().as_array() != &CORE_BRIDGE_PROGRAM_ID {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let data = guardian_set.try_borrow()?;
    if data.len() < 8 {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let on_chain_index = u32::from_le_bytes(
        data[..4]
            .try_into()
            .map_err(|_| err(GlobalAccountantError::InvalidPda))?,
    );
    if on_chain_index != expected_index {
        // Not the recorded set — treat as non-current (superset of expired).
        return Ok(true);
    }
    let keys_len = u32::from_le_bytes(
        data[4..8]
            .try_into()
            .map_err(|_| err(GlobalAccountantError::InvalidPda))?,
    );
    let trailer_offset = 8 + (keys_len as usize) * 20;
    if data.len() < trailer_offset + 8 {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let expiration_time = u32::from_le_bytes(
        data[trailer_offset + 4..trailer_offset + 8]
            .try_into()
            .map_err(|_| err(GlobalAccountantError::InvalidPda))?,
    );
    if expiration_time == 0 {
        return Ok(false); // never-expiring set (the active one)
    }
    let timestamp = Clock::get()?.unix_timestamp;
    // Clamp the i64 timestamp into u32 (negatives -> 0, overflow -> MAX).
    let timestamp_u32 = if timestamp < 0 {
        0
    } else if (timestamp as u64) > (u32::MAX as u64) {
        u32::MAX
    } else {
        timestamp as u32
    };
    Ok(timestamp_u32 > expiration_time)
}
