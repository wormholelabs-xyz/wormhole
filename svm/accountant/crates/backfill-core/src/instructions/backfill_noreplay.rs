//! `BackfillNoReplay` — flip `solana-noreplay` bits for a batch of
//! `(chain, emitter, sequence, digest)` entries and emit one canonical
//! `ACCDGST\0` commit-log per entry.
//!
//! ## Wire format — emitter-grouped (compact)
//!
//! Real backfill traffic clusters: many entries from the same
//! `(chain, emitter)` fall into a small set of buckets (1024 sequences each).
//! The 1232-byte tx wire limit binds on encoding size, not CU, so entries are
//! grouped by `(chain, emitter)`:
//!
//! | offset                       | size | field                          |
//! |------------------------------|------|--------------------------------|
//! | 0                            | 1    | `group_count` (u8)             |
//! | (per group)                  |      |                                |
//! |   +0                         | 2    | chain (u16 BE)                 |
//! |   +2                         | 32   | emitter                        |
//! |   +34                        | 1    | `entry_count` (u8)             |
//! |   +35 + i × 40 + 0           | 8    | sequence (u64 BE)              |
//! |   +35 + i × 40 + 8           | 32   | digest                         |
//!
//! Per-entry footprint is 40 bytes (74 in the old per-entry format), since
//! `(chain, emitter)` is stated once per group. Roughly doubles the entries
//! that fit in one tx.
//!
//! Strict ordering rules (enforced inline; reject `InvalidInstructionData`):
//!   - Groups must appear in strictly ascending `(chain, emitter)`.
//!   - Within a group, entries must appear in strictly ascending `sequence`.
//!   - Groups must be non-empty (`entry_count >= 1`).
//!   - Top-level `group_count` must be >= 1.
//!
//! Bulk-CPI design: consecutive entries sharing a `NoReplayBitmapAccount` bucket OR their
//! bits into one 128-byte mask; one `MarkUsedBulk` CPI per unique bucket, dropping per-entry
//! NoReplay cost from ~3,000 CU to ~80 CU.
//!
//! Auditability rests on the emitted `ACCDGST\0` commit-log: any third party with the Solana
//! ledger archive and a VAA archive can replay entries against the VAAs to verify them lazily.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use crate::definitions::{NoReplayBitmapAccount, NOREPLAY_BITMAP_BYTES};
use crate::instructions::{authority, commit_log, noreplay};
use crate::{err, BackfillError, ProgramResult};

/// Group header: `chain (2) + emitter (32) + entry_count (1)`.
const GROUP_HEADER_BYTES: usize = 2 + 32 + 1;
/// Per-entry payload inside a group: `sequence (8) + digest (32)`.
const ENTRY_BYTES: usize = 8 + 32;
/// Top-level fixed head: `group_count (1)`.
const FIXED_HEAD: usize = 1;

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() < FIXED_HEAD {
        return Err(err(BackfillError::InvalidInstructionData));
    }
    let group_count = data[0] as usize;
    if group_count == 0 {
        return Err(err(BackfillError::InvalidInstructionData));
    }

    // Accounts: [WRITE, SIGNER] payer, [] solana-noreplay program (CPI target),
    // [] noreplay-authority PDA (signs MarkUsedBulk via invoke_signed), [] system program,
    // then one bucket PDA per unique (chain, emitter, bucket_index) in walk order.
    let [payer, noreplay_program, noreplay_authority, system_program, buckets @ ..] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    authority::require_authority(payer)?;

    // Tracked state across the nested walk:
    //   prev_group_key      — last (chain, emitter): strict-ascending group order
    //   prev_full           — last (chain, emitter, sequence): strict-ascending entry order
    //   prev_bucket         — last (chain, emitter, bucket_index): bucket-transition flush
    //   or_mask             — current bucket's accumulated bit pattern
    //   bucket_account_idx  — next bucket account to consume on flush
    let mut prev_group_key: Option<(u16, [u8; 32])> = None;
    let mut prev_full: Option<(u16, [u8; 32], u64)> = None;
    let mut prev_bucket: Option<(u16, [u8; 32], u64)> = None;
    let mut or_mask = [0u8; NOREPLAY_BITMAP_BYTES];
    let mut bucket_account_idx: usize = 0;

    let mut cursor = FIXED_HEAD;
    for _ in 0..group_count {
        // Group header.
        if cursor + GROUP_HEADER_BYTES > data.len() {
            return Err(err(BackfillError::InvalidInstructionData));
        }
        let chain = u16::from_be_bytes([data[cursor], data[cursor + 1]]);
        let mut emitter = [0u8; 32];
        emitter.copy_from_slice(&data[cursor + 2..cursor + 34]);
        let entry_count = data[cursor + 34] as usize;
        cursor += GROUP_HEADER_BYTES;
        if entry_count == 0 {
            return Err(err(BackfillError::InvalidInstructionData));
        }

        // Strict-ascending group order — also forbids duplicate (chain, emitter).
        let cur_group = (chain, emitter);
        if let Some(prev) = prev_group_key {
            if cur_group <= prev {
                return Err(err(BackfillError::InvalidInstructionData));
            }
        }
        prev_group_key = Some(cur_group);

        // Entries.
        for _ in 0..entry_count {
            if cursor + ENTRY_BYTES > data.len() {
                return Err(err(BackfillError::InvalidInstructionData));
            }
            let sequence = u64::from_be_bytes([
                data[cursor],
                data[cursor + 1],
                data[cursor + 2],
                data[cursor + 3],
                data[cursor + 4],
                data[cursor + 5],
                data[cursor + 6],
                data[cursor + 7],
            ]);
            let mut digest = [0u8; 32];
            digest.copy_from_slice(&data[cursor + 8..cursor + 40]);
            cursor += ENTRY_BYTES;

            // Strict-ascending order across the global walk. Group boundaries
            // are already covered by prev_group_key; checking the full tuple
            // here is the strongest single invariant.
            let cur_full = (chain, emitter, sequence);
            if let Some(prev) = prev_full {
                if cur_full <= prev {
                    return Err(err(BackfillError::InvalidInstructionData));
                }
            }

            let cur_bucket = (chain, emitter, NoReplayBitmapAccount::bucket_index(sequence));
            if let Some(prev_b) = prev_bucket {
                if cur_bucket != prev_b {
                    // Bucket change: flush previous before opening the next.
                    if bucket_account_idx >= buckets.len() {
                        return Err(err(BackfillError::InvalidInstructionData));
                    }
                    noreplay::mark_used_bulk(
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

            // `guardian_set_index = 0` sentinel: auditors verify entries against the VAA
            // archive rather than a pinned guardian set.
            commit_log::emit(chain, &emitter, sequence, &digest, 0);

            prev_full = Some(cur_full);
            prev_bucket = Some(cur_bucket);
        }
    }

    // Final flush for the last bucket.
    if let Some(prev_b) = prev_bucket {
        if bucket_account_idx >= buckets.len() {
            return Err(err(BackfillError::InvalidInstructionData));
        }
        noreplay::mark_used_bulk(
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
        return Err(err(BackfillError::InvalidInstructionData));
    }

    // Cursor must land exactly on data.len() after walking every declared group and entry;
    // trailing bytes mean a malformed wire.
    if cursor != data.len() {
        return Err(err(BackfillError::InvalidInstructionData));
    }

    Ok(())
}
