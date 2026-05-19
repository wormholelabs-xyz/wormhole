//! `close_digest` — permissionless close gated by a VAA whose digest equals
//! the one stored in the PDA. Refunds rent to the recorded payer.
//!
//! The VAA verification path is gated behind the `mock-vaa` Cargo feature.
//! Production builds without `mock-vaa` refuse to compile — they will only
//! compile once the real Verify VAA Shim CPI is wired in.

// Hard compile-time fence at the file root. The mock-vaa path is the only
// `extract_candidate_digest` implementation that exists today; without it the
// crate is intentionally uncompilable so a production `cargo build-sbf`
// without the feature flag fails loudly rather than shipping the mock.
#[cfg(not(feature = "mock-vaa"))]
compile_error!(
    "close_digest mock path requires the `mock-vaa` feature; production builds \
     must wire in Verify VAA Shim CPI before removing this gate"
);

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

use crate::definitions::GlobalAccountantError;
use crate::err;
use crate::state::digest;

pub fn process(
    program_id: &Address,
    accounts: &mut [AccountView],
    data: &[u8],
) -> ProgramResult {
    // Accounts:
    //   0. `[SIGNER]` closer (permissionless — any signer is fine)
    //   1. `[WRITE]`  digest PDA
    //   2. `[WRITE]`  rent-recipient (must match recorded payer)
    let [closer, digest_pda, rent_recipient] = accounts else {
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

    let candidate_digest = extract_candidate_digest(data)?;

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

/// Pull the candidate digest out of the instruction data. Behind the
/// `mock-vaa` feature this is the first 32 bytes verbatim; in production this
/// branch must invoke the Verify VAA Shim CPI and assert recovered signatures
/// against the recorded guardian set before returning the embedded digest.
///
/// Only the mock implementation exists today. A non-mock build is fenced off
/// by the top-of-file `compile_error!`, so the bare `#[cfg]` here is enough —
/// no `cfg(not(...))` stub is needed.
#[cfg(feature = "mock-vaa")]
fn extract_candidate_digest(data: &[u8]) -> Result<&[u8; 32], ProgramError> {
    if data.len() < 32 {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    Ok(data[..32].try_into().unwrap())
}
