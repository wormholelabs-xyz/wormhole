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
use wormhole_svm_definitions::zero_copy::GuardianSet;

use crate::definitions::{GlobalAccountantError, CORE_BRIDGE_PROGRAM_ID, GUARDIAN_SET_SEED};
use crate::err;

/// Guardian key: `keccak256(uncompressed_pk)[12..]`.
pub const GUARDIAN_PUBKEY_LEN: usize = 20;

const HEADER_LEN: usize = 8;

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

/// `Ok(true)` when the set is inactive at the clock: `expiration_time` nonzero and past,
/// or mainnet set 0 (index 0, `creation_time` 1628099186), which the Core Bridge never
/// stamped and blocks by hand (`solana/bridge/program/src/api/post_vaa.rs`). The rule is
/// [`GuardianSet::is_active`], shared with the Verify VAA Shim. Caller must have passed
/// [`verify_account`].
pub fn is_expired(guardian_set: &AccountInfo) -> crate::ProgramCoreResult<bool> {
    let data = guardian_set.try_borrow_data()?;
    let set = GuardianSet::new(&data).ok_or_else(|| err(GlobalAccountantError::InvalidPda))?;
    let timestamp = Clock::get()?.unix_timestamp;
    let timestamp_u32 = if timestamp < 0 {
        0
    } else if (timestamp as u64) > (u32::MAX as u64) {
        u32::MAX
    } else {
        timestamp as u32
    };
    Ok(!set.is_active(timestamp_u32))
}
