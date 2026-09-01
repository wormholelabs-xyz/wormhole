//! `BackfillBalance` — write `BalanceAccountLayout` PDAs directly from a
//! wormchain `query_all_accounts` row.
//!
//! Bulk-batched like `BackfillNoReplay`: caller pre-sorts entries strictly
//! ascending by `(chain, token_chain, token_address)`; handler verifies the
//! sort, allocates each PDA, writes the layout.
//!
//! Audit chain is transitive: the wormchain snapshot is the source of
//! truth for balances, and every transfer VAA landing on the operational
//! program post-upgrade is independently log-audited via
//! `BackfillNoReplay`'s `ACCDGST\0` entries plus the operational program's
//! own emissions.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use crate::definitions::{BalanceAccountLayout, Uint256, ACCOUNT_SEED_PREFIX};
use crate::instructions::{authority, pda_init::init_or_upgrade_pda};
use crate::{err, BackfillError, ProgramResult};

/// Wire format (after the 1-byte dispatch discriminator):
///
/// | offset  | size  | field         |
/// |---------|-------|---------------|
/// | 0       | 1     | count         |
/// | 1+i*68  | 2     | chain (BE)    |
/// | 3+i*68  | 2     | token_chain (BE) |
/// | 5+i*68  | 32    | token_address |
/// | 37+i*68 | 32    | balance (BE)  |
///
/// Entries must be strictly ascending by `(chain, token_chain, token_address)`.
const ENTRY_BYTES: usize = 2 + 2 + 32 + 32;
const FIXED_HEAD: usize = 1;

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() < FIXED_HEAD {
        return Err(err(BackfillError::InvalidInstructionData));
    }
    let count = data[0] as usize;
    if count == 0 {
        return Err(err(BackfillError::InvalidInstructionData));
    }
    let expected_len = FIXED_HEAD + count * ENTRY_BYTES;
    if data.len() != expected_len {
        return Err(err(BackfillError::InvalidInstructionData));
    }

    // Accounts: [WRITE, SIGNER] payer, [] system program (required for
    // `init_or_upgrade_pda`'s CPI), then one balance PDA per entry in order.
    let [payer, _system_program, balance_pdas @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if balance_pdas.len() != count {
        return Err(err(BackfillError::InvalidInstructionData));
    }

    authority::require_authority(payer)?;

    let mut prev_key: Option<(u16, u16, [u8; 32])> = None;
    for (i, balance_pda) in balance_pdas.iter().enumerate() {
        let off = FIXED_HEAD + i * ENTRY_BYTES;
        let chain = u16::from_be_bytes([data[off], data[off + 1]]);
        let token_chain = u16::from_be_bytes([data[off + 2], data[off + 3]]);
        let mut token_address = [0u8; 32];
        token_address.copy_from_slice(&data[off + 4..off + 36]);
        let mut balance_bytes = [0u8; 32];
        balance_bytes.copy_from_slice(&data[off + 36..off + 68]);

        let cur_key = (chain, token_chain, token_address);
        if let Some(prev) = prev_key {
            if cur_key <= prev {
                return Err(err(BackfillError::InvalidInstructionData));
            }
        }
        prev_key = Some(cur_key);

        // Derive + verify canonical PDA.
        let chain_be = chain.to_be_bytes();
        let token_chain_be = token_chain.to_be_bytes();
        // `find_program_address` over `create_program_address`+bump: authority
        // is fully trusted, so the extra cost isn't worth the complexity.
        let (expected, canonical_bump) = Pubkey::find_program_address(
            &[
                ACCOUNT_SEED_PREFIX,
                chain_be.as_slice(),
                token_chain_be.as_slice(),
                token_address.as_slice(),
            ],
            program_id,
        );
        if balance_pda.key != &expected {
            return Err(err(BackfillError::InvalidPda));
        }

        // Allocate the PDA at canonical seeds and write the layout. The
        // operational program (which replaces this .so via `program upgrade`)
        // inherits these accounts by program-ID identity.
        let bump_seed = [canonical_bump];
        let seeds: &[&[u8]] = &[
            ACCOUNT_SEED_PREFIX,
            chain_be.as_slice(),
            token_chain_be.as_slice(),
            token_address.as_slice(),
            &bump_seed,
        ];

        init_or_upgrade_pda(
            payer,
            balance_pda,
            program_id,
            seeds,
            BalanceAccountLayout::LEN as u64,
        )?;

        let layout = BalanceAccountLayout::new(
            chain,
            token_chain,
            token_address,
            Uint256::from_be_bytes(balance_bytes),
        );

        let mut data_mut = balance_pda.try_borrow_mut_data()?;
        if data_mut.len() != BalanceAccountLayout::LEN {
            return Err(err(BackfillError::InvalidPda));
        }
        data_mut.copy_from_slice(bytemuck::bytes_of(&layout));
    }

    Ok(())
}
