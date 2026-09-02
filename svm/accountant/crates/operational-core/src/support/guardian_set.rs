//! Core Bridge `GuardianSet` account checks shared by `submit_observations` and
//! `close_pending`.
//!
//! Layout:
//!
//! | offset | size | field              |
//! |--------|------|--------------------|
//! | 0      | 4    | guardian_set_index |
//! | 4      | 4    | keys_len           |
//! | 8      | 20*N | keys               |
//! | 8+20N  | 4    | creation_time      |
//! | 12+20N | 4    | expiration_time    |

use anchor_lang::prelude::*;

use crate::definitions::{GlobalAccountantError, CORE_BRIDGE_PROGRAM_ID, GUARDIAN_SET_SEED};
use crate::err;

/// Guardian key: `keccak256(uncompressed_pk)[12..]`.
pub const GUARDIAN_PUBKEY_LEN: usize = 20;

const HEADER_LEN: usize = 8;
const TRAILER_LEN: usize = 8;

/// `Ok(())` when `guardian_set` is the Core Bridge PDA for `index` and its stored index
/// agrees.
///
/// SECURITY: this is the only trust anchor for guardian keys. Owner must be the Core
/// Bridge and the address must derive from `index`; another genuine epoch is `InvalidPda`.
pub fn verify_account(guardian_set: &AccountInfo, index: u32) -> crate::ProgramCoreResult<()> {
    if guardian_set.owner.to_bytes() != CORE_BRIDGE_PROGRAM_ID {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let index_be = index.to_be_bytes();
    let core_bridge_addr = Pubkey::new_from_array(CORE_BRIDGE_PROGRAM_ID);
    let (expected_address, _) =
        Pubkey::find_program_address(&[GUARDIAN_SET_SEED, &index_be], &core_bridge_addr);
    if guardian_set.key != &expected_address {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let data = guardian_set.try_borrow_data()?;
    if data.len() < HEADER_LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let on_chain_index = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if on_chain_index != index {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(())
}

/// `keys_len` from the header. Caller must have passed [`verify_account`].
pub fn keys_len(data: &[u8]) -> crate::ProgramCoreResult<u32> {
    if data.len() < HEADER_LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(u32::from_le_bytes([data[4], data[5], data[6], data[7]]))
}

/// `Ok(true)` when `expiration_time` is nonzero and before the clock. The Core Bridge
/// sets it on rotation to `now + 24h`; the latest set keeps zero. Caller must have passed
/// [`verify_account`].
pub fn is_expired(guardian_set: &AccountInfo) -> crate::ProgramCoreResult<bool> {
    let data = guardian_set.try_borrow_data()?;
    let keys_len = keys_len(&data)? as usize;
    let trailer_offset = HEADER_LEN + keys_len * GUARDIAN_PUBKEY_LEN;
    if data.len() < trailer_offset + TRAILER_LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let expiration_time = u32::from_le_bytes(
        data[trailer_offset + 4..trailer_offset + 8]
            .try_into()
            .map_err(|_| err(GlobalAccountantError::InvalidPda))?,
    );
    if expiration_time == 0 {
        return Ok(false);
    }
    let timestamp = Clock::get()?.unix_timestamp;
    let timestamp_u32 = if timestamp < 0 {
        0
    } else if (timestamp as u64) > (u32::MAX as u64) {
        u32::MAX
    } else {
        timestamp as u32
    };
    Ok(timestamp_u32 > expiration_time)
}
