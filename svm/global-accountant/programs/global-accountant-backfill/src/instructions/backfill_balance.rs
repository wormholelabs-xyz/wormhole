//! `BackfillBalance` — write `BalanceAccountLayout` PDAs directly from a
//! wormchain `query_all_accounts` row.
//!
//! Bulk-batched same as `BackfillNoReplay`: caller pre-sorts entries strictly
//! ascending by `(chain, token_chain, token_address)`; handler verifies the
//! sort, allocates each PDA, writes the layout.
//!
//! No VAA verification, no commit-log. The audit chain for balances is
//! transitive: the wormchain snapshot is the source of truth (publishable),
//! and every transfer VAA that lands on the operational program after upgrade
//! is independently log-audited via `BackfillNoReplay`'s `ACCDGST\0` entries
//! plus the operational program's emissions.

use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    AccountView, Address, ProgramResult,
};

use crate::definitions::{BalanceAccountLayout, Uint256, ACCOUNT_SEED_PREFIX};
use crate::instructions::{authority, pda_init::init_or_upgrade_pda};
use crate::{err, BackfillError};

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
/// Entries MUST be strictly ascending by `(chain, token_chain, token_address)`.
const ENTRY_BYTES: usize = 2 + 2 + 32 + 32;
const FIXED_HEAD: usize = 1;

pub fn process(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    // ----- (1) Parse + validate wire data -----
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

    // ----- (2) Accounts layout -----
    //
    //   0. [WRITE, SIGNER] payer — must equal `BACKFILL_AUTHORITY`
    //   1. [ ]             system program
    //   2..2+count.        balance PDAs at canonical seeds, in entry order
    // `_system_program` slot is required at the tx wire level for
    // `init_or_upgrade_pda`'s `CreateAccount` CPI; pinocchio finds it via
    // the loader. We don't reference it explicitly in this scope.
    let [payer, _system_program, balance_pdas @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if balance_pdas.len() != count {
        return Err(err(BackfillError::InvalidInstructionData));
    }

    // ----- (3) Authority gate -----
    authority::require_authority(payer)?;
    let payer: &AccountView = payer;

    // ----- (4) Write each entry, verifying sort order and PDA canonicality -----
    let mut prev_key: Option<(u16, u16, [u8; 32])> = None;
    for (i, balance_pda) in balance_pdas.iter_mut().enumerate() {
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
        // Since authority is fully trusted, we could use create_program_address and pass the bump
        // but probably not worth the complexity and minute cost saving
        let (expected, canonical_bump) = Address::find_program_address(
            &[
                ACCOUNT_SEED_PREFIX,
                chain_be.as_slice(),
                token_chain_be.as_slice(),
                token_address.as_slice(),
            ],
            program_id,
        );
        if balance_pda.address() != &expected {
            return Err(err(BackfillError::InvalidPda));
        }

        // Allocate the PDA at canonical seeds and write the layout. The
        // backfill program is the sole writer of these accounts; the operational
        // program (which replaces this .so via `program upgrade`) inherits them
        // by program-ID identity.
        let bump_seed = [canonical_bump];
        let seeds = [
            Seed::from(ACCOUNT_SEED_PREFIX),
            Seed::from(chain_be.as_slice()),
            Seed::from(token_chain_be.as_slice()),
            Seed::from(token_address.as_slice()),
            Seed::from(bump_seed.as_slice()),
        ];
        let signer = Signer::from(&seeds);

        init_or_upgrade_pda(
            payer,
            balance_pda,
            program_id,
            signer,
            BalanceAccountLayout::LEN as u64,
        )?;

        let mut layout: BalanceAccountLayout = bytemuck::Zeroable::zeroed();
        layout.tag = BalanceAccountLayout::TAG;
        layout.chain = chain;
        layout.token_chain = token_chain;
        layout.token_address = token_address;
        layout.balance = Uint256::from_be_bytes(balance_bytes);
        // `_pad0` left zero — the field is crate-private to definitions.

        let mut data_mut = balance_pda.try_borrow_mut()?;
        if data_mut.len() != BalanceAccountLayout::LEN {
            return Err(err(BackfillError::InvalidPda));
        }
        data_mut.copy_from_slice(bytemuck::bytes_of(&layout));
    }

    Ok(())
}
