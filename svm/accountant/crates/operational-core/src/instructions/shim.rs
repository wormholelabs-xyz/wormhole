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
/// (`VerifyHash`). The shim — not this program — authenticates both accounts, so
/// we deliberately pass them through without our own owner/address assertion:
///   - `guardian_signatures`: the shim rejects it unless its `owner` is the shim
///     program id (it only trusts the posted-signature accounts it itself wrote).
///   - `guardian_set`: the shim has no discriminator to check, so it instead
///     asserts the account address equals the Core Bridge `GuardianSet` PDA
///     derived from `[GUARDIAN_SET_SEED, guardian_index, guardian_set_bump]`
///     under the Core Bridge program id. A Core-Bridge PDA is off-curve and only
///     Core-Bridge-assignable, so address equality implies genuine ownership.
///
/// The shim then enforces guardian-set expiry, 13/19 quorum, and per-signature
/// secp256k1 recovery against the stored guardian keys.
///
/// Contrast `submit_observations`: it verifies signatures itself and so must
/// validate the guardian-set account directly — see `quorum::verify_signature`.
///
/// SECURITY: the CPI target is the hardcoded `VERIFY_VAA_SHIM_PROGRAM_ID`, never
/// a caller-supplied account, so a forged shim account cannot redirect the CPI.
pub fn verify_vaa(
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

    // `VerifyHash` is read-only — neither account is signer or writable.
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
