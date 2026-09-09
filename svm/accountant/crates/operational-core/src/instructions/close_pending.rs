//! `close_pending`: permissionless close of a `PendingObservationsLayout` PDA.
//! Refunds lamports to the recorded `payer` when either condition holds:
//!
//! - (a) The recorded guardian set has expired.
//! - (b) NoReplay is marked for `(chain, emitter, sequence)`.
//!
//! Wire format (after the 1-byte discriminator):
//!
//! | offset | size | field                  |
//! |--------|------|------------------------|
//! | 0      | 32   | emitter                |
//! | 32     | 8    | sequence (big endian)  |

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use crate::account_util::add_lamports;
use crate::accounts;
use crate::cpi::noreplay;
use crate::definitions::{
    ClosePendingIxData, GlobalAccountantError, PendingObservationsLayout,
    NOREPLAY_AUTHORITY_SEED_PREFIX,
};
use crate::err;
use crate::support::guardian_set;
use crate::ProgramResult;

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let ix = ClosePendingIxData::from_bytes(data).map_err(err)?;
    let emitter = ix.emitter;
    let sequence = ix.sequence();

    // 0 closer (signer), 1 pending PDA (w), 2 rent recipient (w), 3 GuardianSet, 4 NoReplay bucket.
    let [closer, pending_pda, rent_recipient, guardian_set, noreplay_bucket] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !closer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    let layout = accounts::load::<PendingObservationsLayout>(pending_pda)?;
    let recorded_payer = layout.payer;
    if rent_recipient.key.to_bytes() != recorded_payer {
        return Err(err(GlobalAccountantError::PayerMismatch));
    }

    // SECURITY: re-derive the pending PDA from the fields
    let (expected_pending_pda, _) = crate::support::quorum::derive_pending_pda(
        program_id,
        layout.chain,
        &emitter,
        sequence,
        layout.guardian_set_index,
        &layout.digest,
    );
    if pending_pda.key != &expected_pending_pda {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    // Condition (a)
    guardian_set::verify_account(guardian_set, layout.guardian_set_index)?;
    let expired = guardian_set::is_expired(guardian_set)?;

    // Condition (b)
    let (noreplay_authority_addr, _) =
        Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], program_id);
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
    add_lamports(rent_recipient, lamports)?;
    crate::account_util::close_account(pending_pda)
}
