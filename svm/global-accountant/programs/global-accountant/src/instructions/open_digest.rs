//! `open_digest` — initialise a DigestAccount PDA on quorum-reach.
//!
//! For this slice the instruction is directly invokable. The real call site
//! will be `submit_observations` after a NoReplay check on the quorum-reaching
//! observation; that integration is deferred.

// Hard compile-time fence at the file root. `open_digest` is a test-build
// entrypoint until NoReplay + `submit_observations` integration lands; any
// production `cargo build-sbf` without `test-only-open-digest` must refuse to
// compile this file at all rather than rely on dead-code stripping. Mirrors
// the `mock-vaa` fence in `close_digest.rs`.
#[cfg(not(feature = "test-only-open-digest"))]
compile_error!(
    "open_digest is a test-only entrypoint until NoReplay + submit_observations \
     integration lands; production builds must not compile this module"
);

use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{clock::Clock, rent::Rent, Sysvar},
    AccountView, Address, ProgramResult,
};
use pinocchio_system::instructions::{Allocate, Assign, CreateAccount, Transfer};

use crate::definitions::{DigestAccountLayout, GlobalAccountantError, DIGEST_SEED_PREFIX};
use crate::err;
use crate::state::digest;

/// Layout of the open_digest instruction data (after the 1-byte discriminator).
/// Distinct from the account layout — see `DigestAccountLayout` in
/// `global-accountant-definitions` for that.
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
    //
    // No separate `digest_pda.address() == &expected` check: canonical-bump
    // equality already implies the PDA address is the unique one derivable
    // from `(seeds, program_id, canonical_bump)`. A passing bump check with a
    // different account address is impossible.
    let (_expected, canonical_bump) = Address::find_program_address(
        &[DIGEST_SEED_PREFIX, &chain_be, &emitter_arr, &sequence_be],
        program_id,
    );
    if bump != canonical_bump {
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
    layout.emitter = emitter_arr;
    layout.digest = digest_arr;
    layout.payer = *payer.address().as_array();
    layout.sequence = sequence;
    layout.quorum_at_slot = slot;
    layout.guardian_set_index = guardian_set_index;
    layout.chain = chain;
    digest::store(digest_pda, &layout)
}

/// Initialise the PDA, defending against the dust-DoS grief vector: an attacker
/// can `system_program::transfer(1)` to the canonical PDA address before the
/// legitimate open. A naive `CreateAccount` CPI then fails ("account already in
/// use") and the (chain, emitter, sequence) is effectively bricked. Three
/// branches:
///
/// 1. Empty + zero-lamport + system-owned -> `CreateAccount` (fast path).
/// 2. Pre-funded (lamports > 0) + system-owned + data-empty -> Transfer (top
///    up to rent-exempt minimum if short) + Allocate + Assign. Equivalent to
///    `pinocchio_system::create_account_with_minimum_balance_signed` but with
///    an explicit owner check so we return our own `InvalidPda` instead of
///    letting the system program error surface.
/// 3. Anything else (already-initialised, foreign owner) -> `InvalidPda`.
fn init_or_upgrade_pda(
    payer: &AccountView,
    digest_pda: &AccountView,
    program_id: &Address,
    signer: Signer,
    space: u64,
) -> ProgramResult {
    let rent_exempt_minimum = Rent::get()?.try_minimum_balance(space as usize)?;
    let initial_lamports = digest_pda.lamports();
    let initial_data_len = digest_pda.data_len();
    let initial_owner_is_system = digest_pda.owner() == &pinocchio_system::ID;

    if initial_data_len != 0 || !initial_owner_is_system {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    if initial_lamports == 0 {
        CreateAccount {
            from: payer,
            to: digest_pda,
            lamports: rent_exempt_minimum,
            space,
            owner: program_id,
        }
        .invoke_signed(core::slice::from_ref(&signer))?;
    } else {
        // Over-funded PDA is accepted as a gift to the protocol; `saturating_sub`
        // keeps `top_up` at 0 rather than aborting on the negative delta.
        let top_up = rent_exempt_minimum.saturating_sub(initial_lamports);
        if top_up > 0 {
            Transfer {
                from: payer,
                to: digest_pda,
                lamports: top_up,
            }
            .invoke()?;
        }
        Allocate {
            account: digest_pda,
            space,
        }
        .invoke_signed(core::slice::from_ref(&signer))?;
        Assign {
            account: digest_pda,
            owner: program_id,
        }
        .invoke_signed(core::slice::from_ref(&signer))?;
    }

    Ok(())
}
