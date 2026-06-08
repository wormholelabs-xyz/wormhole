//! `Retire` — flip the backfill authority's `retired` flag.
//!
//! Belt-and-braces kill switch the operator pulls just before
//! `solana program upgrade` to the operational program. After retirement every
//! subsequent `BackfillNoReplay` / `BackfillBalance` call rejects with
//! `AuthorityRetired` — defence in depth in case the program upgrade is
//! delayed or rolled back.
//!
//! The program bytecode replacement at upgrade is the primary protection; this
//! ix is the in-program tripwire.

use pinocchio::{account::Ref, error::ProgramError, AccountView, Address, ProgramResult};

use crate::state::{BackfillAuthorityLayout, BACKFILL_AUTHORITY_SEED_PREFIX};
use crate::{err, BackfillError};

pub fn process(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    // No data payload after the dispatch discriminator.
    if !data.is_empty() {
        return Err(err(BackfillError::InvalidInstructionData));
    }

    // Accounts:
    //   0. [SIGNER]  payer / current authority
    //   1. [WRITE]   backfill authority PDA
    let [payer, authority_pda] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !payer.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    // Canonical-PDA address check.
    let (expected, _bump) =
        Address::find_program_address(&[BACKFILL_AUTHORITY_SEED_PREFIX], program_id);
    if authority_pda.address() != &expected {
        return Err(err(BackfillError::InvalidPda));
    }

    // Reject uninitialised authority — Retire is only meaningful post-init.
    if authority_pda.owner() == &pinocchio_system::ID || authority_pda.data_len() == 0 {
        return Err(err(BackfillError::InvalidPda));
    }
    if authority_pda.owner() != program_id {
        return Err(err(BackfillError::InvalidPda));
    }

    // Read current state.
    let already_retired;
    let recorded_authority;
    {
        let data: Ref<'_, [u8]> = authority_pda.try_borrow()?;
        if data.len() != BackfillAuthorityLayout::LEN {
            return Err(err(BackfillError::InvalidPda));
        }
        let layout: &BackfillAuthorityLayout = bytemuck::from_bytes(&data);
        already_retired = layout.retired != 0;
        recorded_authority = layout.authority;
    }

    if already_retired {
        // Retire is one-shot. Re-calling after retirement returns the same
        // error backfill ixes would surface — keeps caller bookkeeping simple.
        return Err(err(BackfillError::AuthorityRetired));
    }
    if &recorded_authority != payer.address().as_array() {
        return Err(err(BackfillError::AuthorityMismatch));
    }

    // Flip the flag in place. The `_padding` field stays whatever it was on
    // first-write (likely zero).
    let mut data_mut = authority_pda.try_borrow_mut()?;
    if data_mut.len() != BackfillAuthorityLayout::LEN {
        return Err(err(BackfillError::InvalidPda));
    }
    let layout: &mut BackfillAuthorityLayout = bytemuck::from_bytes_mut(&mut data_mut);
    layout.retired = 1;

    Ok(())
}
