//! Instruction handlers.
//!
//! `submit_observations` is the only public quorum-init path in production;
//! `open_digest` stays in this module only behind `test-only-open-digest` so
//! the mollusk DigestAccount-lifecycle tests can drive that PDA directly.
//! Both paths share `open_digest_inner` (this file) and `init_or_upgrade_pda`
//! (`pda_init`) — keeping them as crate-private helpers makes the
//! production-vs-test surface area for the open path a single function rather
//! than two near-duplicate code paths.

pub mod close_digest;
pub mod close_pending;
pub mod noreplay;
pub mod pda_init;
// `open_digest` is the public quorum-init entrypoint exposed only in test
// builds; in prod it is only reachable from `submit_observations` after the
// NoReplay check. The module body itself refuses to compile without
// `test-only-open-digest` (file-top `compile_error!` mirrors the `mock-vaa`
// pattern in `close_digest.rs`), and we gate the `mod` declaration so prod
// builds do not even need to parse the file.
#[cfg(feature = "test-only-open-digest")]
pub mod open_digest;
pub mod submit_observations;

use pinocchio::{
    cpi::{Seed, Signer},
    sysvars::{clock::Clock, Sysvar},
    AccountView, Address, ProgramResult,
};

use crate::definitions::{DigestAccountLayout, GlobalAccountantError, DIGEST_SEED_PREFIX};
use crate::err;
use crate::state::digest;

use self::pda_init::init_or_upgrade_pda;

/// Allocate + write a `DigestAccountLayout` at the canonical
/// `(b"digest", chain, emitter, sequence)` PDA.
///
/// Re-used by `open_digest::process` (test-build entrypoint) and by
/// `submit_observations::process`'s commit branch on quorum reach. Both call
/// sites have already parsed instruction data into the same `[u8; N]` shapes,
/// so this helper takes the raw big-endian byte arrays directly to avoid a
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
    bump: u8,
) -> ProgramResult {
    // Canonical-bump enforcement: until NoReplay lands, the only thing
    // preventing an attacker from opening multiple sibling PDAs for the same
    // `(chain, emitter, sequence)` is rejecting non-canonical bumps. We accept
    // the bump in instruction data for CU savings on the hot path, but
    // recompute the canonical bump via `find_program_address` and reject any
    // mismatch. This costs one syscall (~1.5K CU on Solana) — acceptable.
    //
    // No separate `digest_pda.address() == &expected` check: canonical-bump
    // equality already implies the PDA address is the unique one derivable
    // from `(seeds, program_id, canonical_bump)`. A passing bump check with a
    // different account address is impossible.
    let (_expected, canonical_bump) = Address::find_program_address(
        &[DIGEST_SEED_PREFIX, &chain_be, &emitter, &sequence_be],
        program_id,
    );
    if bump != canonical_bump {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    let bump_seed = [bump];
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
