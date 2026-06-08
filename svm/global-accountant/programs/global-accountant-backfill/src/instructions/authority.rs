//! Backfill authority PDA: lazy-init + signer gate.
//!
//! On first backfill ix, the PDA at `[BACKFILL_AUTHORITY_SEED_PREFIX]` is
//! allocated and the calling signer's pubkey is stamped as the authority.
//! Every subsequent backfill ix asserts the same signer is calling and that
//! the `retired` flag has not been flipped via `Retire`.

use bytemuck::Zeroable;
use pinocchio::{
    account::Ref,
    cpi::{Seed, Signer},
    error::ProgramError,
    AccountView, Address, ProgramResult,
};

use crate::state::{BackfillAuthorityLayout, BACKFILL_AUTHORITY_SEED_PREFIX};
use crate::{err, instructions::pda_init::init_or_upgrade_pda, BackfillError};

/// Lazy-init the authority PDA on first call, otherwise verify
/// `signer == authority && !retired`. The PDA's address is verified canonical
/// before any state read so a caller cannot substitute a foreign account.
pub fn require_authority_or_init(
    program_id: &Address,
    payer: &AccountView,
    authority_pda: &mut AccountView,
    system_program: &AccountView,
) -> ProgramResult {
    let _ = system_program; // captured for the `CreateAccount` CPI inside init_or_upgrade_pda
    if !payer.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    let (expected, canonical_bump) =
        Address::find_program_address(&[BACKFILL_AUTHORITY_SEED_PREFIX], program_id);
    if authority_pda.address() != &expected {
        return Err(err(BackfillError::InvalidPda));
    }

    let already_initialised =
        authority_pda.owner() != &pinocchio_system::ID || authority_pda.data_len() != 0;

    if !already_initialised {
        // First call: allocate, sign with the PDA's canonical bump, write the
        // authority record stamping the payer as authority and retired=0.
        let bump_seed = [canonical_bump];
        let seeds = [
            Seed::from(BACKFILL_AUTHORITY_SEED_PREFIX),
            Seed::from(bump_seed.as_slice()),
        ];
        let signer = Signer::from(&seeds);
        init_or_upgrade_pda(
            payer,
            authority_pda,
            program_id,
            signer,
            BackfillAuthorityLayout::LEN as u64,
        )?;

        let mut layout = BackfillAuthorityLayout::zeroed();
        layout.authority = *payer.address().as_array();
        layout.retired = 0;
        let mut data = authority_pda.try_borrow_mut()?;
        if data.len() != BackfillAuthorityLayout::LEN {
            return Err(err(BackfillError::InvalidPda));
        }
        data.copy_from_slice(bytemuck::bytes_of(&layout));
        return Ok(());
    }

    // Already initialised: enforce authority match and not-retired.
    let data: Ref<'_, [u8]> = authority_pda.try_borrow()?;
    if data.len() != BackfillAuthorityLayout::LEN {
        return Err(err(BackfillError::InvalidPda));
    }
    let layout: &BackfillAuthorityLayout = bytemuck::from_bytes(&data);
    if layout.retired != 0 {
        return Err(err(BackfillError::AuthorityRetired));
    }
    if &layout.authority != payer.address().as_array() {
        return Err(err(BackfillError::AuthorityMismatch));
    }
    Ok(())
}
