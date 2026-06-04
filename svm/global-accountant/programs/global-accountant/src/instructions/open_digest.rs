//! Shared DigestAccount-open implementation.
//!
//! Compiled in every build shape. Both production commit paths call
//! [`open_digest_inner`] directly: `submit_observations::process` on quorum
//! reach, and `submit_vaas::process` after Shim verification. The standalone
//! `test_only_open_digest` entrypoint (gated behind `test-only-open-digest`)
//! wraps the same helper so mollusk tests can drive the PDA lifecycle
//! without standing up a full 13-of-19 quorum or a verified VAA.

use pinocchio::{
    cpi::{Seed, Signer},
    sysvars::{clock::Clock, Sysvar},
    AccountView, Address, ProgramResult,
};

use crate::definitions::{DigestAccountLayout, DIGEST_SEED_PREFIX};
use crate::state::digest;

use super::pda_init::init_or_upgrade_pda;

/// Allocate + write a `DigestAccountLayout` at the canonical
/// `(b"digest", chain, emitter, sequence)` PDA.
///
/// Re-used by `submit_observations::process`'s commit branch on quorum reach,
/// by `submit_vaas::process` after Shim verification, and by
/// `test_only_open_digest::process` (test-build entrypoint). All call sites have
/// already parsed instruction data into the same `[u8; N]` shapes, so this
/// helper takes the raw big-endian byte arrays directly to avoid a
/// double-conversion through `u16` / `u64` here only to re-encode for the
/// `find_program_address` call.
#[allow(clippy::too_many_arguments)]
pub(crate) fn open_digest_inner(
    program_id: &Address,
    payer: &AccountView,
    digest_pda: &mut AccountView,
    chain_be: [u8; 2],
    emitter: [u8; 32],
    sequence_be: [u8; 8],
    digest_bytes: [u8; 32],
    guardian_set_index: u32,
) -> ProgramResult {
    // The canonical bump is derived on-chain — callers never supply it.
    // NoReplay reserves the `(chain, emitter, sequence)` slot atomically with
    // the commit branch, but that does not pin the DigestAccount to its
    // canonical address; deriving the bump here does. A DigestAccount at a
    // non-canonical address would strand its rent forever (`close_digest`
    // only ever derives the canonical address) and relayer-side lookups would
    // miss the on-chain breadcrumb. `invoke_signed` below only signs for the
    // canonical address, so a caller-supplied account anywhere else fails the
    // init CPI. One `find_program_address` syscall (~1.5K CU) — acceptable.
    let (_expected, canonical_bump) = Address::find_program_address(
        &[DIGEST_SEED_PREFIX, &chain_be, &emitter, &sequence_be],
        program_id,
    );

    let bump_seed = [canonical_bump];
    let seeds = [
        Seed::from(DIGEST_SEED_PREFIX),
        Seed::from(chain_be.as_slice()),
        Seed::from(emitter.as_slice()),
        Seed::from(sequence_be.as_slice()),
        Seed::from(bump_seed.as_slice()),
    ];
    let signer = Signer::from(&seeds);

    init_or_upgrade_pda(
        payer,
        digest_pda,
        program_id,
        signer,
        DigestAccountLayout::LEN as u64,
    )?;

    let slot = Clock::get()?.slot;

    // `_padding` is crate-private in the definitions crate, so we initialise
    // through `Zeroable` and assign named fields instead of using a struct
    // literal. The padding bytes are zero on a freshly allocated account; this
    // is purely defence in depth (and the only way to construct the layout
    // from outside the definitions crate). See
    // `DigestAccountLayout._padding` in `crates/definitions/src/lib.rs` for
    // the privacy rationale.
    let mut layout: DigestAccountLayout = bytemuck::Zeroable::zeroed();
    layout.emitter = emitter;
    layout.digest = digest_bytes;
    layout.payer = *payer.address().as_array();
    layout.sequence = u64::from_be_bytes(sequence_be);
    layout.quorum_at_slot = slot;
    layout.guardian_set_index = guardian_set_index;
    layout.chain = u16::from_be_bytes(chain_be);
    digest::store(digest_pda, &layout)
}
