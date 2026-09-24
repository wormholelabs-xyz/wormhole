//! `modify_balance`: accountant governance for the caller's module. Applies an Add or
//! Subtract delta to a `BalanceAccount` PDA. A per-sequence `ModifyBalance` PDA is the
//! replay guard.
//! Accepts target chain `SOLANA_CHAIN_ID` only.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use crate::accounts::{self, balance as balance_account};
use crate::cpi::shim;
use crate::definitions::{
    split_body, BalanceAccountKey, BalanceAccountLayout, GlobalAccountantError, GovernanceModule,
    ModificationKind, ModifyBalanceIxData, ModifyBalanceKey, ModifyBalanceLayout,
    ModifyBalancePayload, VaaBodyHeader,
};
use crate::hash::double_keccak256;
use crate::support::pda;
use crate::{err, ProgramCoreResult, ProgramResult};

/// A `ModifyBalance` body is exactly header + payload.
const MODIFY_BALANCE_BODY_LEN: usize = VaaBodyHeader::LEN + ModifyBalancePayload::LEN;

/// Order: instruction framing, signer, Shim signature check, governance validation against
/// `module`, PDA checks, replay guard, balance delta, modification record.
pub fn process(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
    module: &GovernanceModule,
) -> ProgramResult {
    let (ix, body) = parse_instruction(data)?;

    // Accounts:
    //   0. `[WRITE, SIGNER]` payer
    //   1. `[]`              Verify VAA Shim program
    //   2. `[]`              Core Bridge `GuardianSet` PDA
    //   3. `[]`              `GuardianSignatures` PDA
    //   4. `[WRITE]`         `BalanceAccount` PDA
    //   5. `[]`              system program
    //   6. `[WRITE]`         `ModifyBalance` PDA
    let [payer, _verify_vaa_shim_program, guardian_set, guardian_signatures, balance_pda, _system_program, modify_balance_pda] =
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
    let kind = payload.validate(header, module).map_err(err)?;

    let balance_bump = check_balance_pda(program_id, balance_pda, payload)?;
    let modification_bump =
        check_modify_balance_pda(program_id, modify_balance_pda, payload.sequence())?;

    apply_delta(program_id, payer, balance_pda, balance_bump, payload, kind)?;
    record_modify_balance(
        program_id,
        payer,
        modify_balance_pda,
        modification_bump,
        payload,
        kind,
    )?;

    log_modify_balance(
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
    let key = BalanceAccountKey::new(
        payload.chain_id(),
        payload.token_chain(),
        payload.token_address,
    );
    pda::check(program_id, balance_pda, &key)
}

/// `modify_balance_pda` must be the canonical account for `sequence` and must not exist yet
/// (replay guard); returns its bump.
fn check_modify_balance_pda(
    program_id: &Pubkey,
    modify_balance_pda: &AccountInfo,
    sequence: u64,
) -> ProgramCoreResult<u8> {
    pda::check_uninitialised(
        program_id,
        modify_balance_pda,
        &ModifyBalanceKey::new(sequence),
        GlobalAccountantError::DuplicateModifyBalance,
    )
}

/// `(b"modify_balance", sequence_be)`.
pub fn derive_modify_balance_pda(program_id: &Pubkey, sequence: u64) -> (Pubkey, u8) {
    pda::derive(program_id, &ModifyBalanceKey::new(sequence))
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
    if !pda::is_initialised(program_id, balance_pda)? {
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
    let mut layout = accounts::load::<BalanceAccountLayout>(balance_pda)?;
    match kind {
        ModificationKind::Add => layout.raw_add(amount).map_err(err)?,
        ModificationKind::Subtract => layout.raw_sub(amount).map_err(err)?,
    }
    accounts::store(balance_pda, &layout)
}

/// Create the `ModifyBalance` PDA and write the record.
fn record_modify_balance<'info>(
    program_id: &Pubkey,
    payer: &AccountInfo<'info>,
    modify_balance_pda: &AccountInfo<'info>,
    modification_bump: u8,
    payload: &ModifyBalancePayload,
    kind: ModificationKind,
) -> ProgramResult {
    let record = ModifyBalanceLayout::new(
        kind,
        payload.chain_id(),
        payload.token_chain(),
        payload.sequence(),
        payload.token_address,
        payload.amount(),
        payload.reason,
    );
    pda::create(
        program_id,
        payer,
        modify_balance_pda,
        &record.key(),
        modification_bump,
        &record,
    )
}

/// Log the modification for off-chain indexers.
fn log_modify_balance(sequence: u64, chain_id: u16, kind: u8, reason: &[u8; 32]) {
    msg!(
        "modification sequence={} chain_id={} kind={}",
        sequence,
        chain_id,
        kind
    );
    anchor_lang::solana_program::log::sol_log_data(&[reason]);
}
