//! `modify_balance`: accountant governance. Applies an Add or Subtract delta to a
//! `BalanceAccount` PDA. A per-sequence `Modification` PDA is the replay guard.
//! Accepts target chain `SOLANA_CHAIN_ID` only.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;
use anchor_lang::solana_program::system_program;

use accountant_operational_core::hash::double_keccak256;
use accountant_operational_core::instructions::{pda_init::init_or_upgrade_pda, shim};
use accountant_operational_core::state::{account as balance_account, modification};
use accountant_operational_core::{ProgramCoreResult, ProgramResult};

use crate::definitions::{
    split_body, BalanceAccountLayout, GlobalAccountantError, ModificationKind, ModifyBalanceIxData,
    ModifyBalanceLayout, ModifyBalancePayload, VaaBodyHeader, MODIFICATION_SEED_PREFIX,
};
use crate::err;
use crate::instructions::transfer::derive_balance_account_pda;

/// A `ModifyBalance` body is exactly header + payload.
const MODIFY_BALANCE_BODY_LEN: usize = VaaBodyHeader::LEN + ModifyBalancePayload::LEN;

/// Order: instruction framing, signer, Shim signature check, governance validation,
/// PDA checks, replay guard, balance delta, modification record.
pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let (ix, body) = parse_instruction(data)?;

    // Accounts:
    //   0. `[WRITE, SIGNER]` payer
    //   1. `[]`              Verify VAA Shim program
    //   2. `[]`              Core Bridge `GuardianSet` PDA
    //   3. `[]`              `GuardianSignatures` PDA
    //   4. `[WRITE]`         `BalanceAccount` PDA
    //   5. `[]`              system program
    //   6. `[WRITE]`         `Modification` PDA
    let [payer, _verify_vaa_shim_program, guardian_set, guardian_signatures, balance_pda, _system_program, modification_pda] =
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

    let (header, payload) = ModifyBalancePayload::from_body(body).map_err(err)?;
    let kind = payload.validate(header).map_err(err)?;

    let balance_bump = check_balance_pda(program_id, balance_pda, payload)?;
    let modification_bump =
        check_modification_pda(program_id, modification_pda, payload.sequence())?;

    apply_delta(program_id, payer, balance_pda, balance_bump, payload, kind)?;
    record_modification(
        program_id,
        payer,
        modification_pda,
        modification_bump,
        payload,
        kind,
    )?;

    log_modification(
        payload.sequence(),
        payload.chain_id(),
        kind as u8,
        &payload.reason,
    );
    Ok(())
}

/// Instruction data: [`ModifyBalanceIxData`] prefix then an exact-length body.
fn parse_instruction(data: &[u8]) -> ProgramCoreResult<(&ModifyBalanceIxData, &[u8])> {
    let (ix, body) = split_body::<ModifyBalanceIxData>(data).map_err(err)?;
    if body.len() != MODIFY_BALANCE_BODY_LEN {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    Ok((ix, body))
}

/// `balance_pda` must be the canonical account for the payload's token; returns its bump.
fn check_balance_pda(
    program_id: &Pubkey,
    balance_pda: &AccountInfo,
    payload: &ModifyBalancePayload,
) -> ProgramCoreResult<u8> {
    let (expected, bump) = derive_balance_account_pda(
        program_id,
        payload.chain_id(),
        payload.token_chain(),
        &payload.token_address,
    );
    if balance_pda.key != &expected {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(bump)
}

/// `modification_pda` must be the canonical account for `sequence` and must not exist yet
/// (replay guard); returns its bump.
fn check_modification_pda(
    program_id: &Pubkey,
    modification_pda: &AccountInfo,
    sequence: u64,
) -> ProgramCoreResult<u8> {
    let (expected, bump) = derive_modification_pda(program_id, sequence);
    if modification_pda.key != &expected {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    if modification_pda.owner != &system_program::ID {
        return Err(err(GlobalAccountantError::DuplicateModification));
    }
    Ok(bump)
}

/// `(b"modification", sequence_be)`.
pub fn derive_modification_pda(program_id: &Pubkey, sequence: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[MODIFICATION_SEED_PREFIX, &sequence.to_be_bytes()],
        program_id,
    )
}

/// Apply `kind` with `payload.amount()`. Add on an absent PDA creates it with
/// `balance = amount`; Subtract on an absent PDA is an underflow.
fn apply_delta<'info>(
    program_id: &Pubkey,
    payer: &AccountInfo<'info>,
    balance_pda: &AccountInfo<'info>,
    balance_bump: u8,
    payload: &ModifyBalancePayload,
    kind: ModificationKind,
) -> ProgramResult {
    let amount = payload.amount();
    // Not initialized branch
    if balance_pda.owner == &system_program::ID {
        return match kind {
            ModificationKind::Subtract => Err(err(GlobalAccountantError::ModifyBalanceUnderflow)),
            ModificationKind::Add => balance_account::create(
                program_id,
                payer,
                balance_pda,
                balance_bump,
                &BalanceAccountLayout::new(
                    payload.chain_id(),
                    payload.token_chain(),
                    payload.token_address,
                    amount,
                ),
            ),
        };
    }
    let mut layout = balance_account::load(balance_pda)?;
    match kind {
        ModificationKind::Add => layout.raw_add(amount).map_err(err)?,
        ModificationKind::Subtract => layout.raw_sub(amount).map_err(err)?,
    }
    balance_account::store(balance_pda, &layout)
}

/// Create the `Modification` PDA and write the record.
fn record_modification<'info>(
    program_id: &Pubkey,
    payer: &AccountInfo<'info>,
    modification_pda: &AccountInfo<'info>,
    modification_bump: u8,
    payload: &ModifyBalancePayload,
    kind: ModificationKind,
) -> ProgramResult {
    let bump_seed = [modification_bump];
    let seeds: &[&[u8]] = &[MODIFICATION_SEED_PREFIX, &payload.sequence, &bump_seed]; // sequence BE
    init_or_upgrade_pda(
        payer,
        modification_pda,
        program_id,
        seeds,
        ModifyBalanceLayout::LEN as u64,
    )?;

    let record = ModifyBalanceLayout::new(
        kind,
        payload.chain_id(),
        payload.token_chain(),
        payload.sequence(),
        payload.token_address,
        payload.amount(),
        payload.reason,
    );
    modification::store(modification_pda, &record)
}

/// Log the modification for off-chain indexers.
fn log_modification(sequence: u64, chain_id: u16, kind: u8, reason: &[u8; 32]) {
    msg!(
        "modification sequence={} chain_id={} kind={}",
        sequence,
        chain_id,
        kind
    );
    anchor_lang::solana_program::log::sol_log_data(&[reason]);
}
