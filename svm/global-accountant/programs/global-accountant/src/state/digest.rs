use pinocchio::{account::Ref, error::ProgramError, AccountView};

use crate::definitions::{DigestAccountLayout, GlobalAccountantError, DIGEST_SEED_PREFIX};
use crate::err;

/// Build the seeds (without bump) for a digest PDA. Returned as an array
/// suitable for `Address::find_program_address` / `derive_address`.
pub fn digest_seeds<'a>(
    chain_be: &'a [u8; 2],
    emitter: &'a [u8; 32],
    sequence_be: &'a [u8; 8],
) -> [&'a [u8]; 4] {
    [DIGEST_SEED_PREFIX, chain_be, emitter, sequence_be]
}

/// Read a `DigestAccountLayout` out of an account's data. The layout is `Pod`,
/// so the cheapest correct thing is to copy it out by value — that releases
/// the underlying borrow before the caller mutates anything else.
pub fn load(account: &AccountView) -> Result<DigestAccountLayout, ProgramError> {
    let data: Ref<'_, [u8]> = account.try_borrow()?;
    if data.len() != DigestAccountLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(*bytemuck::from_bytes::<DigestAccountLayout>(&data))
}

/// Write a `DigestAccountLayout` into an account's data buffer.
pub fn store(account: &mut AccountView, value: &DigestAccountLayout) -> Result<(), ProgramError> {
    let mut data = account.try_borrow_mut()?;
    if data.len() != DigestAccountLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    data.copy_from_slice(bytemuck::bytes_of(value));
    Ok(())
}
