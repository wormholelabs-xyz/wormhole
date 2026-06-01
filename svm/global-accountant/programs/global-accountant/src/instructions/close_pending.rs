//! `close_pending` — permissionless cleanup of stranded `PendingObservationsLayout`
//! PDAs.
//!
//! Anyone may close a pending PDA when **either** of the following on-chain
//! conditions holds:
//!
//! - (a) The pending PDA's `guardian_set_index` references a guardian set
//!   whose `is_active(timestamp)` returns false (i.e., the set has expired).
//! - (b) NoReplay is already marked for `(chain, emitter, sequence)` — proving
//!   the entry has been accounted-for via some other path.
//!
//! Lamports are refunded to the recorded `payer` in either case.
//!
//! Wire format (after the 1-byte dispatch discriminator):
//!
//! | offset | size | field                            |
//! |--------|------|----------------------------------|
//! | 0      | 32   | emitter                          |
//! | 32     | 8    | sequence (big endian, matches pending-PDA seed) |
//!
//! `emitter` and `sequence` are the values used to derive the pending PDA's
//! canonical address (alongside the recorded `chain` read from the layout and
//! the recorded digest also read from the layout). The program re-derives the
//! canonical pending-PDA address from these and rejects any mismatch, and
//! re-derives the noreplay bitmap PDA address from `(authority, chain_be ‖
//! emitter, sequence / 1024)` so trigger (b) reads the *correct* bucket bit
//! rather than a caller-supplied arbitrary account.

use pinocchio::{
    error::ProgramError,
    sysvars::{clock::Clock, Sysvar},
    AccountView, Address, ProgramResult,
};

use crate::definitions::{
    GlobalAccountantError, CORE_BRIDGE_PROGRAM_ID, NOREPLAY_AUTHORITY_SEED_PREFIX,
    PENDING_SEED_PREFIX,
};
use crate::err;
use crate::instructions::noreplay;
use crate::state::pending;

/// Wire-format size of the `close_pending` instruction data after the 1-byte
/// dispatch discriminator. See module doc for the field map.
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
    //   0. `[SIGNER]` closer (permissionless — any signer)
    //   1. `[WRITE]`  pending PDA
    //   2. `[WRITE]`  rent recipient — must equal the recorded payer
    //   3. `[]`       GuardianSet PDA — to check `is_active(timestamp)`
    //   4. `[]`       NoReplay bitmap PDA — to check the marked condition.
    //                The address is re-derived inside this ix from
    //                (noreplay_authority, chain ‖ emitter, sequence / 1024)
    //                and the supplied account is rejected if the address does
    //                not match.
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

    // Canonical-pending-PDA enforcement: re-derive the address from
    // `(b"pending", chain_be, emitter, sequence_be, digest)` and refuse to act
    // on any account whose address differs. Without this check, an attacker
    // could pass a pending PDA whose layout records one `(chain, sequence)`
    // but actually lives at the address for a different `(chain, sequence)`,
    // tricking the bitmap-bit lookup into reading an unrelated bucket.
    let chain_be = layout.chain.to_be_bytes();
    let (expected_pending_pda, _) = Address::find_program_address(
        &[
            PENDING_SEED_PREFIX,
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

    // Trigger (b): NoReplay-marked. The shared `noreplay::is_marked` helper
    // re-derives the canonical bitmap PDA from
    // `(noreplay_authority, chain ‖ emitter, sequence / 1024)` and rejects any
    // caller-supplied bucket at a non-canonical address. close_pending is the
    // cold path — re-deriving the noreplay-authority PDA inline (one extra
    // `find_program_address`, ~1.5K CU) keeps the account list small.
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

/// Returns `Ok(true)` if the supplied `GuardianSet` PDA is expired at the
/// current Solana clock timestamp, OR if the supplied account does not match
/// the expected index (a wrong-set account is treated as "no longer the
/// active set" — the closer can pass any expired set to prove the trigger).
///
/// The account owner is checked against [`CORE_BRIDGE_PROGRAM_ID`] up front:
/// without this check, an attacker could construct an account at an arbitrary
/// address with bytes claiming the set is expired and repeatedly DoS any
/// pending PDA from reaching quorum. The mismatch-index sub-case of trigger
/// (a) made the attack costless (no clock manipulation needed), so the owner
/// check is load-bearing for liveness.
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
        // The supplied account is not the recorded set — treat as "not the
        // currently-active set", which is a strict superset of "expired".
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
    // `unix_timestamp` is an `i64`; cast to u32 safely. Values beyond
    // u32::MAX (~year 2106) get clamped to MAX, which is treated as "expired
    // a long time ago" — the right answer for any 2026-era set.
    let timestamp_u32 = if timestamp < 0 {
        0
    } else if (timestamp as u64) > (u32::MAX as u64) {
        u32::MAX
    } else {
        timestamp as u32
    };
    Ok(timestamp_u32 > expiration_time)
}
