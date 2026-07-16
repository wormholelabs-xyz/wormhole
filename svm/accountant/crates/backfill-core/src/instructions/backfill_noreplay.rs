//! `BackfillNoReplay` — flip `solana-noreplay` bits for a batch of
//! `(chain, emitter, sequence, digest)` entries and emit one canonical
//! `ACCDGST\0` commit-log per entry.
//!
//! ## Wire format — emitter-grouped (compact)
//!
//! Real backfill traffic clusters: many entries from the same
//! `(chain, emitter)` fall into a small set of buckets (1024 sequences each).
//! The 1232-byte tx wire limit makes the per-entry encoding the binding cost,
//! not CU. We therefore encode entries grouped by `(chain, emitter)`:
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
//! Per-entry footprint is **40 bytes** (down from 74 in the old per-entry
//! format) since `(chain, emitter)` are stated once per group rather than
//! repeated for every entry. Approximately doubles the entries fittable
//! into a single tx — see the master plan's wire-budget analysis.
//!
//! Strict ordering rules (enforced inline; reject `InvalidInstructionData`):
//!   - Groups MUST appear in strictly ascending `(chain, emitter)`.
//!   - Within a group, entries MUST appear in strictly ascending `sequence`.
//!   - Groups MUST be non-empty (`entry_count ≥ 1`).
//!   - Top-level `group_count` MUST be ≥ 1.
//!
//! ## Bulk-CPI design
//!
//! Consecutive entries that share `(chain, emitter, sequence / 1024)` OR
//! their bits into a single 128-byte mask and emit one `MarkUsedBulk` CPI
//! per unique bucket. Drops per-entry NoReplay CPU from ~3,000 CU to ~80 CU.
//!
//! ## Trust model
//!
//! The program does NOT verify VAA signatures. The audit chain is lazily
//! verifiable by any third party with the Solana ledger archive and a VAA
//! archive — see `lib.rs` and the master plan
//! `accountant-migration-backfill.md`. Every entry emits a canonical
//! `ACCDGST\0` commit-log; that is the audit primitive.

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

use crate::definitions::{Pubkey, NOREPLAY_BITMAP_BYTES, NOREPLAY_BITS_PER_BUCKET};
use crate::instructions::{authority::require_authority, commit_log, noreplay};
use crate::{err, BackfillError};

/// Group header: `chain (2) + emitter (32) + entry_count (1)`.
const GROUP_HEADER_BYTES: usize = 2 + 32 + 1;
/// Per-entry payload inside a group: `sequence (8) + digest (32)`.
const ENTRY_BYTES: usize = 8 + 32;
/// Top-level fixed head: `group_count (1)`.
const FIXED_HEAD: usize = 1;

pub fn process(
    program_id: &Address,
    accounts: &mut [AccountView],
    data: &[u8],
    authority: &Pubkey,
) -> ProgramResult {
    // ----- (1) Parse + validate top-level head -----
    if data.len() < FIXED_HEAD {
        return Err(err(BackfillError::InvalidInstructionData));
    }
    let group_count = data[0] as usize;
    if group_count == 0 {
        return Err(err(BackfillError::InvalidInstructionData));
    }

    // ----- (2) Accounts layout -----
    //
    //   0. [WRITE, SIGNER] payer — must equal `BACKFILL_AUTHORITY`
    //   1. [ ]             solana-noreplay program (CPI target)
    //   2. [ ]             noreplay-authority PDA (signs MarkUsedBulk via invoke_signed)
    //   3. [ ]             system program
    //   4..4+M.            bucket PDAs, one per UNIQUE
    //                      (chain, emitter, sequence/1024) in the order the
    //                      handler walks them. M is implicit; the handler
    //                      verifies it consumes exactly all bucket slots.
    let [payer, noreplay_program, noreplay_authority, system_program, buckets @ ..] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // ----- (3) Authority gate -----
    require_authority(payer, authority)?;
    let payer: &AccountView = payer;

    // ----- (4) Walk groups → entries, OR-mask per bucket, flush on transition -----
    //
    // Tracked state across the nested walk:
    //   prev_group_key  — last (chain, emitter), for strict-ascending group order
    //   prev_full       — last (chain, emitter, sequence), for strict-ascending entry order
    //   prev_bucket     — last (chain, emitter, sequence/1024), for bucket-transition flush
    //   or_mask         — current bucket's accumulated bit pattern
    //   bucket_account_idx — next bucket account to consume on flush
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
            let sequence =
                u64::from_be_bytes(data[cursor..cursor + 8].try_into().expect("8 bytes"));
            let mut digest = [0u8; 32];
            digest.copy_from_slice(&data[cursor + 8..cursor + 40]);
            cursor += ENTRY_BYTES;

            // Strict-ascending entry order within the global walk — across
            // group boundaries the (chain, emitter) component already enforces
            // it via prev_group_key, so checking the full tuple here is the
            // strongest single invariant.
            let cur_full = (chain, emitter, sequence);
            if let Some(prev) = prev_full {
                if cur_full <= prev {
                    return Err(err(BackfillError::InvalidInstructionData));
                }
            }

            let cur_bucket = (chain, emitter, sequence / NOREPLAY_BITS_PER_BUCKET);
            if let Some(prev_b) = prev_bucket {
                if cur_bucket != prev_b {
                    // Bucket change: flush previous before opening the next.
                    if bucket_account_idx >= buckets.len() {
                        return Err(err(BackfillError::InvalidInstructionData));
                    }
                    noreplay::mark_used_bulk(
                        payer,
                        &mut buckets[bucket_account_idx],
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

            let bit = (sequence % NOREPLAY_BITS_PER_BUCKET) as usize;
            or_mask[bit / 8] |= 1u8 << (bit % 8);

            // `guardian_set_index = 0` sentinel: backfill entries are not
            // pinned to a specific guardian set; auditors verify against the
            // VAA archive. Emitted unconditionally — if the bucket flush
            // below fails the tx rolls back and indexers will not process
            // these logs.
            commit_log::emit(chain, &emitter, sequence, &digest, 0);

            prev_full = Some(cur_full);
            prev_bucket = Some(cur_bucket);
        }
    }

    // ----- (5) Final flush for the last bucket -----
    if let Some(prev_b) = prev_bucket {
        if bucket_account_idx >= buckets.len() {
            return Err(err(BackfillError::InvalidInstructionData));
        }
        noreplay::mark_used_bulk(
            payer,
            &mut buckets[bucket_account_idx],
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

    // ----- (6) Exact-count check on bucket accounts -----
    //
    // A trailing unused bucket account means the caller passed dead state
    // (potential griefing on rent). A trailing shortfall is impossible — the
    // in-loop bound check would have caught it.
    if bucket_account_idx != buckets.len() {
        return Err(err(BackfillError::InvalidInstructionData));
    }

    // ----- (7) Exact-length check on instruction data -----
    //
    // After walking every declared group and entry, the cursor must have
    // consumed all bytes. Trailing bytes are a malformed wire and rejected.
    if cursor != data.len() {
        return Err(err(BackfillError::InvalidInstructionData));
    }

    Ok(())
}
