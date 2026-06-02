//! `close_digest` — permissionless close gated by a VAA whose digest equals
//! the one stored in the PDA. Refunds rent to the recorded payer.
//!
//! CPIs into the Wormhole Verify VAA Shim
//! (`EFaNWErqAtVWufdNb7yofSHHfWFos843DFpu4JBw24at`). The Shim's `VerifyHash`
//! instruction reads a `GuardianSignatures` PDA (posted in a prior
//! `PostSignatures` tx) and the Core Bridge's `GuardianSet` PDA, then verifies
//! guardian-quorum signatures against the supplied 32-byte digest. The Shim
//! writes no state; on success it simply returns `Ok(())`.

use pinocchio::{
    error::ProgramError,
    instruction::{InstructionAccount, InstructionView},
    AccountView, Address, ProgramResult,
};

use crate::definitions::{GlobalAccountantError, VERIFY_HASH_DATA_LEN, VERIFY_HASH_SELECTOR};
use crate::err;
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
    let [closer, digest_pda, rent_recipient, guardian_signatures, guardian_set, verify_vaa_shim_program] =
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
    verify_vaa(
        verify_vaa_shim_program,
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

/// Verify the candidate digest via CPI to the Wormhole Verify VAA Shim
/// (`VerifyHash`).
///
/// The Shim's checks (see `programs/verify-vaa/src/lib.rs::process_verify_hash`):
///   1. `guardian_signatures` is owned by the Shim program.
///   2. `guardian_set`'s address matches `(GUARDIAN_SET_SEED,
///      guardian_index_be, guardian_set_bump)` under the Core Bridge program.
///   3. The guardian set is not expired.
///   4. The recovered Ethereum pubkeys reach quorum against the stored digest.
///
/// All four checks live inside the Shim — we just pass the accounts through.
fn verify_vaa(
    verify_vaa_shim_program: &AccountView,
    guardian_set: &AccountView,
    guardian_signatures: &AccountView,
    digest: &[u8; 32],
    guardian_set_bump: u8,
) -> ProgramResult {
    // Defence-in-depth: refuse to CPI to anything other than the Shim. The
    // runtime would still reject a wrong program ID (the Shim's entrypoint
    // rejects any `program_id != ID` up front), but failing here yields our
    // own `InvalidPda` error code instead of a generic `IncorrectProgramId`.
    if verify_vaa_shim_program.address().as_array()
        != &crate::definitions::VERIFY_VAA_SHIM_PROGRAM_ID
    {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    // Build the Shim's `VerifyHash` instruction data on the stack:
    //   [0..8]  = VERIFY_HASH_SELECTOR (Anchor discriminator)
    //   [8]     = guardian_set_bump
    //   [9..41] = digest
    let mut ix_data = [0u8; VERIFY_HASH_DATA_LEN];
    ix_data[..8].copy_from_slice(&VERIFY_HASH_SELECTOR);
    ix_data[8] = guardian_set_bump;
    ix_data[9..].copy_from_slice(digest);

    // The Shim's `VerifyHash` is read-only — neither account is `is_signer`
    // and neither is `is_writable`. See the AccountMeta block in
    // `crates/shim/src/verify_vaa/verify_hash.rs::VerifyHash::instruction`.
    let ix_accounts = [
        InstructionAccount::readonly(guardian_set.address()),
        InstructionAccount::readonly(guardian_signatures.address()),
    ];

    let instruction = InstructionView {
        program_id: verify_vaa_shim_program.address(),
        data: &ix_data,
        accounts: &ix_accounts,
    };

    // Checked invoke: pinocchio verifies the borrow state of each AccountView
    // matches the instruction's mutability. `guardian_set` and
    // `guardian_signatures` are read-only, so the runtime's account-data
    // borrows held by this program (currently none — `digest::load` copies by
    // value and drops the borrow before this point) are irrelevant. No signer
    // seeds: the Shim's `VerifyHash` requires no signature.
    pinocchio::cpi::invoke(&instruction, &[guardian_set, guardian_signatures])
}
