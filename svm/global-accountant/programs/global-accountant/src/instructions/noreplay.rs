//! NoReplay integration — pre-check (direct account read) plus the
//! quorum-completing `MarkUsed` CPI into `solana-noreplay`.
//!
//! Two callers consume this module:
//!
//!   - `submit_observations::process` — pre-check at the top to reject replays
//!     before any signature verification or PDA work, then CPI `MarkUsed` on
//!     quorum reach to claim the slot.
//!   - `close_pending::process` — pre-check only, as trigger (b) of the
//!     permissionless cleanup ix.
//!
//! Both branches share the same direct-read pre-check (`is_marked`). The CPI
//! itself only fires from `submit_observations`; `close_pending` never holds
//! the authority PDA.
//!
//! Wire format for the noreplay program (verified against
//! `~/WormholeLabs/CoreTeam/solana-noreplay/program/src/instruction.rs` and
//! `state.rs`):
//!
//! - Account size: 129 bytes (`[bump: u8][bitmap: 128 B]`).
//! - Bit `sequence % 1024` of `bitmap` indicates whether the sequence is
//!   already marked.
//! - `MarkUsed` data wire shape: `[disc=1u8][ns_len: u16 LE][ns][seq: u64 LE]`.
//! - `MarkUsed` accounts (in order): payer (signer, writable), authority
//!   (signer, readonly), bitmap PDA (writable), system program (readonly).
//! - PDA seeds for the bitmap: `[authority, ns[..min(len, 32)], ns[min(len, 32)..],
//!   (seq / 1024).to_le_bytes()]` (`pda.rs`).
//!
//! Our authority is a PDA owned by global-accountant — derived once at
//! `[NOREPLAY_AUTHORITY_SEED_PREFIX]` — so the bitmap is exclusively
//! write-controlled by this program.

use pinocchio::{AccountView, Address, ProgramResult};

use crate::definitions::{GlobalAccountantError, NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID};
use crate::err;

/// Length of the noreplay namespace used by global-accountant: `chain_be (2 B)
/// ‖ emitter (32 B)`. The noreplay program splits namespaces > 32 bytes into
/// two seed chunks of 32 + remainder, so this constant pins the split point.
const NAMESPACE_TOTAL_LEN: usize = 2 + 32;
const NAMESPACE_CHUNK_BOUNDARY: usize = 32;

/// Re-derive the canonical noreplay bitmap PDA for
/// `(authority, chain, emitter, sequence)` under the `solana-noreplay`
/// program. Mirrors `solana_noreplay::pda::BitmapPdaSeeds`:
///
/// ```text
/// seeds = [authority, namespace[..32], namespace[32..], (sequence / 1024) LE]
/// ```
///
/// where `namespace = chain.to_be_bytes() ‖ emitter`. Used by the production
/// [`is_marked`] to reject any caller-supplied bucket account that does not
/// live at the canonical address. Returns `(address, bump)`.
///
/// `pub` so client tooling and tests can re-derive bucket addresses without
/// re-encoding the seed scheme by hand.
pub fn derive_bucket_pda(
    noreplay_authority: &Address,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> (Address, u8) {
    let mut namespace = [0u8; NAMESPACE_TOTAL_LEN];
    namespace[..2].copy_from_slice(&chain.to_be_bytes());
    namespace[2..].copy_from_slice(emitter);
    let bucket_index_bytes = (sequence / NOREPLAY_BITS_PER_BUCKET).to_le_bytes();
    let noreplay_program_id_addr = Address::from(NOREPLAY_PROGRAM_ID);
    let authority_bytes: &[u8] = noreplay_authority.as_array();
    Address::find_program_address(
        &[
            authority_bytes,
            &namespace[..NAMESPACE_CHUNK_BOUNDARY],
            &namespace[NAMESPACE_CHUNK_BOUNDARY..],
            &bucket_index_bytes,
        ],
        &noreplay_program_id_addr,
    )
}

// ============================================================================
// Pre-check (direct account read).
// ============================================================================

/// Direct-read pre-check against the noreplay bitmap PDA.
///
/// Returns:
///   - `Ok(false)` if the PDA is uninitialised (system-owned, zero data) or
///     the relevant bit is clear — proceed with the submission.
///   - `Ok(true)` if the bit at `sequence % 1024` of the bitmap is already
///     set — the submission must short-circuit as `AlreadyAccounted`.
///   - `Err(InvalidPda)` if the bucket address does not match the canonical
///     derivation, or the account data is structurally malformed.
///
/// The bucket address is re-derived from `(noreplay_authority, chain, emitter,
/// sequence)` and any caller-supplied account at a non-canonical address is
/// rejected. Without that check, a caller could pass an arbitrary bucket and
/// trick the bit lookup into reading an unrelated namespace.
///
/// No CPI; cost is dominated by one `find_program_address` (~1.5K CU) plus the
/// data borrow + bit test. The PDA may be passed as read-only here; `MarkUsed`
/// later passes it as writable, but a single `AccountView` cannot be both at
/// the same time, so the caller must pass it as writable up front (per the
/// account-list documentation in `submit_observations.rs`).
pub fn is_marked(
    bucket: &AccountView,
    noreplay_authority: &Address,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> Result<bool, pinocchio::error::ProgramError> {
    let (expected_bucket, _) = derive_bucket_pda(noreplay_authority, chain, emitter, sequence);
    if bucket.address() != &expected_bucket {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    // Uninitialised bucket -> system-owned, zero data, no bit set yet. This is
    // the normal case for the first message in a sequence bucket; do NOT
    // error.
    if bucket.owner() == &pinocchio_system::ID {
        return Ok(false);
    }
    // Past lazy-init: owner is the noreplay program (or some other owner,
    // which we treat as "untrusted, but we can still inspect the bit"). The
    // bitmap PDA must be exactly 129 bytes per `solana_noreplay::state`.
    let data = bucket.try_borrow()?;
    if data.len()
        != crate::definitions::NOREPLAY_BITMAP_OFFSET + crate::definitions::NOREPLAY_BITMAP_BYTES
    {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let bit = (sequence % NOREPLAY_BITS_PER_BUCKET) as usize;
    let byte = data[crate::definitions::NOREPLAY_BITMAP_OFFSET + bit / 8];
    Ok(byte & (1 << (bit % 8)) != 0)
}

// ============================================================================
// Mark-used (write).
//
// CPIs `MarkUsed` into solana-noreplay with `invoke_signed`-derived authority.
// Reached only from `submit_observations`'s quorum-completion branch.
// ============================================================================

/// Length of the `MarkUsed` instruction-data buffer assembled on-chain:
/// `[disc=1u8][ns_len: u16 LE][ns: 34 B][seq: u64 LE]`. Pinned here so the
/// CPI builder can use a fixed-size array on the stack rather than an
/// allocation-bearing `Vec`. The 34-byte namespace mirrors the VAA wire
/// format (`chain_be ‖ emitter`) and the `DIGEST_SEED_PREFIX` derivation in
/// `open_digest`, so on-chain and off-chain derivations agree.
const MARK_USED_DATA_LEN: usize = 1 + 2 + NAMESPACE_TOTAL_LEN + 8;

#[allow(clippy::too_many_arguments)]
pub fn mark_used(
    payer: &AccountView,
    bucket: &mut AccountView,
    _noreplay_program: &AccountView,
    noreplay_authority: &AccountView,
    system_program: &AccountView,
    program_id: &pinocchio::Address,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> ProgramResult {
    use pinocchio::cpi::{Seed, Signer};
    use pinocchio::instruction::{InstructionAccount, InstructionView};

    use crate::definitions::{NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_MARK_USED_DISCRIMINATOR};

    // No program-ID check on `_noreplay_program`: the CPI target below is
    // built from the hardcoded `NOREPLAY_PROGRAM_ID` constant, never from the
    // caller-supplied account, so a forged account cannot redirect the CPI —
    // if the real program is absent from the instruction context the
    // `invoke_signed` fails (mapped to `NoReplayCpiFailed`). The account must
    // still be passed so the runtime can resolve the callee, but it is never
    // trusted or dereferenced. Do NOT "simplify" the `InstructionView` to use
    // `_noreplay_program.address()` as the target: that would make the CPI
    // destination caller-controlled and let an attacker fake `MarkUsed`
    // success, bypassing replay protection entirely.

    // Derive the canonical noreplay-authority PDA and verify the caller
    // supplied the right account. `find_program_address` is ~1.5K CU on the
    // happy path; the alternative (passing the bump in instruction data) only
    // saves CUs at the cost of accepting a non-canonical bump — and the
    // authority lives outside the instruction wire shape today, so re-deriving
    // is the simpler interface.
    let noreplay_program_id_addr = Address::from(NOREPLAY_PROGRAM_ID);
    let (expected_authority, authority_bump) =
        Address::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], program_id);
    if noreplay_authority.address() != &expected_authority {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    // Build the 34-byte namespace on the stack: chain_be (2 B) ‖ emitter (32 B).
    let mut namespace = [0u8; NAMESPACE_TOTAL_LEN];
    namespace[..2].copy_from_slice(&chain.to_be_bytes());
    namespace[2..].copy_from_slice(emitter);

    // Build the `MarkUsed` instruction data on the stack.
    let mut ix_data = [0u8; MARK_USED_DATA_LEN];
    ix_data[0] = NOREPLAY_MARK_USED_DISCRIMINATOR;
    ix_data[1..3].copy_from_slice(&(NAMESPACE_TOTAL_LEN as u16).to_le_bytes());
    ix_data[3..3 + NAMESPACE_TOTAL_LEN].copy_from_slice(&namespace);
    ix_data[3 + NAMESPACE_TOTAL_LEN..].copy_from_slice(&sequence.to_le_bytes());

    // `MarkUsed` account list (verified against `solana_noreplay::instruction::MarkUsedAccounts`):
    //   0. [signer, writable] payer
    //   1. [signer, readonly] authority (our PDA)
    //   2. [writable]         bitmap PDA
    //   3. [readonly]         system program
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

    // `invoke_signed` seeds for the noreplay-authority PDA: just the prefix +
    // canonical bump.
    let bump_seed = [authority_bump];
    let signer_seeds = [
        Seed::from(NOREPLAY_AUTHORITY_SEED_PREFIX),
        Seed::from(bump_seed.as_slice()),
    ];
    let signers = [Signer::from(&signer_seeds)];

    // The CPI's failure surface includes `AccountAlreadyInitialized` — the
    // race-against-another-tx case where someone else marked the bit between
    // our pre-check and our CPI. Promote that (and any other CPI failure) to
    // our `NoReplayCpiFailed` error code so on-chain logs disambiguate from
    // the pre-check `AlreadyAccounted`.
    pinocchio::cpi::invoke_signed(
        &instruction,
        &[payer, noreplay_authority, bucket, system_program],
        &signers,
    )
    .map_err(|_| err(GlobalAccountantError::NoReplayCpiFailed))
}
