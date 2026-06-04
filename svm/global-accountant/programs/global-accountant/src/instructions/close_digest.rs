//! `close_digest` — permissionless close gated by a VAA whose digest matches the
//! one stored in the PDA. Refunds rent to the recorded payer, verifying the
//! digest via a CPI to the Wormhole Verify VAA Shim.

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

use crate::definitions::GlobalAccountantError;
use crate::err;
use crate::instructions::shim;
use crate::state::digest;

/// Wire format for the `close_digest` instruction data (after the 1-byte
/// dispatch discriminator):
///
/// | offset | size | field              |
/// |--------|------|--------------------|
/// | 0      | 32   | VAA digest         |
/// | 32     | 1    | guardian_set_bump  |
///
/// `guardian_set_bump` lets the Shim derive the Core Bridge `GuardianSet` PDA.
const CLOSE_DIGEST_DATA_LEN: usize = 32 + 1;

pub fn process(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    // Accounts:
    //   0. `[SIGNER]` closer (permissionless)
    //   1. `[WRITE]`  digest PDA (this program's account)
    //   2. `[WRITE]`  rent recipient (must match recorded payer)
    //   3. `[]`       `GuardianSignatures` PDA (from a prior `PostSignatures` tx)
    //   4. `[]`       Core Bridge `GuardianSet` PDA
    //   5. `[]`       Verify VAA Shim program (CPI target)
    let [closer, digest_pda, rent_recipient, guardian_signatures, guardian_set, _verify_vaa_shim_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !closer.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    // Ownership check before any read/lamport movement: a spoofed system-owned
    // account at this address could otherwise name itself payer and sweep rent.
    if digest_pda.owner() != program_id {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    let (candidate_digest, guardian_set_bump) = parse_instruction_data(data)?;

    let layout = digest::load(digest_pda)?;
    let stored_digest = layout.digest;
    let stored_payer = layout.payer;

    if &stored_digest != candidate_digest {
        return Err(err(GlobalAccountantError::DigestMismatch));
    }
    if rent_recipient.address().as_array() != &stored_payer {
        return Err(err(GlobalAccountantError::PayerMismatch));
    }

    // `layout.guardian_set_index` is metadata only and intentionally not pinned:
    // close accepts a quorum from any currently-active set (the Shim rejects
    // retired ones), avoiding stuck accounts after rotation.
    shim::verify_vaa(
        guardian_set,
        guardian_signatures,
        candidate_digest,
        guardian_set_bump,
    )?;

    // Move lamports into the rent recipient; `close` zeroes the PDA header below.
    let lamports = digest_pda.lamports();
    let recipient_lamports = rent_recipient.lamports();
    rent_recipient.set_lamports(
        recipient_lamports
            .checked_add(lamports)
            .ok_or(ProgramError::ArithmeticOverflow)?,
    );

    digest_pda.close()
}

/// Parse `(digest, guardian_set_bump)` out of the instruction data.
fn parse_instruction_data(data: &[u8]) -> Result<(&[u8; 32], u8), ProgramError> {
    let data: &[u8; CLOSE_DIGEST_DATA_LEN] = data
        .try_into()
        .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
    let digest_arr: &[u8; 32] = data[..32]
        .try_into()
        .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
    let guardian_set_bump = data[32];
    Ok((digest_arr, guardian_set_bump))
}
