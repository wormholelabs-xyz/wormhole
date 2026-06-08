//! `BackfillNoReplay` — flip `solana-noreplay` bits for a batch of
//! `(chain, emitter, sequence, digest)` entries and emit one canonical
//! `ACCDGST\0` commit-log per entry.
//!
//! ## Bulk-CPI design
//!
//! Per-entry `MarkUsed` was costing ~3,000 CU each — mostly CPI dispatch +
//! arg deser + PDA verification inside noreplay. Real backfill traffic
//! clusters: many entries from the same `(chain, emitter)` fall into a single
//! 1024-bit bucket. We therefore:
//!
//! 1. Require entries strictly ascending by `(chain, emitter, sequence)` —
//!    enforced inline. Sort is the caller's responsibility; off-chain
//!    orchestrator has full `std`/`alloc` and can sort the catalogue once.
//! 2. Group consecutive entries that share `(chain, emitter, sequence / 1024)`
//!    and OR their bits into a single 128-byte mask.
//! 3. Emit one `MarkUsedBulk` CPI per unique bucket, dropping per-entry CU
//!    from ~3,000 to ~80 in the dense-bucket case.
//!
//! ## Trust model
//!
//! The program does NOT verify VAA signatures. The audit chain is lazily
//! verifiable by any third party with the Solana ledger archive and a VAA
//! archive — see `lib.rs` and the master plan
//! `accountant-migration-backfill.md`. Every entry emits a canonical
//! `ACCDGST\0` commit-log; that is the audit primitive.

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

use crate::definitions::{NOREPLAY_BITMAP_BYTES, NOREPLAY_BITS_PER_BUCKET};
use crate::instructions::{authority, commit_log, noreplay};
use crate::{err, BackfillError};

/// Wire format (after the 1-byte dispatch discriminator):
///
/// | offset  | size  | field             |
/// |---------|-------|-------------------|
/// | 0       | 1     | count             |
/// | 1+i*74  | 2     | chain (BE)        |
/// | 3+i*74  | 32    | emitter           |
/// | 35+i*74 | 8     | sequence (BE)     |
/// | 43+i*74 | 32    | digest            |
///
/// Entries MUST be strictly ascending by `(chain, emitter, sequence)`.
const ENTRY_BYTES: usize = 2 + 32 + 8 + 32;
const FIXED_HEAD: usize = 1;

pub fn process(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    // ----- (1) Parse + validate wire data -----
    if data.len() < FIXED_HEAD {
        return Err(err(BackfillError::InvalidInstructionData));
    }
    let count = data[0] as usize;
    if count == 0 {
        return Err(err(BackfillError::InvalidInstructionData));
    }
    let expected_len = FIXED_HEAD + count * ENTRY_BYTES;
    if data.len() != expected_len {
        return Err(err(BackfillError::InvalidInstructionData));
    }

    // ----- (2) Accounts layout -----
    //
    //   0. [WRITE, SIGNER] payer
    //   1. [WRITE]         backfill authority PDA (lazy-init on first call)
    //   2. [ ]             solana-noreplay program (CPI target)
    //   3. [ ]             noreplay-authority PDA (signs MarkUsedBulk via invoke_signed)
    //   4. [ ]             system program
    //   5..5+M.            bucket PDAs, one per UNIQUE (chain, emitter, bucket_index)
    //                      in the same order they appear in `data`. M is implicit;
    //                      the handler verifies it walks exactly the right count.
    let [payer, backfill_auth, noreplay_program, noreplay_authority, system_program, buckets @ ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // ----- (3) Authority gate -----
    authority::require_authority_or_init(program_id, payer, backfill_auth, system_program)?;
    let payer: &AccountView = payer;

    // ----- (4) Walk entries, grouping by bucket, OR-masking, flushing on transition -----
    //
    // Two parallel keys per entry:
    //   `prev_full`   = (chain, emitter, sequence)        used for strict-ascending check
    //   `prev_bucket` = (chain, emitter, sequence/1024)   used for bucket-grouping
    //
    // Strict-ascending on `prev_full` also forbids duplicate entries — a
    // duplicate would fail the `<=` check.
    let mut prev_full: Option<(u16, [u8; 32], u64)> = None;
    let mut prev_bucket: Option<(u16, [u8; 32], u64)> = None;
    let mut or_mask = [0u8; NOREPLAY_BITMAP_BYTES];
    let mut bucket_account_idx: usize = 0;

    for i in 0..count {
        let off = FIXED_HEAD + i * ENTRY_BYTES;
        let chain = u16::from_be_bytes([data[off], data[off + 1]]);
        let mut emitter = [0u8; 32];
        emitter.copy_from_slice(&data[off + 2..off + 34]);
        let sequence =
            u64::from_be_bytes(data[off + 34..off + 42].try_into().expect("8 bytes"));
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&data[off + 42..off + 74]);

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

        // `guardian_set_index = 0` sentinel: backfill entries are not pinned
        // to a specific guardian set; auditors verify against the VAA archive.
        // Emitted unconditionally — if the bucket flush below fails the tx
        // rolls back and indexers will not process these logs.
        commit_log::emit(chain, &emitter, sequence, &digest, 0);

        prev_full = Some(cur_full);
        prev_bucket = Some(cur_bucket);
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

    Ok(())
}
