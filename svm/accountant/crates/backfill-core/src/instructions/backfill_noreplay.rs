//! `BackfillNoReplay` — flip `solana-noreplay` bits for a batch of
//! `(chain, emitter, sequence, digest)` entries and emit one canonical
//! `ACCDGST\0` commit-log per entry.
//!
//! Bulk-CPI design: consecutive entries sharing a `NoReplayBitmapAccount` bucket OR
//! their bits into one 128-byte mask; one `MarkUsedBulk` CPI per unique bucket.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use crate::cpi::noreplay::mark_used_bulk;
use crate::definitions::{
    GlobalAccountantError, NoReplayBatch, NoReplayBitmapAccount, NOREPLAY_BITMAP_BYTES,
    UNPINNED_GUARDIAN_SET_INDEX,
};
use crate::support::authority::require_authority;
use crate::support::commit_log;
use crate::{err, ProgramResult};

pub fn process(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
    expected_authority: &[u8; 32],
) -> ProgramResult {
    let batch = NoReplayBatch::parse(data).map_err(err)?;

    // Accounts: [WRITE, SIGNER] payer, [] solana-noreplay program (CPI target),
    // [] noreplay-authority PDA (signs MarkUsedBulk via invoke_signed), [] system program,
    // then one bucket PDA per unique (chain, emitter, bucket_index) in walk order.
    let [payer, noreplay_program, noreplay_authority, system_program, buckets @ ..] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    require_authority(payer, expected_authority)?;

    let mut prev_bucket: Option<(u16, [u8; 32], u64)> = None;
    let mut or_mask = [0u8; NOREPLAY_BITMAP_BYTES];
    let mut bucket_account_idx: usize = 0;

    for group in batch.groups() {
        let chain = group.header.chain();
        let emitter = group.header.emitter;

        for entry in group.entries {
            let sequence = entry.sequence();
            let cur_bucket = (
                chain,
                emitter,
                NoReplayBitmapAccount::bucket_index(sequence),
            );
            if let Some(prev_b) = prev_bucket {
                if cur_bucket != prev_b {
                    if bucket_account_idx >= buckets.len() {
                        return Err(err(GlobalAccountantError::InvalidInstructionData));
                    }
                    mark_used_bulk(
                        payer,
                        &buckets[bucket_account_idx],
                        noreplay_program,
                        noreplay_authority,
                        system_program,
                        program_id,
                        prev_b.0,
                        &prev_b.1,
                        prev_b.2,
                        &or_mask,
                    )?;
                    bucket_account_idx += 1;
                    or_mask = [0u8; NOREPLAY_BITMAP_BYTES];
                }
            }

            let bit = NoReplayBitmapAccount::bit_index(sequence);
            or_mask[bit / 8] |= 1u8 << (bit % 8);

            commit_log::emit(
                chain,
                &emitter,
                sequence,
                &entry.digest,
                UNPINNED_GUARDIAN_SET_INDEX,
            );

            prev_bucket = Some(cur_bucket);
        }
    }

    // Final flush for the last bucket.
    if let Some(prev_b) = prev_bucket {
        if bucket_account_idx >= buckets.len() {
            return Err(err(GlobalAccountantError::InvalidInstructionData));
        }
        mark_used_bulk(
            payer,
            &buckets[bucket_account_idx],
            noreplay_program,
            noreplay_authority,
            system_program,
            program_id,
            prev_b.0,
            &prev_b.1,
            prev_b.2,
            &or_mask,
        )?;
        bucket_account_idx += 1;
    }

    // A trailing unused bucket account means the caller passed dead state (rent-griefing
    // risk); a shortfall is already caught in-loop.
    if bucket_account_idx != buckets.len() {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }

    Ok(())
}
