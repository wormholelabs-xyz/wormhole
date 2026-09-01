//! `upgrade_contract`: accountant governance. Replaces this program's code with a
//! prepared buffer through the BPF upgradeable loader. NoReplay blocks replay.
//! Accepts target chain `SOLANA_CHAIN_ID` only.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::cpi::{loader, noreplay, shim};
use accountant_operational_core::hash::double_keccak256;
use accountant_operational_core::{ProgramCoreResult, ProgramResult};

use crate::definitions::{
    split_body, GlobalAccountantError, UpgradeContractIxData, UpgradeContractPayload,
    VaaBodyHeader, GOVERNANCE_EMITTER, SOLANA_CHAIN_ID,
};
use crate::err;

/// An `UpgradeContract` body is exactly header + payload.
const UPGRADE_CONTRACT_BODY_LEN: usize = VaaBodyHeader::LEN + UpgradeContractPayload::LEN;

/// Order: instruction framing, signer, Shim signature check, governance validation,
/// NoReplay pre-check, loader upgrade CPI, NoReplay mark.
pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let (ix, body) = parse_instruction(data)?;

    // Accounts:
    //   0. `[WRITE, SIGNER]` payer
    //   1. `[]`              Verify VAA Shim program
    //   2. `[]`              Core Bridge `GuardianSet` PDA
    //   3. `[]`              `GuardianSignatures` PDA
    //   4. `[WRITE]`         NoReplay bitmap PDA
    //   5. `[]`              NoReplay program
    //   6. `[]`              NoReplay authority PDA
    //   7. `[]`              system program
    //   8. `[]`              upgrade authority PDA
    //   9. `[WRITE]`         spill
    //  10. `[WRITE]`         buffer (the payload's `new_contract`)
    //  11. `[WRITE]`         program-data account
    //  12. `[WRITE]`         this program's account
    //  13. `[]`              rent sysvar
    //  14. `[]`              clock sysvar
    //  15. `[]`              BPF upgradeable loader
    let [payer, _verify_vaa_shim_program, guardian_set, guardian_signatures, noreplay_bucket, noreplay_program, noreplay_authority, system_program_acc, upgrade_authority, spill, buffer, program_data, program_account, rent, clock, _bpf_loader_upgradeable_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    shim::verify_vaa(
        guardian_set,
        guardian_signatures,
        &double_keccak256(body),
        ix.guardian_set_bump,
    )?;

    let (header, payload) = UpgradeContractPayload::from_body(body).map_err(err)?;
    payload.validate(header).map_err(err)?;
    let sequence = header.sequence();

    if noreplay::is_marked(
        noreplay_bucket,
        noreplay_authority.key,
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        sequence,
    )? {
        return Err(err(GlobalAccountantError::AlreadyAccounted));
    }

    loader::upgrade_program(
        program_account,
        program_data,
        buffer,
        spill,
        upgrade_authority,
        rent,
        clock,
        program_id,
        &Pubkey::new_from_array(payload.new_contract),
    )?;

    noreplay::mark_used(
        payer,
        noreplay_bucket,
        noreplay_program,
        noreplay_authority,
        system_program_acc,
        program_id,
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        sequence,
    )?;

    Ok(())
}

/// Instruction data: [`UpgradeContractIxData`] prefix then an exact-length body.
fn parse_instruction(data: &[u8]) -> ProgramCoreResult<(&UpgradeContractIxData, &[u8])> {
    let (ix, body) = split_body::<UpgradeContractIxData>(data).map_err(err)?;
    if body.len() != UPGRADE_CONTRACT_BODY_LEN {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    Ok((ix, body))
}
