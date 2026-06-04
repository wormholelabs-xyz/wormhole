//! Shared Verify VAA Shim CPI helper.
//!
//! Single authority for the `VerifyHash` CPI shape — `close_digest`,
//! `register_chain`, `modify_balance`, and `submit_vaas` all route through
//! [`verify_vaa`] so the Shim wire format lives in exactly one place.

use pinocchio::{
    instruction::{InstructionAccount, InstructionView},
    AccountView, Address, ProgramResult,
};

use crate::definitions::{VERIFY_HASH_DATA_LEN, VERIFY_HASH_SELECTOR, VERIFY_VAA_SHIM_PROGRAM_ID};

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
///
/// The CPI target is built from the hardcoded `VERIFY_VAA_SHIM_PROGRAM_ID`
/// constant, never from a caller-supplied account, so a forged shim-program
/// account cannot redirect the CPI — if the real Shim is absent from the
/// instruction context the `invoke` fails at the runtime. The Shim program
/// account must still appear in the caller's account list for the runtime to
/// resolve the callee, but it is never read or trusted, and no program-ID
/// pre-check is needed here.
pub(crate) fn verify_vaa(
    guardian_set: &AccountView,
    guardian_signatures: &AccountView,
    digest: &[u8; 32],
    guardian_set_bump: u8,
) -> ProgramResult {
    let shim_program_id = Address::from(VERIFY_VAA_SHIM_PROGRAM_ID);

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
        program_id: &shim_program_id,
        data: &ix_data,
        accounts: &ix_accounts,
    };

    pinocchio::cpi::invoke(&instruction, &[guardian_set, guardian_signatures])
}
