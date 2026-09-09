//! `submit_vaas`: permissionless signed-VAA path. Checks the VAA through the Shim
//! and applies balances without the quorum tracker. Shares NoReplay state with
//! `submit_observations`, so each `(chain, emitter, sequence)` commits once on either path.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::cpi::{noreplay, shim};
use accountant_operational_core::hash::double_keccak256;
use accountant_operational_core::support::commit_log;
use accountant_operational_core::ProgramResult;

use crate::definitions::{
    parse_vaa_namespace_key, split_body, GlobalAccountantError, SubmitVaasIxData, VaaBodyHeader,
};
use crate::err;
use crate::instructions::transfer;
use accountant_operational_core::accounts::chain_registration;

/// Order: Shim check, NoReplay pre-check, registration check, NoReplay mark,
/// commit log, balance apply.
pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let (ix, body_bytes) = split_body::<SubmitVaasIxData>(data).map_err(err)?;
    if body_bytes.len() <= VaaBodyHeader::LEN {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let guardian_set_bump = ix.guardian_set_bump;

    let digest = double_keccak256(body_bytes);

    // Accounts:
    //   0. `[WRITE, SIGNER]` submitter (rent payer)
    //   1. `[]`              Verify VAA Shim program
    //   2. `[]`              Core Bridge `GuardianSet` PDA
    //   3. `[]`              `GuardianSignatures` PDA
    //   4. `[WRITE]`         NoReplay bitmap PDA
    //   5. `[]`              NoReplay program
    //   6. `[]`              NoReplay authority PDA
    //   7. `[WRITE]`         source-chain balance PDA (any account for non-Transfer payloads)
    //   8. `[WRITE]`         destination-chain balance PDA (as 7)
    //   9. `[]`              system program
    //  10. `[]`              `ChainRegistration` PDA
    let [submitter, _verify_vaa_shim_program, guardian_set, guardian_signatures, noreplay_bucket, noreplay_program, noreplay_authority, source_account_pda, dest_account_pda, _system_program, chain_registration_pda] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !submitter.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    shim::verify_vaa(
        guardian_set,
        guardian_signatures,
        &digest,
        guardian_set_bump,
    )?;

    let header = parse_vaa_namespace_key(body_bytes).map_err(err)?;
    let (chain, emitter, sequence) = (header.chain, header.emitter, header.sequence);

    if noreplay::is_marked(noreplay_bucket, program_id, chain, &emitter, sequence)? {
        return Err(err(GlobalAccountantError::AlreadyAccounted));
    }

    // SECURITY: a signed VAA from an unregistered emitter must not move balances.
    chain_registration::verify(program_id, chain_registration_pda, chain, &emitter)?;

    // Mark before the balance change; a later failure rolls the mark back.
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

    // `guardian_set_index = 0`: the Shim accepts any active set.
    commit_log::emit(chain, &emitter, sequence, &digest, 0);

    transfer::apply_from_body(
        program_id,
        submitter,
        source_account_pda,
        dest_account_pda,
        chain,
        body_bytes,
    )?;

    Ok(())
}
