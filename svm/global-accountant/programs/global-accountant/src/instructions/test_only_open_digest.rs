//! `test_only_open_digest` — test-only entrypoint to initialise a DigestAccount
//! PDA directly.
//!
//! Nothing in this module is reachable from production code. It is a thin
//! wire-format wrapper around the shared `super::open_digest::open_digest_inner`
//! helper, which the two production commit paths (`submit_observations` on
//! quorum reach, `submit_vaas` after Shim verification) call themselves. The
//! standalone entrypoint is kept behind `test-only-open-digest` so the
//! existing mollusk tests can drive the PDA lifecycle without standing up a
//! full 13-of-19 quorum or a verified VAA.

// Hard compile-time fence at the file root. `test_only_open_digest` is a test-build
// entrypoint; any production `cargo build-sbf` without `test-only-open-digest`
// must refuse to compile this file at all rather than rely on dead-code
// stripping. Mirrors the `mock-vaa` fence in `close_digest.rs`. The shared
// inner helper lives in `super::open_digest` so it is unaffected by this gate.
#[cfg(not(feature = "test-only-open-digest"))]
compile_error!(
    "test_only_open_digest is a test-only entrypoint; production opens go through \
     `submit_observations` or `submit_vaas`. Production builds must not \
     compile this module."
);

use pinocchio::{AccountView, Address, ProgramResult};

use crate::definitions::GlobalAccountantError;
use crate::err;
use crate::instructions::open_digest::open_digest_inner;

/// Layout of the open_digest instruction data (after the 1-byte discriminator).
/// Distinct from the account layout — see `DigestAccountLayout` in
/// `global-accountant-definitions` for that.
///
/// | offset | size | field                              |
/// |--------|------|------------------------------------|
/// | 0      | 2    | chain (big endian)                 |
/// | 2      | 32   | emitter                            |
/// | 34     | 8    | sequence (big endian)              |
/// | 42     | 32   | digest                             |
/// | 74     | 4    | guardian_set_index (little endian) |
///
/// No bump travels in the wire: `open_digest_inner` derives the canonical
/// bump on-chain via `find_program_address`.
const OPEN_DIGEST_DATA_LEN: usize = 2 + 32 + 8 + 32 + 4;

pub fn process(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let data: &[u8; OPEN_DIGEST_DATA_LEN] = data
        .try_into()
        .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;

    let (chain_bytes, rest) = data.split_at(2);
    let (emitter, rest) = rest.split_at(32);
    let (sequence_bytes, rest) = rest.split_at(8);
    let (digest_bytes, gsi_bytes) = rest.split_at(32);

    let chain_be: [u8; 2] = chain_bytes
        .try_into()
        .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
    let emitter_arr: [u8; 32] = emitter
        .try_into()
        .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
    let sequence_be: [u8; 8] = sequence_bytes
        .try_into()
        .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
    let digest_arr: [u8; 32] = digest_bytes
        .try_into()
        .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
    let gsi_bytes: [u8; 4] = gsi_bytes
        .try_into()
        .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
    let guardian_set_index = u32::from_le_bytes(gsi_bytes);

    // Accounts:
    //   0. `[WRITE, SIGNER]` payer (rent funder)
    //   1. `[WRITE]`         digest PDA (uninit)
    //   2. `[]`              system program
    let [payer, digest_pda, _system_program] = accounts else {
        return Err(pinocchio::error::ProgramError::NotEnoughAccountKeys);
    };

    if !payer.is_signer() {
        return Err(pinocchio::error::ProgramError::MissingRequiredSignature);
    }

    open_digest_inner(
        program_id,
        payer,
        digest_pda,
        chain_be,
        emitter_arr,
        sequence_be,
        digest_arr,
        guardian_set_index,
    )
}
