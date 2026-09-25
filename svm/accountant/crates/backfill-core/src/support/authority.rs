//! Signer check against an expected pubkey.
//!
//! SECURITY: this is the only gate on every backfill write. Each program shell pins its
//! `expected` at compile time from its own `env!` variable, so the key an artifact enforces
//! is fixed when the `.so` is built and a missing variable is a build error. Callers run this
//! before parsing instruction data. `just verify-authority` replays the gate of a built `.so`
//! against an operator key.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::{err, ProgramResult};

use crate::definitions::GlobalAccountantError;

/// Reject unless `payer` signed the tx and its pubkey equals `expected`.
#[inline]
pub fn require_authority(payer: &AccountInfo, expected: &[u8; 32]) -> ProgramResult {
    if !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if payer.key.to_bytes() != *expected {
        return Err(err(GlobalAccountantError::UnauthorizedCaller));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED: [u8; 32] = [0x11u8; 32];

    #[test]
    fn require_authority_table() {
        let owner = Pubkey::default();
        // (label, key, is_signer, expected result)
        let cases: [(&str, [u8; 32], bool, ProgramResult); 3] = [
            ("expected key, signing", EXPECTED, true, Ok(())),
            (
                "unrelated key, signing",
                [0x22u8; 32],
                true,
                Err(err(GlobalAccountantError::UnauthorizedCaller)),
            ),
            (
                "expected key, not signing",
                EXPECTED,
                false,
                Err(ProgramError::MissingRequiredSignature),
            ),
        ];

        for (label, key, is_signer, expected) in cases {
            let key = Pubkey::new_from_array(key);
            let mut lamports = 0u64;
            let mut data: [u8; 0] = [];
            let payer = AccountInfo::new(
                &key,
                is_signer,
                true,
                &mut lamports,
                &mut data,
                &owner,
                false,
            );
            assert_eq!(require_authority(&payer, &EXPECTED), expected, "{label}");
        }
    }
}
