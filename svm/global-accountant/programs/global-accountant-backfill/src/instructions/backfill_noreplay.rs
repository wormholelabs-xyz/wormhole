//! `BackfillNoReplay` — bulk flip `solana-noreplay` bits for a batch of
//! `(chain, emitter, sequence, digest)` entries and emit one canonical
//! `ACCDGST\0` commit-log per entry.
//!
//! Trust model: the program does NOT verify VAA signatures. The audit chain is
//! lazily verifiable by any third party with the Solana ledger archive and a
//! VAA archive — see the comments in `lib.rs` and the master plan
//! `accountant-migration-backfill.md`.

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

use crate::instructions::{authority, commit_log, noreplay};
use crate::{err, BackfillError};

/// Wire format (after the 1-byte dispatch discriminator):
///
/// | offset | size  | field             |
/// |--------|-------|-------------------|
/// | 0      | 1     | count             |
/// | 1+i*74 | 2     | chain (BE)        |
/// | 3+i*74 | 32    | emitter           |
/// | 35+i*74| 8     | sequence (BE)     |
/// | 43+i*74| 32    | digest            |
const ENTRY_BYTES: usize = 2 + 32 + 8 + 32;
const FIXED_HEAD: usize = 1; // count byte

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
    //   3. [ ]             noreplay-authority PDA (signs MarkUsed via invoke_signed)
    //   4. [ ]             system program
    //   5..5+count.        bucket PDAs, one per entry (writable; init by noreplay)
    let [payer, backfill_auth, noreplay_program, noreplay_authority, system_program, buckets @ ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if buckets.len() != count {
        return Err(err(BackfillError::InvalidInstructionData));
    }

    // ----- (3) Authority gate -----
    authority::require_authority_or_init(program_id, payer, backfill_auth, system_program)?;
    // Re-immutable-borrow after init; subsequent uses don't need write access.
    let payer: &AccountView = payer;

    // ----- (4) Drive every entry: CPI MarkUsed then emit canonical log -----
    //
    // The noreplay program is the source of truth for bucket canonicality —
    // a non-canonical bucket pubkey in `buckets[i]` would fail the seed
    // verification inside `MarkUsed`. The commit-log emission therefore comes
    // AFTER the CPI returns Ok, so the log is never written without a real
    // state mutation. Failure on any entry aborts the whole tx (rolling back
    // all prior entries this tx) — guard against partially-applied state.
    for (i, bucket) in buckets.iter_mut().enumerate() {
        let off = FIXED_HEAD + i * ENTRY_BYTES;
        let chain = u16::from_be_bytes([data[off], data[off + 1]]);
        let mut emitter = [0u8; 32];
        emitter.copy_from_slice(&data[off + 2..off + 34]);
        let sequence =
            u64::from_be_bytes(data[off + 34..off + 42].try_into().expect("8 bytes"));
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&data[off + 42..off + 74]);

        noreplay::mark_used(
            payer,
            bucket,
            noreplay_program,
            noreplay_authority,
            system_program,
            program_id,
            chain,
            &emitter,
            sequence,
        )?;

        // `guardian_set_index = 0` sentinel: backfill entries are not pinned
        // to a specific guardian set; auditors verify against the VAA archive.
        commit_log::emit(chain, &emitter, sequence, &digest, 0);
    }

    Ok(())
}
