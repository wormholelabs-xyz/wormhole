//! Bulk-flip `MarkUsedBulk` CPI (`mark_used_bulk`).
//! Wire layout: [`NoReplayMarkUsedBulkData`]. The authority is this program's PDA at
//! `[NOREPLAY_AUTHORITY_SEED_PREFIX]`.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke_signed;
use anchor_lang::solana_program::system_program;

use accountant_operational_core::cpi::noreplay::{derive_authority, derive_bucket_pda};
use accountant_operational_core::{err, ProgramCoreResult, ProgramResult};

use crate::definitions::{
    GlobalAccountantError, NoReplayMarkUsedBulkData, NoReplayNamespace,
    NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITMAP_BYTES, NOREPLAY_BITS_PER_BUCKET,
    NOREPLAY_PROGRAM_ID,
};

/// Re-derive the authority PDA and the bucket PDA, and check both against the accounts
/// the caller passed. Returns the authority bump for `invoke_signed`.
///
/// SECURITY: the handler picks bucket accounts positionally out of `remaining_accounts`,
/// so a wrong order would OR the mask into another namespace's bitmap. Mirrors the same
/// check in `is_marked`.
fn verify_accounts(
    program_id: &Pubkey,
    noreplay_authority: &AccountInfo,
    bucket: &AccountInfo,
    namespace: &NoReplayNamespace,
    bucket_index: u64,
) -> ProgramCoreResult<u8> {
    let (expected_authority, authority_bump) = derive_authority(program_id);
    if noreplay_authority.key != &expected_authority {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    // `derive_bucket_pda` takes a sequence and divides it down; the first sequence of
    // the bucket recovers `bucket_index` exactly.
    let first_sequence = bucket_index
        .checked_mul(NOREPLAY_BITS_PER_BUCKET)
        .ok_or_else(|| err(GlobalAccountantError::InvalidInstructionData))?;
    let (expected_bucket, _) = derive_bucket_pda(
        &expected_authority,
        u16::from_be_bytes(namespace.chain),
        &namespace.emitter,
        first_sequence,
    );
    if bucket.key != &expected_bucket {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    Ok(authority_bump)
}

/// `MarkUsedBulk` instruction. Data payload carries a 128-byte OR mask
/// instead of one sequence.
fn mark_used_bulk_instruction(
    payer: &Pubkey,
    noreplay_authority: &Pubkey,
    bucket: &Pubkey,
    namespace: &NoReplayNamespace,
    bucket_index: u64,
    or_mask: [u8; NOREPLAY_BITMAP_BYTES],
) -> Instruction {
    let data = NoReplayMarkUsedBulkData::new(*namespace, bucket_index, or_mask);
    Instruction {
        program_id: Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(*noreplay_authority, true),
            AccountMeta::new(*bucket, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: data.as_bytes().to_vec(),
    }
}

/// The caller must keep the NoReplay program in the transaction's account list, so the
/// runtime loads it. The CPI target comes from `NOREPLAY_PROGRAM_ID`.
///
/// Flip up to 1024 bits in one CPI: ~80 CU/entry versus ~3,065 CU for `mark_used`,
/// mostly saved on repeated CPI dispatch, arg deser, and `find_program_address`.
#[allow(clippy::too_many_arguments)]
pub fn mark_used_bulk<'info>(
    payer: &AccountInfo<'info>,
    bucket: &AccountInfo<'info>,
    noreplay_authority: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    program_id: &Pubkey,
    namespace: &NoReplayNamespace,
    bucket_index: u64,
    or_mask: &[u8; NOREPLAY_BITMAP_BYTES],
) -> ProgramResult {
    let authority_bump = verify_accounts(
        program_id,
        noreplay_authority,
        bucket,
        namespace,
        bucket_index,
    )?;
    let instruction = mark_used_bulk_instruction(
        payer.key,
        noreplay_authority.key,
        bucket.key,
        namespace,
        bucket_index,
        *or_mask,
    );
    invoke_signed(
        &instruction,
        &[
            payer.clone(),
            noreplay_authority.clone(),
            bucket.clone(),
            system_program.clone(),
        ],
        &[&[NOREPLAY_AUTHORITY_SEED_PREFIX, &[authority_bump]]],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_used_bulk_instruction_shape() {
        let payer = Pubkey::new_unique();
        let authority = Pubkey::new_unique();
        let bucket = Pubkey::new_unique();
        let mask = [0xFFu8; NOREPLAY_BITMAP_BYTES];
        let namespace = NoReplayNamespace::new(2, [0xAB; 32]);
        let ix = mark_used_bulk_instruction(&payer, &authority, &bucket, &namespace, 7, mask);
        assert_eq!(ix.program_id, Pubkey::new_from_array(NOREPLAY_PROGRAM_ID));
        assert_eq!(ix.data.len(), NoReplayMarkUsedBulkData::LEN);
        let metas: Vec<(Pubkey, bool, bool)> = ix
            .accounts
            .iter()
            .map(|m| (m.pubkey, m.is_signer, m.is_writable))
            .collect();
        assert_eq!(
            metas,
            vec![
                (payer, true, true),
                (authority, true, false),
                (bucket, false, true),
                (system_program::ID, false, false),
            ]
        );
    }
}
