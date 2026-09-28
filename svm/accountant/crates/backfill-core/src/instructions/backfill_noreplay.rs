//! `BackfillNoReplay` — flip `solana-noreplay` bits for a batch of
//! `(chain, emitter, sequence, digest)` entries and emit one canonical
//! `ACCDGST\0` commit-log per entry.
//!
//! Bulk-CPI design: consecutive entries sharing a `NoReplayBitmapAccount` bucket OR
//! their bits into one 128-byte mask; one `MarkUsedBulk` CPI per unique bucket.
//!
//! SECURITY: `require_authority` is the sole authentication for every bit flipped here. It
//! runs before the parser, so the parser sees operator-supplied data only. `mark_used_bulk`
//! re-derives the authority PDA and the bucket PDA from the entry's own namespace, so a
//! substituted bucket account fails with `InvalidPda` ahead of the CPI. The bucket slot count
//! must equal the number of bucket transitions the walk makes, which pins each mask to the
//! account the caller passed for it.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::support::commit_log;
use accountant_operational_core::{err, ProgramResult};

use crate::cpi::noreplay::mark_used_bulk;
use crate::definitions::{
    GlobalAccountantError, NoReplayBatch, NoReplayBitmapAccount, NoReplayNamespace,
    NOREPLAY_BITMAP_BYTES, UNPINNED_GUARDIAN_SET_INDEX,
};
use crate::support::authority::require_authority;

/// One bucket's identity. A change of either field ends the current mask and forces a
/// flush, so the two travel as one value.
#[derive(Clone, Copy, PartialEq, Eq)]
struct BucketKey {
    namespace: NoReplayNamespace,
    bucket_index: u64,
}

/// Order: account framing, authority, wire parse, then the bucket walk, one `MarkUsedBulk`
/// CPI per bucket transition and a final flush.
pub fn process(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
    expected_authority: &[u8; 32],
) -> ProgramResult {
    // Accounts:
    //   0.  `[WRITE, SIGNER]` payer
    //   1.  `[]`              solana-noreplay program (CPI target)
    //   2.  `[]`              noreplay-authority PDA (signs `MarkUsedBulk` via `invoke_signed`)
    //   3.  `[]`              system program
    //   4.. `[WRITE]`         bucket PDA, one per unique `(chain, emitter, bucket_index)`,
    //                         in walk order
    let [payer, _noreplay_program, noreplay_authority, system_program, buckets @ ..] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    require_authority(payer, expected_authority)?;

    let batch = NoReplayBatch::parse(data).map_err(err)?;
    msg!(
        "BackfillNoReplay: {} entries",
        batch
            .groups()
            .map(|group| group.entries.len())
            .sum::<usize>()
    );

    let mut previous_bucket: Option<BucketKey> = None;
    let mut or_mask = [0u8; NOREPLAY_BITMAP_BYTES];
    let mut bucket_account_index: usize = 0;

    for group in batch.groups() {
        let chain = group.header.chain();
        let emitter = group.header.emitter;

        for entry in group.entries {
            let sequence = entry.sequence();
            let current_bucket = BucketKey {
                namespace: NoReplayNamespace::new(chain, emitter),
                bucket_index: NoReplayBitmapAccount::bucket_index(sequence),
            };
            if let Some(previous) = previous_bucket {
                if current_bucket != previous {
                    if bucket_account_index >= buckets.len() {
                        return Err(err(GlobalAccountantError::InvalidInstructionData));
                    }
                    mark_used_bulk(
                        payer,
                        &buckets[bucket_account_index],
                        noreplay_authority,
                        system_program,
                        program_id,
                        &previous.namespace,
                        previous.bucket_index,
                        &or_mask,
                    )?;
                    bucket_account_index += 1;
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

            previous_bucket = Some(current_bucket);
        }
    }

    // Final flush for the last bucket.
    if let Some(previous) = previous_bucket {
        if bucket_account_index >= buckets.len() {
            return Err(err(GlobalAccountantError::InvalidInstructionData));
        }
        mark_used_bulk(
            payer,
            &buckets[bucket_account_index],
            noreplay_authority,
            system_program,
            program_id,
            &previous.namespace,
            previous.bucket_index,
            &or_mask,
        )?;
        bucket_account_index += 1;
    }

    // A trailing unused bucket account means the caller passed dead state (rent-griefing
    // risk); a shortfall is already caught in-loop.
    if bucket_account_index != buckets.len() {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }

    Ok(())
}
