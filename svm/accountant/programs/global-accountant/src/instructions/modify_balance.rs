//! `modify_balance`: accountant governance. Applies an Add or Subtract delta to a
//! `BalanceAccount` PDA. A per-sequence `Modification` PDA is the replay guard.
//! Accepts target chain `SOLANA_CHAIN_ID` only.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::hash::double_keccak256;
use accountant_operational_core::instructions::{pda_init::init_or_upgrade_pda, shim};
use accountant_operational_core::state::{account as balance_account, modification};
use accountant_operational_core::ProgramResult;

use crate::definitions::{
    BalanceAccountLayout, GlobalAccountantError, ModificationKind, ModifyBalanceLayout, Uint256,
    ACCOUNTANT_GOVERNANCE_MODULE, ACCOUNT_SEED_PREFIX, GOVERNANCE_EMITTER,
    MODIFICATION_SEED_PREFIX, MODIFY_BALANCE_ACTION, SOLANA_CHAIN_ID,
};
use crate::err;

/// Wire format after the 1-byte discriminator:
///
/// | offset | size     | field             |
/// |--------|----------|-------------------|
/// | 0      | 1        | guardian_set_bump |
/// | 1      | 2        | body_len (LE)     |
/// | 3      | body_len | body              |
///
/// `guardian_set_bump` goes to the Shim's `VerifyHash`.
const MODIFY_BALANCE_FIXED_LEN: usize = 1 + 2;

/// Body size cap. A `ModifyBalance` body is 195 bytes.
const MODIFY_BALANCE_BODY_MAX: usize = 256;

/// VAA body header offsets.
const BODY_EMITTER_CHAIN_OFFSET: usize = 8;
const BODY_EMITTER_ADDRESS_OFFSET: usize = 10;
const BODY_HEADER_LEN: usize = 51;

/// Payload offsets from the body start:
///
/// | offset                | size | field          |
/// |-----------------------|------|----------------|
/// | BODY_HEADER_LEN       | 32   | module         |
/// | +32                   | 1    | action         |
/// | +33                   | 2    | target_chain   |
/// | +35                   | 8    | payload_seq    |
/// | +43                   | 2    | chain_id       |
/// | +45                   | 2    | token_chain    |
/// | +47                   | 32   | token_address  |
/// | +79                   | 1    | kind           |
/// | +80                   | 32   | amount         |
/// | +112                  | 32   | reason         |
const PAYLOAD_MODULE_OFFSET: usize = BODY_HEADER_LEN;
const PAYLOAD_ACTION_OFFSET: usize = BODY_HEADER_LEN + 32;
const PAYLOAD_TARGET_CHAIN_OFFSET: usize = BODY_HEADER_LEN + 33;
const PAYLOAD_SEQUENCE_OFFSET: usize = BODY_HEADER_LEN + 35;
const PAYLOAD_CHAIN_ID_OFFSET: usize = BODY_HEADER_LEN + 43;
const PAYLOAD_TOKEN_CHAIN_OFFSET: usize = BODY_HEADER_LEN + 45;
const PAYLOAD_TOKEN_ADDRESS_OFFSET: usize = BODY_HEADER_LEN + 47;
const PAYLOAD_KIND_OFFSET: usize = BODY_HEADER_LEN + 79;
const PAYLOAD_AMOUNT_OFFSET: usize = BODY_HEADER_LEN + 80;
const PAYLOAD_REASON_OFFSET: usize = BODY_HEADER_LEN + 112;
const PAYLOAD_TOTAL_LEN: usize = 32 + 1 + 2 + 8 + 2 + 2 + 32 + 1 + 32 + 32;
const BODY_MIN_LEN: usize = BODY_HEADER_LEN + PAYLOAD_TOTAL_LEN;

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() < MODIFY_BALANCE_FIXED_LEN {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let guardian_set_bump = data[0];
    let body_len = u16::from_le_bytes([data[1], data[2]]) as usize;
    if !(BODY_MIN_LEN..=MODIFY_BALANCE_BODY_MAX).contains(&body_len)
        || data.len() != MODIFY_BALANCE_FIXED_LEN + body_len
    {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let body_bytes = &data[MODIFY_BALANCE_FIXED_LEN..MODIFY_BALANCE_FIXED_LEN + body_len];

    let digest = double_keccak256(body_bytes);

    // Accounts:
    //   0. `[WRITE, SIGNER]` payer
    //   1. `[]`              Verify VAA Shim program
    //   2. `[]`              Core Bridge `GuardianSet` PDA
    //   3. `[]`              `GuardianSignatures` PDA
    //   4. `[WRITE]`         `BalanceAccount` PDA
    //   5. `[]`              system program
    //   6. `[WRITE]`         `Modification` PDA
    let [payer, _verify_vaa_shim_program, guardian_set, guardian_signatures, balance_pda, _system_program_acc, modification_pda] =
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
        &digest,
        guardian_set_bump,
    )?;

    // SECURITY: the emitter must be `(chain=1, GOVERNANCE_EMITTER)`.
    let body_emitter_chain = u16::from_be_bytes([
        body_bytes[BODY_EMITTER_CHAIN_OFFSET],
        body_bytes[BODY_EMITTER_CHAIN_OFFSET + 1],
    ]);
    if body_emitter_chain != SOLANA_CHAIN_ID
        || body_bytes[BODY_EMITTER_ADDRESS_OFFSET..BODY_EMITTER_ADDRESS_OFFSET + 32]
            != GOVERNANCE_EMITTER
    {
        return Err(err(GlobalAccountantError::InvalidGovernanceEmitter));
    }

    if body_bytes[PAYLOAD_MODULE_OFFSET..PAYLOAD_MODULE_OFFSET + 32] != ACCOUNTANT_GOVERNANCE_MODULE
    {
        return Err(err(GlobalAccountantError::InvalidGovernanceModule));
    }
    if body_bytes[PAYLOAD_ACTION_OFFSET] != MODIFY_BALANCE_ACTION {
        return Err(err(GlobalAccountantError::InvalidGovernanceAction));
    }
    let target_chain = u16::from_be_bytes([
        body_bytes[PAYLOAD_TARGET_CHAIN_OFFSET],
        body_bytes[PAYLOAD_TARGET_CHAIN_OFFSET + 1],
    ]);
    if target_chain != SOLANA_CHAIN_ID {
        return Err(err(GlobalAccountantError::GovernanceChainMismatch));
    }
    let kind_byte = body_bytes[PAYLOAD_KIND_OFFSET];
    let kind = ModificationKind::from_u8(kind_byte)
        .ok_or_else(|| err(GlobalAccountantError::InvalidModificationKind))?;

    let payload_sequence = u64::from_be_bytes(
        body_bytes[PAYLOAD_SEQUENCE_OFFSET..PAYLOAD_SEQUENCE_OFFSET + 8]
            .try_into()
            .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?,
    );
    let chain_id = u16::from_be_bytes([
        body_bytes[PAYLOAD_CHAIN_ID_OFFSET],
        body_bytes[PAYLOAD_CHAIN_ID_OFFSET + 1],
    ]);
    let token_chain = u16::from_be_bytes([
        body_bytes[PAYLOAD_TOKEN_CHAIN_OFFSET],
        body_bytes[PAYLOAD_TOKEN_CHAIN_OFFSET + 1],
    ]);
    let mut token_address = [0u8; 32];
    token_address.copy_from_slice(
        &body_bytes[PAYLOAD_TOKEN_ADDRESS_OFFSET..PAYLOAD_TOKEN_ADDRESS_OFFSET + 32],
    );
    let mut amount_bytes = [0u8; 32];
    amount_bytes.copy_from_slice(&body_bytes[PAYLOAD_AMOUNT_OFFSET..PAYLOAD_AMOUNT_OFFSET + 32]);
    let amount = Uint256(amount_bytes);
    let mut reason = [0u8; 32];
    reason.copy_from_slice(&body_bytes[PAYLOAD_REASON_OFFSET..PAYLOAD_REASON_OFFSET + 32]);

    let chain_id_be = chain_id.to_be_bytes();
    let token_chain_be = token_chain.to_be_bytes();
    let (expected_balance_pda, canonical_balance_bump) = Pubkey::find_program_address(
        &[
            ACCOUNT_SEED_PREFIX,
            &chain_id_be,
            &token_chain_be,
            &token_address,
        ],
        program_id,
    );
    if balance_pda.key != &expected_balance_pda {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    let payload_sequence_be = payload_sequence.to_be_bytes();
    let (expected_modification_pda, canonical_modification_bump) = Pubkey::find_program_address(
        &[MODIFICATION_SEED_PREFIX, &payload_sequence_be],
        program_id,
    );
    if modification_pda.key != &expected_modification_pda {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    // Replay guard.
    if modification_pda.owner != &anchor_lang::solana_program::system_program::ID {
        return Err(err(GlobalAccountantError::DuplicateModification));
    }

    // Subtract on an absent PDA fails before allocation; Add creates it with `balance = amount`.
    let balance_is_uninit = balance_pda.owner == &anchor_lang::solana_program::system_program::ID;
    if balance_is_uninit {
        match kind {
            ModificationKind::Subtract => {
                return Err(err(GlobalAccountantError::ModifyBalanceUnderflow));
            }
            ModificationKind::Add => {
                init_balance_account(
                    program_id,
                    payer,
                    balance_pda,
                    canonical_balance_bump,
                    chain_id,
                    token_chain,
                    &token_address,
                    amount,
                )?;
            }
        }
    } else {
        let mut layout = balance_account::load(balance_pda)?;
        match kind {
            ModificationKind::Add => layout.raw_add(amount).map_err(err)?,
            ModificationKind::Subtract => layout.raw_sub(amount).map_err(err)?,
        }
        balance_account::store(balance_pda, &layout)?;
    }

    let bump_seed = [canonical_modification_bump];
    let seeds: &[&[u8]] = &[MODIFICATION_SEED_PREFIX, &payload_sequence_be, &bump_seed];
    init_or_upgrade_pda(
        payer,
        modification_pda,
        program_id,
        seeds,
        ModifyBalanceLayout::LEN as u64,
    )?;

    let mut log: ModifyBalanceLayout = bytemuck::Zeroable::zeroed();
    log.tag = ModifyBalanceLayout::TAG;
    log.sequence = payload_sequence;
    log.chain_id = chain_id;
    log.token_chain = token_chain;
    log.kind = kind_byte;
    log.token_address = token_address;
    log.amount = amount;
    log.reason = reason;
    modification::store(modification_pda, &log)?;

    log_modification(payload_sequence, chain_id, kind_byte, &reason);

    Ok(())
}

/// Create the `BalanceAccount` PDA with `balance = amount`.
#[allow(clippy::too_many_arguments)]
fn init_balance_account<'info>(
    program_id: &Pubkey,
    payer: &AccountInfo<'info>,
    balance_pda: &AccountInfo<'info>,
    canonical_bump: u8,
    chain_id: u16,
    token_chain: u16,
    token_address: &[u8; 32],
    amount: Uint256,
) -> ProgramResult {
    let chain_id_be = chain_id.to_be_bytes();
    let token_chain_be = token_chain.to_be_bytes();
    let bump_seed = [canonical_bump];
    let seeds: &[&[u8]] = &[
        ACCOUNT_SEED_PREFIX,
        &chain_id_be,
        &token_chain_be,
        token_address,
        &bump_seed,
    ];

    init_or_upgrade_pda(
        payer,
        balance_pda,
        program_id,
        seeds,
        BalanceAccountLayout::LEN as u64,
    )?;

    let mut layout: BalanceAccountLayout = bytemuck::Zeroable::zeroed();
    layout.tag = BalanceAccountLayout::TAG;
    layout.chain = chain_id;
    layout.token_chain = token_chain;
    layout.token_address = *token_address;
    layout.balance = amount;
    balance_account::store(balance_pda, &layout)
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
