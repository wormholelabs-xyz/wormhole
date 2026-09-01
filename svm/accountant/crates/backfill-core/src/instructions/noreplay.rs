//! NoReplay `MarkUsedBulk` CPI helper.
//!
//! Per-entry `MarkUsed` cost ~3,065 CU each — mostly CPI dispatch, arg
//! deser, and `find_program_address` inside noreplay. `MarkUsedBulk` does
//! the same allocation-and-write work but accepts a 128-byte OR mask, so
//! one CPI flips up to 1024 bits. Wire format: [`NoReplayMarkUsedBulkData`].

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke_signed;

use crate::definitions::{
    NoReplayMarkUsedBulkData, NoReplayNamespace, NOREPLAY_AUTHORITY_SEED_PREFIX,
    NOREPLAY_BITMAP_BYTES, NOREPLAY_PROGRAM_ID,
};
use crate::{err, BackfillError, ProgramCoreResult, ProgramResult};

/// Check `noreplay_authority` is this program's authority PDA; returns its bump.
fn verify_authority(
    program_id: &Pubkey,
    noreplay_authority: &AccountInfo,
) -> ProgramCoreResult<u8> {
    let (expected, bump) =
        Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], program_id);
    if noreplay_authority.key != &expected {
        return Err(err(BackfillError::InvalidPda));
    }
    Ok(bump)
}

/// `MarkUsedBulk` instruction. Accounts: payer (signer, writable),
/// authority (signer), bitmap PDA (writable), system program.
///
/// SECURITY: the CPI target is pinned to the constant `NOREPLAY_PROGRAM_ID`.
fn mark_used_bulk_instruction(
    payer: &Pubkey,
    noreplay_authority: &Pubkey,
    bucket: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    bucket_index: u64,
    or_mask: [u8; NOREPLAY_BITMAP_BYTES],
) -> Instruction {
    let data =
        NoReplayMarkUsedBulkData::new(NoReplayNamespace::new(chain, *emitter), bucket_index, or_mask);
    Instruction {
        program_id: Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(*noreplay_authority, true),
            AccountMeta::new(*bucket, false),
            AccountMeta::new_readonly(anchor_lang::solana_program::system_program::ID, false),
        ],
        data: data.as_bytes().to_vec(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn mark_used_bulk<'info>(
    payer: &AccountInfo<'info>,
    bucket: &AccountInfo<'info>,
    _noreplay_program: &AccountInfo<'info>,
    noreplay_authority: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    program_id: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    bucket_index: u64,
    or_mask: &[u8; NOREPLAY_BITMAP_BYTES],
) -> ProgramResult {
    let authority_bump = verify_authority(program_id, noreplay_authority)?;
    let instruction = mark_used_bulk_instruction(
        payer.key,
        noreplay_authority.key,
        bucket.key,
        chain,
        emitter,
        bucket_index,
        *or_mask,
    );
    // `invoke_signed` returns `Err(...)` only for pre-CPI validation failures
    // (missing account, address mismatch, borrow conflicts). If the inner
    // noreplay program returns a `ProgramError`, the SBF runtime aborts this
    // program with that exit code directly, bypassing this `Result`.
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
        let ix = mark_used_bulk_instruction(&payer, &authority, &bucket, 2, &[0xAB; 32], 7, mask);
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
                (anchor_lang::solana_program::system_program::ID, false, false),
            ]
        );
    }
}
