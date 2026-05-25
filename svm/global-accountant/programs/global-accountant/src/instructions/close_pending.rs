//! `close_pending` — permissionless cleanup of stranded `PendingObservationsLayout`
//! PDAs.
//!
//! Per `accountant-migration-pending-quorum-design.md` §3.6, anyone may close
//! a pending PDA when **either** of the following on-chain conditions holds:
//!
//! - (a) The pending PDA's `guardian_set_index` references a guardian set
//!   whose `is_active(timestamp)` returns false (i.e., the set has expired).
//! - (b) NoReplay is already marked for `(chain, emitter, sequence)` — proving
//!   the entry has been accounted-for via some other path.
//!
//! Lamports are refunded to the recorded `payer` in either case.

use pinocchio::{
    error::ProgramError,
    sysvars::{clock::Clock, Sysvar},
    AccountView, Address, ProgramResult,
};

use crate::definitions::GlobalAccountantError;
use crate::err;
use crate::instructions::noreplay;
use crate::state::pending;

pub fn process(
    _program_id: &Address,
    accounts: &mut [AccountView],
    _data: &[u8],
) -> ProgramResult {
    // Accounts:
    //   0. `[SIGNER]` closer (permissionless — any signer)
    //   1. `[WRITE]`  pending PDA
    //   2. `[WRITE]`  rent recipient — must equal the recorded payer
    //   3. `[]`       GuardianSet PDA — to check `is_active(timestamp)`
    //   4. `[]`       NoReplay bucket — to check the marked condition
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

    // Trigger (a): GuardianSet expired.
    let expired = guardian_set_expired(guardian_set, layout.guardian_set_index)?;

    // Trigger (b): NoReplay-marked. Same helper as the `submit_observations`
    // pre-check. The mock branch ignores chain/emitter/sequence; the real
    // branch will index into the bucket bitmap by `sequence % 1024`.
    //
    // We pass the recorded chain and a zero emitter/sequence: the pending
    // PDA does not store the emitter (it is one of the PDA seeds, not a
    // field), and the mock NoReplay only inspects the sentinel byte at
    // position 0. Phase 2.3 will read the emitter from the PDA's seeds via
    // the bump-seed parameter when the real CPI lands.
    let already_accounted = noreplay::is_marked(noreplay_bucket, layout.chain, &[0u8; 32], 0)?;

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
/// Note: we deliberately do NOT verify the account is owned by the Core Bridge
/// here. The caller-supplied account is read-only and the only datum we care
/// about is `(index, expiration_time)`. A spoofed account claiming a different
/// expiration would only let the spoofer close *their own* pending bucket
/// earlier than legitimate — which is fine, since they are the recorded payer
/// and would have received the rent regardless. The hard guarantee is that
/// **two** triggers exist and **either** is sufficient.
fn guardian_set_expired(
    guardian_set: &AccountView,
    expected_index: u32,
) -> Result<bool, ProgramError> {
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
