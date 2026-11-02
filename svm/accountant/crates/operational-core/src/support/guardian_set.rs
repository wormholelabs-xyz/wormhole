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

/// Header plus `creation_time` and `expiration_time`, with zero keys.
const MINIMUM_LEN: usize = HEADER_LEN + 8;

/// Mainnet set 0 `creation_time`. The Core Bridge never stamped that set's expiry.
const GUARDIAN_SET_ZERO_CREATION_TIME: u32 = 1_628_099_186;

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
    if read_index(guardian_set)? != index {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(())
}

/// `guardian_set_index` from the account header.
pub fn read_index(guardian_set: &AccountInfo) -> crate::ProgramCoreResult<u32> {
    let data = guardian_set.try_borrow_data()?;
    if data.len() < HEADER_LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
}

/// `keys_len` from the header. Caller must have passed [`verify_account`].
pub fn keys_len(data: &[u8]) -> crate::ProgramCoreResult<u32> {
    if data.len() < HEADER_LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(u32::from_le_bytes([data[4], data[5], data[6], data[7]]))
}

/// `(creation_time, expiration_time)` after the keys. `None` when `data` is shorter than
/// `keys_len` implies.
fn read_times(data: &[u8]) -> Option<(u32, u32)> {
    let header = data.get(..HEADER_LEN)?;
    let keys_len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    let keys_end = (keys_len as usize)
        .checked_mul(GUARDIAN_PUBKEY_LEN)?
        .checked_add(HEADER_LEN)?;
    let times = data.get(keys_end..keys_end.checked_add(MINIMUM_LEN - HEADER_LEN)?)?;
    let creation_time = u32::from_le_bytes([times[0], times[1], times[2], times[3]]);
    let expiration_time = u32::from_le_bytes([times[4], times[5], times[6], times[7]]);
    Some((creation_time, expiration_time))
}

/// The Verify VAA Shim's `GuardianSet::is_active`: mainnet set 0 is never active; any other
/// set is active while `expiration_time == 0 || timestamp <= expiration_time`.
fn is_active(
    guardian_set_index: u32,
    creation_time: u32,
    expiration_time: u32,
    timestamp: u32,
) -> bool {
    if guardian_set_index == 0 && creation_time == GUARDIAN_SET_ZERO_CREATION_TIME {
        return false;
    }
    expiration_time == 0 || timestamp <= expiration_time
}

/// `Ok(true)` when the set is inactive at the clock: `expiration_time` nonzero and past,
/// or mainnet set 0 (index 0, `creation_time` 1628099186), which the Core Bridge never
/// stamped and blocks by hand (`solana/bridge/program/src/api/post_vaa.rs`). The rule
/// matches the Verify VAA Shim's `GuardianSet::is_active`. Caller must have passed
/// [`verify_account`].
pub fn is_expired(guardian_set: &AccountInfo) -> crate::ProgramCoreResult<bool> {
    let data = guardian_set.try_borrow_data()?;
    let index = read_index(guardian_set)?;
    let (creation_time, expiration_time) =
        read_times(&data).ok_or_else(|| err(GlobalAccountantError::InvalidPda))?;
    let timestamp = Clock::get()?.unix_timestamp;
    let timestamp_u32 = if timestamp < 0 {
        0
    } else if (timestamp as u64) > (u32::MAX as u64) {
        u32::MAX
    } else {
        timestamp as u32
    };
    Ok(!is_active(
        index,
        creation_time,
        expiration_time,
        timestamp_u32,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wormhole_svm_definitions::zero_copy::GuardianSet;

    fn account_data(index: u32, keys: u32, creation_time: u32, expiration_time: u32) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&index.to_le_bytes());
        data.extend_from_slice(&keys.to_le_bytes());
        data.resize(data.len() + keys as usize * GUARDIAN_PUBKEY_LEN, 0xAB);
        data.extend_from_slice(&creation_time.to_le_bytes());
        data.extend_from_slice(&expiration_time.to_le_bytes());
        data
    }

    /// The inlined rule and parser agree with the shim's `GuardianSet`.
    #[test]
    fn activity_rule_matches_verify_vaa_shim() {
        const T: u32 = 1_800_000_000;
        // (label, index, keys, creation_time, expiration_time, timestamp)
        let cases: [(&str, u32, u32, u32, u32, u32); 10] = [
            ("current set", 4, 19, 1_713_281_400, 0, T),
            ("current set at u32::MAX", 4, 19, 1_713_281_400, 0, u32::MAX),
            ("before expiry", 3, 19, 1, T + 1, T),
            ("at expiry", 3, 19, 1, T, T),
            ("after expiry", 3, 19, 1, T - 1, T),
            ("mainnet set 0", 0, 1, GUARDIAN_SET_ZERO_CREATION_TIME, 0, T),
            ("set 0, other creation time", 0, 1, 1, 0, T),
            (
                "set 1, set 0 creation time",
                1,
                1,
                GUARDIAN_SET_ZERO_CREATION_TIME,
                0,
                T,
            ),
            ("zero keys", 7, 0, 1, 0, 0),
            ("129 keys", 7, 129, 1, T, T),
        ];
        for (label, index, keys, creation, expiration, timestamp) in cases {
            let data = account_data(index, keys, creation, expiration);
            let shim = GuardianSet::new(&data).unwrap_or_else(|| panic!("{label}: shim parse"));
            assert_eq!(
                read_times(&data),
                Some((creation, expiration)),
                "{label}: times"
            );
            assert_eq!(
                is_active(index, creation, expiration, timestamp),
                shim.is_active(timestamp),
                "{label}: rule"
            );
        }

        let full = account_data(4, 19, 1, 0);
        let mut huge_keys_len = account_data(4, 0, 1, 0);
        huge_keys_len[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        let short: [(&str, &[u8]); 4] = [
            ("empty", &[]),
            ("header only", &full[..HEADER_LEN]),
            ("one byte short", &full[..full.len() - 1]),
            ("keys_len past the data", &huge_keys_len),
        ];
        for (label, data) in short {
            assert!(GuardianSet::new(data).is_none(), "{label}: shim rejects");
            assert_eq!(read_times(data), None, "{label}: rejected here");
        }
    }
}
