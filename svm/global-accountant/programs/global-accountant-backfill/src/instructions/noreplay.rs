//! NoReplay `MarkUsedBulk` CPI helper.
//!
//! Per-entry `MarkUsed` was costing ~3,065 CU each — mostly CPI dispatch +
//! arg deser + `find_program_address` inside noreplay. `MarkUsedBulk` does
//! the same allocation-and-write work but accepts a 128-byte OR mask, so a
//! single CPI flips up to 1024 bits in one shot. Wire format frozen by the
//! sibling noreplay extension; see `NOREPLAY_MARK_USED_BULK_DISCRIMINATOR`
//! in `crates/definitions`.

use pinocchio::{AccountView, Address, ProgramResult};

use crate::definitions::{
    NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITMAP_BYTES, NOREPLAY_MARK_USED_BULK_DISCRIMINATOR,
    NOREPLAY_PROGRAM_ID,
};
use crate::{err, BackfillError};

const NAMESPACE_TOTAL_LEN: usize = 2 + 32;
/// `[disc: u8][ns_len: u16 LE][ns: 34 B][bucket_index: u64 LE][or_mask: 128 B]`.
const BULK_DATA_LEN: usize = 1 + 2 + NAMESPACE_TOTAL_LEN + 8 + NOREPLAY_BITMAP_BYTES;

#[allow(clippy::too_many_arguments)]
pub fn mark_used_bulk(
    payer: &AccountView,
    bucket: &mut AccountView,
    _noreplay_program: &AccountView,
    noreplay_authority: &AccountView,
    system_program: &AccountView,
    program_id: &Address,
    chain: u16,
    emitter: &[u8; 32],
    bucket_index: u64,
    or_mask: &[u8; NOREPLAY_BITMAP_BYTES],
) -> ProgramResult {
    use pinocchio::cpi::{Seed, Signer};
    use pinocchio::instruction::{InstructionAccount, InstructionView};

    // SECURITY: target program is the hardcoded constant, never the supplied
    // account address — a caller-controlled target would let an attacker fake
    // success and write fictitious state.
    let noreplay_program_id_addr = Address::from(NOREPLAY_PROGRAM_ID);
    let (expected_authority, authority_bump) =
        Address::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], program_id);
    if noreplay_authority.address() != &expected_authority {
        return Err(err(BackfillError::InvalidPda));
    }

    let mut namespace = [0u8; NAMESPACE_TOTAL_LEN];
    namespace[..2].copy_from_slice(&chain.to_be_bytes());
    namespace[2..].copy_from_slice(emitter);

    let mut ix_data = [0u8; BULK_DATA_LEN];
    ix_data[0] = NOREPLAY_MARK_USED_BULK_DISCRIMINATOR;
    ix_data[1..3].copy_from_slice(&(NAMESPACE_TOTAL_LEN as u16).to_le_bytes());
    ix_data[3..3 + NAMESPACE_TOTAL_LEN].copy_from_slice(&namespace);
    let bucket_index_offset = 3 + NAMESPACE_TOTAL_LEN;
    ix_data[bucket_index_offset..bucket_index_offset + 8]
        .copy_from_slice(&bucket_index.to_le_bytes());
    let mask_offset = bucket_index_offset + 8;
    ix_data[mask_offset..mask_offset + NOREPLAY_BITMAP_BYTES].copy_from_slice(or_mask);

    let ix_accounts = [
        InstructionAccount::writable_signer(payer.address()),
        InstructionAccount::readonly_signer(noreplay_authority.address()),
        InstructionAccount::writable(bucket.address()),
        InstructionAccount::readonly(system_program.address()),
    ];

    let instruction = InstructionView {
        program_id: &noreplay_program_id_addr,
        data: &ix_data,
        accounts: &ix_accounts,
    };

    let bump_seed = [authority_bump];
    let signer_seeds = [
        Seed::from(NOREPLAY_AUTHORITY_SEED_PREFIX),
        Seed::from(bump_seed.as_slice()),
    ];
    let signers = [Signer::from(&signer_seeds)];

    // `invoke_signed` itself only returns `Err(...)` for *pre-CPI validation*
    // failures — `NotEnoughAccountKeys`, `InvalidArgument` (address mismatch),
    // borrow-check conflicts. If the inner noreplay program returns a
    // `ProgramError`, the SBF runtime aborts THIS program with the inner
    // exit code directly; pinocchio's `invoke_signed` never sees that error.
    // So mapping the returned `Result` to a custom "CPI failed" code would
    // only ever fire on caller bugs in this helper, which are better surfaced
    // as their natural variant for debuggability. Pass through unchanged.
    pinocchio::cpi::invoke_signed(
        &instruction,
        &[payer, noreplay_authority, bucket, system_program],
        &signers,
    )
}
