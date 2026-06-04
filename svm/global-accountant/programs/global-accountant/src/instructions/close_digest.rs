//! `close_digest` — permissionless close gated by a VAA whose digest equals
//! the one stored in the PDA. Refunds rent to the recorded payer.
//!
//! CPIs into the Wormhole Verify VAA Shim
//! (`EFaNWErqAtVWufdNb7yofSHHfWFos843DFpu4JBw24at`). The Shim's `VerifyHash`
//! instruction reads a `GuardianSignatures` PDA (posted in a prior
//! `PostSignatures` tx) and the Core Bridge's `GuardianSet` PDA, then verifies
//! guardian-quorum signatures against the supplied 32-byte digest. The Shim
//! writes no state; on success it simply returns `Ok(())`.

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
/// `guardian_set_bump` lets the Shim's `VerifyHash` derive the Core Bridge's
/// `GuardianSet` PDA without paying the `find_program_address` cost on-chain.
const CLOSE_DIGEST_DATA_LEN: usize = 32 + 1;

pub fn process(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    // Accounts:
    //   0. `[SIGNER]` closer (permissionless — any signer is fine)
    //   1. `[WRITE]`  digest PDA (this program's account)
    //   2. `[WRITE]`  rent-recipient (must match recorded payer)
    //   3. `[]`       Verify VAA Shim — guardian signatures PDA
    //                 (created by a prior `PostSignatures` tx)
    //   4. `[]`       Wormhole Core Bridge — `GuardianSet` PDA
    //   5. `[]`       Verify VAA Shim program (CPI target)
    //
    // The Shim accepts the guardian-set PDA at instruction-account index 0 and
    // the guardian-signatures PDA at index 1 — see
    // `svm/wormhole-core-shims/programs/verify-vaa/src/lib.rs::process_verify_hash`.
    // Slot 5 (Shim program) must be present so the runtime can resolve the
    // CPI callee, but the program never reads it — `shim::verify_vaa` targets
    // the hardcoded program ID.
    let [closer, digest_pda, rent_recipient, guardian_signatures, guardian_set, _verify_vaa_shim_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !closer.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    // Ownership check before any data read or lamport movement. Without this,
    // an attacker can pre-create a system-owned account at the canonical PDA
    // address with hand-crafted layout bytes naming themselves as payer and
    // sweep the lamports through the runtime's debit path.
    if digest_pda.owner() != program_id {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    let (candidate_digest, guardian_set_bump) = parse_instruction_data(data)?;

    // Load the layout by value so we can mutate the account below.
    let layout = digest::load(digest_pda)?;
    let stored_digest = layout.digest;
    let stored_payer = layout.payer;

    if &stored_digest != candidate_digest {
        return Err(err(GlobalAccountantError::DigestMismatch));
    }
    if rent_recipient.address().as_array() != &stored_payer {
        return Err(err(GlobalAccountantError::PayerMismatch));
    }

    // Design note: `layout.guardian_set_index` is metadata only and intentionally
    // NOT compared against `guardian_signatures.guardian_set_index`. Close accepts
    // a quorum from any currently-active set; the Shim's `is_active(timestamp)`
    // check rejects retired sets, and pinning close to the original set would
    // create permanent stuck accounts after rotation without improving safety.
    shim::verify_vaa(
        guardian_set,
        guardian_signatures,
        candidate_digest,
        guardian_set_bump,
    )?;

    // Move lamports out of the PDA into the rent recipient. `AccountView::close`
    // zeroes the 48-byte header (owner + lamports + data_len) below, so the
    // PDA's lamports do not need to be cleared separately here.
    let lamports = digest_pda.lamports();
    let recipient_lamports = rent_recipient.lamports();
    rent_recipient.set_lamports(
        recipient_lamports
            .checked_add(lamports)
            .ok_or(ProgramError::ArithmeticOverflow)?,
    );

    // Pinocchio's `close` zeroes owner, lamports, and data_length so the
    // runtime releases the account at the end of the instruction.
    digest_pda.close()
}

/// Parse `(digest, guardian_set_bump)` out of the instruction data.
fn parse_instruction_data(data: &[u8]) -> Result<(&[u8; 32], u8), ProgramError> {
    let data: &[u8; CLOSE_DIGEST_DATA_LEN] = data
        .try_into()
        .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
    // `split_array_ref` would be tidier but is unstable; manually re-borrow
    // the digest range to a fixed-size reference. The `?` keeps a panicking
    // `.unwrap()` off the hot path even though the length check above already
    // guarantees the slice is exactly 33 bytes.
    let digest_arr: &[u8; 32] = data[..32]
        .try_into()
        .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
    let guardian_set_bump = data[32];
    Ok((digest_arr, guardian_set_bump))
}
