//! Shared Verify VAA Shim CPI helper.
//!
//! Single authority for the `VerifyHash` CPI shape — `close_digest`,
//! `register_chain`, `modify_balance`, and `submit_vaas` all route through
//! [`verify_vaa`] so the Shim wire format lives in exactly one place.
//!
//! Ported mechanically from the pinocchio `InstructionView`/`invoke` shape to
//! `anchor_lang::solana_program::instruction::{Instruction, AccountMeta}` +
//! `invoke` — the wire bytes and account list are unchanged (migration plan
//! §2d): this CPI targets a foreign, non-Anchor program identified by a
//! hardcoded address, so there is no generated client to route through
//! either way.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke;

use crate::definitions::{VERIFY_HASH_DATA_LEN, VERIFY_HASH_SELECTOR, VERIFY_VAA_SHIM_PROGRAM_ID};
use crate::ProgramResult;

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
pub fn verify_vaa<'info>(
    guardian_set: &AccountInfo<'info>,
    guardian_signatures: &AccountInfo<'info>,
    digest: &[u8; 32],
    guardian_set_bump: u8,
) -> ProgramResult {
    let shim_program_id = Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID);

    // Build the Shim's `VerifyHash` instruction data on the stack:
    //   [0..8]  = VERIFY_HASH_SELECTOR (Anchor discriminator)
    //   [8]     = guardian_set_bump
    //   [9..41] = digest
    let mut ix_data = [0u8; VERIFY_HASH_DATA_LEN];
    ix_data[..8].copy_from_slice(&VERIFY_HASH_SELECTOR);
    ix_data[8] = guardian_set_bump;
    ix_data[9..].copy_from_slice(digest);

    // `VerifyHash` is read-only — neither account is signer or writable.
    let ix_accounts = vec![
        AccountMeta::new_readonly(*guardian_set.key, false),
        AccountMeta::new_readonly(*guardian_signatures.key, false),
    ];

    let instruction = Instruction {
        program_id: shim_program_id,
        accounts: ix_accounts,
        data: ix_data.to_vec(),
    };

    invoke(&instruction, &[guardian_set.clone(), guardian_signatures.clone()])
}
