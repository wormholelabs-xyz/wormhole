//! NoReplay `MarkUsed` CPI helper. Byte-identical wire format to the
//! operational program's helper; duplicated here for the reasons documented in
//! `commit_log.rs`. The backfill program shares the operational program's
//! `noreplay-authority` PDA (same seed) so a future race-window where both
//! programs are deployed simultaneously could not be exploited to write
//! conflicting bits — though by design the backfill program is upgraded out
//! before the operational program lands.

use pinocchio::{AccountView, Address, ProgramResult};

use crate::definitions::{NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_MARK_USED_DISCRIMINATOR, NOREPLAY_PROGRAM_ID};
use crate::{err, BackfillError};

const NAMESPACE_TOTAL_LEN: usize = 2 + 32;
const MARK_USED_DATA_LEN: usize = 1 + 2 + NAMESPACE_TOTAL_LEN + 8;

#[allow(clippy::too_many_arguments)]
pub fn mark_used(
    payer: &AccountView,
    bucket: &mut AccountView,
    _noreplay_program: &AccountView,
    noreplay_authority: &AccountView,
    system_program: &AccountView,
    program_id: &Address,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> ProgramResult {
    use pinocchio::cpi::{Seed, Signer};
    use pinocchio::instruction::{InstructionAccount, InstructionView};

    // SECURITY: target program is the hardcoded constant, never the supplied
    // account address — a caller-controlled target would let an attacker fake
    // `MarkUsed` success and write fictitious bits.
    let noreplay_program_id_addr = Address::from(NOREPLAY_PROGRAM_ID);
    let (expected_authority, authority_bump) =
        Address::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], program_id);
    if noreplay_authority.address() != &expected_authority {
        return Err(err(BackfillError::InvalidPda));
    }

    let mut namespace = [0u8; NAMESPACE_TOTAL_LEN];
    namespace[..2].copy_from_slice(&chain.to_be_bytes());
    namespace[2..].copy_from_slice(emitter);

    let mut ix_data = [0u8; MARK_USED_DATA_LEN];
    ix_data[0] = NOREPLAY_MARK_USED_DISCRIMINATOR;
    ix_data[1..3].copy_from_slice(&(NAMESPACE_TOTAL_LEN as u16).to_le_bytes());
    ix_data[3..3 + NAMESPACE_TOTAL_LEN].copy_from_slice(&namespace);
    ix_data[3 + NAMESPACE_TOTAL_LEN..].copy_from_slice(&sequence.to_le_bytes());

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

    pinocchio::cpi::invoke_signed(
        &instruction,
        &[payer, noreplay_authority, bucket, system_program],
        &signers,
    )
    .map_err(|_| err(BackfillError::NoReplayCpiFailed))
}
