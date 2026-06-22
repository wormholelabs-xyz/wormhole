//! NTT `submit_vaas` — permissionless signed-VAA backfill over the shared core
//! primitives.
//!
//! Consumes a fully-signed VAA via the Verify VAA Shim CPI and applies the NTT
//! transfer flow directly, bypassing the quorum tracker. The Shim verify,
//! NoReplay mark, and commit-log emit reuse `accountant-operational-core`; only
//! the balance work and account layout are NTT-specific. Shares NoReplay state
//! with `submit_observations`: once `(chain, emitter, seq)` is marked, any later
//! caller on either path is rejected as `AlreadyAccounted`.

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

use accountant_operational_core::hash::double_keccak256;
use accountant_operational_core::instructions::{commit_log, noreplay, shim};

use crate::definitions::{parse_vaa_namespace_key, GlobalAccountantError, VAA_BODY_HEADER_LEN};
use crate::err;
use crate::instructions::ntt_transfer::apply_ntt_transfer;

/// Wire format (after the 1-byte dispatch discriminator), identical to WTT:
/// `guardian_set_bump(1) ‖ body_len(u16 LE) ‖ body`.
const SUBMIT_VAAS_FIXED_LEN: usize = 1 + 2;

/// NTT `submit_vaas`. Account layout (mirrors WTT slots 0..6, then the six NTT
/// transfer accounts in place of WTT's chain-registration slot):
///
///   0. `[WRITE, SIGNER]` submitter — fee / rent payer for lazy PDAs.
///   1. `[]`              Verify VAA Shim program (CPI target).
///   2. `[]`              Core Bridge `GuardianSet` PDA.
///   3. `[]`              `GuardianSignatures` PDA (posted via the Shim).
///   4. `[WRITE]`         NoReplay bitmap PDA.
///   5. `[]`              NoReplay program (CPI target).
///   6. `[]`              NoReplay authority PDA owned by this program.
///   7. `[]`              system program.
///   8. `[]`              relayer-registration PDA for `emitter_chain`.
///   9. `[]`              TransceiverHub PDA `(emitter_chain, sender)`.
///  10. `[]`              TransceiverPeer PDA `(emitter_chain, sender, recipient_chain)`.
///  11. `[]`              TransceiverPeer PDA `(recipient_chain, source_peer, emitter_chain)`.
///  12. `[WRITE]`         source balance `(emitter_chain, hub_chain, hub_address)`.
///  13. `[WRITE]`         dest balance `(recipient_chain, hub_chain, hub_address)`.
pub fn process(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    // ----- (1) Parse wire data -----
    if data.len() < SUBMIT_VAAS_FIXED_LEN {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let guardian_set_bump = data[0];
    let body_len = u16::from_le_bytes([data[1], data[2]]) as usize;
    if body_len <= VAA_BODY_HEADER_LEN || data.len() != SUBMIT_VAAS_FIXED_LEN + body_len {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let body_bytes = &data[SUBMIT_VAAS_FIXED_LEN..SUBMIT_VAAS_FIXED_LEN + body_len];

    // ----- (2) Compute digest -----
    let digest = double_keccak256(body_bytes);

    let [submitter, _verify_vaa_shim_program, guardian_set, guardian_signatures, noreplay_bucket, noreplay_program, noreplay_authority, system_program_acc, transfer_accounts @ ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !submitter.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    // ----- (3) Shim CPI to verify the digest against the posted sigs -----
    shim::verify_vaa(
        guardian_set,
        guardian_signatures,
        &digest,
        guardian_set_bump,
    )?;

    // ----- (4) Parse the body header -----
    let header = parse_vaa_namespace_key(body_bytes).map_err(err)?;
    let (chain, emitter, sequence) = (header.chain, header.emitter, header.sequence);

    // ----- (5) NoReplay pre-check -----
    if noreplay::is_marked(
        noreplay_bucket,
        noreplay_authority.address(),
        chain,
        &emitter,
        sequence,
    )? {
        return Err(err(GlobalAccountantError::AlreadyAccounted));
    }

    // ----- (6) NoReplay mark-used CPI -----
    noreplay::mark_used(
        submitter,
        noreplay_bucket,
        noreplay_program,
        noreplay_authority,
        system_program_acc,
        program_id,
        chain,
        &emitter,
        sequence,
    )?;

    // ----- (7) Emit canonical commit log -----
    commit_log::emit(chain, &emitter, sequence, &digest, 0);

    // ----- (8) NTT transfer flow -----
    //
    // Runs after the replay slot is claimed and the breadcrumb laid down. Any
    // error (missing hub, peer mismatch, malformed payload) rolls back with the
    // tx, leaving the slot unconsumed for a future upgrade.
    apply_ntt_transfer(
        program_id,
        submitter,
        transfer_accounts,
        chain,
        &emitter,
        body_bytes,
    )?;

    Ok(())
}
