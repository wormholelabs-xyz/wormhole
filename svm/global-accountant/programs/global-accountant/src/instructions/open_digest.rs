//! Shared DigestAccount-open implementation, called by `submit_observations`
//! (on quorum), `submit_vaas` (after Shim verification), and the test-only
//! `test_only_open_digest` entrypoint.

use pinocchio::{
    cpi::{Seed, Signer},
    sysvars::{clock::Clock, Sysvar},
    AccountView, Address, ProgramResult,
};

use crate::definitions::{DigestAccountLayout, DIGEST_SEED_PREFIX};
use crate::state::digest;

use super::pda_init::init_or_upgrade_pda;

/// Allocate + write a `DigestAccountLayout` at the canonical
/// `(b"digest", chain, emitter, sequence)` PDA. Takes raw big-endian byte arrays
/// matching the callers' already-parsed instruction data.
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
    // Canonical bump derived on-chain; `invoke_signed` below only signs for the
    // canonical address. A non-canonical DigestAccount would strand its rent
    // (close_digest only derives the canonical address) and miss relayer lookups.
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

    // Construct via `Zeroable` + named-field assignment since `_padding` is
    // crate-private in the definitions crate.
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
