//! `open_digest` — initialise a DigestAccount PDA on quorum-reach.
//!
//! For this slice the instruction is directly invokable. The real call site
//! will be `submit_observations` after a NoReplay check on the quorum-reaching
//! observation; that integration is deferred.

use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{clock::Clock, Sysvar},
    AccountView, Address, ProgramResult,
};
use pinocchio_system::instructions::CreateAccount;

use crate::definitions::{DigestAccountLayout, GlobalAccountantError, DIGEST_SEED_PREFIX};
use crate::err;
use crate::state::digest;

/// Layout of the open_digest instruction data (after the 1-byte discriminator):
///
/// | offset | size | field                              |
/// |--------|------|------------------------------------|
/// | 0      | 2    | chain (big endian)                 |
/// | 2      | 32   | emitter                            |
/// | 34     | 8    | sequence (big endian)              |
/// | 42     | 32   | digest                             |
/// | 74     | 4    | guardian_set_index (little endian) |
/// | 78     | 1    | bump                               |
const OPEN_DIGEST_DATA_LEN: usize = 2 + 32 + 8 + 32 + 4 + 1;

pub fn process(
    program_id: &Address,
    accounts: &mut [AccountView],
    data: &[u8],
) -> ProgramResult {
    let data: &[u8; OPEN_DIGEST_DATA_LEN] = data
        .try_into()
        .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;

    let (chain_bytes, rest) = data.split_at(2);
    let (emitter, rest) = rest.split_at(32);
    let (sequence_bytes, rest) = rest.split_at(8);
    let (digest_bytes, rest) = rest.split_at(32);
    let (gsi_bytes, bump_byte) = rest.split_at(4);

    let chain_be: [u8; 2] = chain_bytes.try_into().unwrap();
    let emitter_arr: [u8; 32] = emitter.try_into().unwrap();
    let sequence_be: [u8; 8] = sequence_bytes.try_into().unwrap();
    let digest_arr: [u8; 32] = digest_bytes.try_into().unwrap();
    let guardian_set_index = u32::from_le_bytes(gsi_bytes.try_into().unwrap());
    let bump = bump_byte[0];

    let chain = u16::from_be_bytes(chain_be);
    let sequence = u64::from_be_bytes(sequence_be);

    // Accounts:
    //   0. `[WRITE, SIGNER]` payer (rent funder)
    //   1. `[WRITE]`         digest PDA (uninit)
    //   2. `[]`              system program
    let [payer, digest_pda, _system_program] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !payer.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    // TODO: real NoReplay CPI here. For this slice, opening is unconditional —
    // the integration test owns the replay invariant.

    // Canonical-bump enforcement: until NoReplay lands, the only thing
    // preventing an attacker from opening multiple sibling PDAs for the same
    // `(chain, emitter, sequence)` is rejecting non-canonical bumps. We accept
    // the bump in instruction data for CU savings on the hot path, but
    // recompute the canonical bump via `find_program_address` and reject any
    // mismatch. This costs one syscall (~1.5K CU on Solana) — acceptable.
    let (expected, canonical_bump) = Address::find_program_address(
        &[DIGEST_SEED_PREFIX, &chain_be, &emitter_arr, &sequence_be],
        program_id,
    );
    if bump != canonical_bump || digest_pda.address() != &expected {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    let bump_seed = [bump];
    let seeds = [
        Seed::from(DIGEST_SEED_PREFIX),
        Seed::from(chain_be.as_slice()),
        Seed::from(emitter_arr.as_slice()),
        Seed::from(sequence_be.as_slice()),
        Seed::from(bump_seed.as_slice()),
    ];
    let signer = Signer::from(&seeds);

    CreateAccount::with_minimum_balance(
        payer,
        digest_pda,
        DigestAccountLayout::LEN as u64,
        program_id,
        None,
    )?
    .invoke_signed(&[signer])?;

    let slot = Clock::get()?.slot;

    // `_padding` is crate-private in the definitions crate, so we initialise
    // through `Zeroable` and assign named fields instead of using a struct
    // literal. The padding bytes are guaranteed zero on a freshly allocated
    // account, so this is purely defence in depth.
    let mut layout: DigestAccountLayout = bytemuck::Zeroable::zeroed();
    layout.emitter = emitter_arr;
    layout.digest = digest_arr;
    layout.payer = *payer.address().as_array();
    layout.sequence = sequence;
    layout.quorum_at_slot = slot;
    layout.guardian_set_index = guardian_set_index;
    layout.chain = chain;
    digest::store(digest_pda, &layout)
}
