//! `submit_vaas` — permissionless signed-VAA backfill.
//!
//! Consumes a fully-signed VAA via the Verify VAA Shim CPI and applies its
//! balance effects directly, bypassing the quorum tracker. Escape hatch for
//! stuck pending buckets and migration backfill.
//!
//! Shares only NoReplay state with `submit_observations`: once `(chain, emitter,
//! seq)` is marked, any later caller on either path is rejected as
//! `AlreadyAccounted`. Attest payloads no-op the balance work but still commit
//! the DigestAccount and flip NoReplay; unknown actions are rejected, leaving
//! the slot unconsumed for a future upgrade.

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

use crate::definitions::{
    parse_token_bridge_payload, parse_vaa_namespace_key, GlobalAccountantError, TokenBridgeAction,
    VAA_BODY_HEADER_LEN,
};
use crate::err;
use crate::instructions::{commit_log, noreplay, shim, transfer::apply_transfer};
use crate::state::chain_registration;

/// Wire format for the `submit_vaas` instruction data (after the 1-byte
/// dispatch discriminator):
///
/// | offset | size     | field             |
/// |--------|----------|-------------------|
/// | 0      | 1        | guardian_set_bump |
/// | 1      | 2        | body_len (LE)     |
/// | 3      | body_len | body              |
///
/// `guardian_set_bump` is passed to the Shim's `VerifyHash`. `body_len` is
/// bounded only by the `u16` width; the transports are far tighter.
const SUBMIT_VAAS_FIXED_LEN: usize = 1 + 2;

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

    // Accounts:
    //   0. `[WRITE, SIGNER]` submitter — fee / rent payer for lazy PDAs.
    //   1. `[]`              Verify VAA Shim program (CPI target).
    //   2. `[]`              Core Bridge `GuardianSet` PDA.
    //   3. `[]`              `GuardianSignatures` PDA (posted via the Shim's `PostSignatures`).
    //   4. `[WRITE]`         NoReplay bitmap PDA — pre-check then CPI on commit.
    //   5. `[]`              NoReplay program (CPI target).
    //   6. `[]`              NoReplay authority PDA owned by this program.
    //   7. `[WRITE]`         source-chain Account PDA. Untouched for non-Transfer
    //                       payloads; sentinel acceptable.
    //   8. `[WRITE]`         dest-chain Account PDA. Same semantics as slot 7.
    //   9. `[]`              system program.
    //  10. `[]`              Chain registration PDA — cross-check the body's
    //                       `(emitter_chain, emitter_address)`.
    //
    // The canonical digest record is emitted via `sol_log_data` rather than
    // stored in a PDA; off-chain indexers consume the program-log line carrying
    // the `ACCOUNTANT_DIGEST_LOG_TAG` prefix.
    let [submitter, _verify_vaa_shim_program, guardian_set, guardian_signatures, noreplay_bucket, noreplay_program, noreplay_authority, source_account_pda, dest_account_pda, _system_program, chain_registration_pda] =
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
    //
    // Reject any `(chain, emitter, seq)` already accounted via either path.
    if noreplay::is_marked(
        noreplay_bucket,
        noreplay_authority.address(),
        chain,
        &emitter,
        sequence,
    )? {
        return Err(err(GlobalAccountantError::AlreadyAccounted));
    }

    // ----- (5b) Chain registration cross-check -----
    //
    // The body's emitter must be registered; otherwise valid sigs for a
    // non-Token-Bridge VAA could route accounting against a fake emitter.
    chain_registration::verify(program_id, chain_registration_pda, chain, &emitter)?;

    // ----- (6) NoReplay mark-used CPI -----
    //
    // Burn the slot before any balance change; tx-level rollback covers later
    // failures. A racing tx surfaces as the inner noreplay program's
    // `AccountAlreadyInitialized` — the SBF runtime propagates it directly
    // (see `noreplay::mark_used` comment).
    noreplay::mark_used(
        submitter,
        noreplay_bucket,
        noreplay_program,
        noreplay_authority,
        _system_program,
        program_id,
        chain,
        &emitter,
        sequence,
    )?;

    // ----- (7) Emit canonical commit log -----
    //
    // Same breadcrumb the quorum path leaves, now via `sol_log_data` rather
    // than a PDA. `guardian_set_index = 0` is a sentinel — `submit_vaas` does
    // not pin a single set (the Shim accepts any currently-active one).
    commit_log::emit(chain, &emitter, sequence, &digest, 0);

    // ----- (8) Parse Token Bridge payload + apply balance work -----
    //
    // Runs after the replay slot is claimed and the breadcrumb laid down.
    // Transfer mutates the Account PDAs; Attest no-ops; unknown actions reject
    // (tx rollback leaves the slot unconsumed for a future upgrade).
    match parse_token_bridge_payload(body_bytes).map_err(err)? {
        TokenBridgeAction::Transfer {
            amount,
            token_chain,
            token_address,
            recipient_chain,
        } => {
            apply_transfer(
                program_id,
                submitter,
                source_account_pda,
                dest_account_pda,
                chain,
                recipient_chain,
                token_chain,
                &token_address,
                amount,
            )?;
        }
        TokenBridgeAction::Attest => {
            // No balance work; slots 8 and 9 are untouched (sentinels OK).
        }
        TokenBridgeAction::Other => {
            return Err(err(GlobalAccountantError::UnknownTokenBridgePayload));
        }
    }

    Ok(())
}

use crate::hash::double_keccak256;
