//! `BackfillRelayerRegistration` — write `RelayerChainRegistrationLayout` PDAs
//! from wormchain `relayer_chain_registrations` rows. NTT's analogue of the
//! Token Bridge chain registration; same shape as `backfill_balance`.

use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    AccountView, Address, ProgramResult,
};

use accountant_backfill_core::authority::require_authority;
use accountant_backfill_core::pda_init::init_or_upgrade_pda;
use accountant_backfill_core::{err, BackfillError};
use global_accountant_definitions::{
    Pubkey, RelayerChainRegistrationLayout, RELAYER_CHAIN_REGISTRATION_SEED_PREFIX,
};

/// Wire format (after the 1-byte dispatch discriminator):
///
/// | offset  | size | field           |
/// |---------|------|-----------------|
/// | 0       | 1    | count           |
/// | 1+i*34  | 2    | chain (BE)      |
/// | 3+i*34  | 32   | emitter_address |
///
/// Entries MUST be strictly ascending by `chain`.
const ENTRY_BYTES: usize = 2 + 32;
const FIXED_HEAD: usize = 1;

pub fn process(
    program_id: &Address,
    accounts: &mut [AccountView],
    data: &[u8],
    authority: &Pubkey,
) -> ProgramResult {
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

    //   0. [WRITE, SIGNER] payer — must equal `BACKFILL_AUTHORITY`
    //   1. [ ]             system program (consumed by `init_or_upgrade_pda`)
    //   2..2+count.        relayer-registration PDAs at canonical seeds, in order
    let [payer, _system_program, pdas @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if pdas.len() != count {
        return Err(err(BackfillError::InvalidInstructionData));
    }

    require_authority(payer, authority)?;
    let payer: &AccountView = payer;

    let mut prev_chain: Option<u16> = None;
    for (i, pda) in pdas.iter_mut().enumerate() {
        let off = FIXED_HEAD + i * ENTRY_BYTES;
        let chain = u16::from_be_bytes([data[off], data[off + 1]]);
        let mut emitter_address = [0u8; 32];
        emitter_address.copy_from_slice(&data[off + 2..off + 34]);

        // Strict-ascending order also forbids duplicate chains.
        if let Some(prev) = prev_chain {
            if chain <= prev {
                return Err(err(BackfillError::InvalidInstructionData));
            }
        }
        prev_chain = Some(chain);

        let chain_be = chain.to_be_bytes();
        let (expected, canonical_bump) = Address::find_program_address(
            &[RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, chain_be.as_slice()],
            program_id,
        );
        if pda.address() != &expected {
            return Err(err(BackfillError::InvalidPda));
        }

        let bump_seed = [canonical_bump];
        let seeds = [
            Seed::from(RELAYER_CHAIN_REGISTRATION_SEED_PREFIX),
            Seed::from(chain_be.as_slice()),
            Seed::from(bump_seed.as_slice()),
        ];
        let signer = Signer::from(&seeds);

        init_or_upgrade_pda(
            payer,
            pda,
            program_id,
            signer,
            RelayerChainRegistrationLayout::LEN as u64,
        )?;

        let mut layout: RelayerChainRegistrationLayout = bytemuck::Zeroable::zeroed();
        layout.tag = RelayerChainRegistrationLayout::TAG;
        layout.chain = chain;
        layout.emitter_address = emitter_address;
        // `_pad0` / `_padding` left zero — crate-private to definitions.

        let mut data_mut = pda.try_borrow_mut()?;
        if data_mut.len() != RelayerChainRegistrationLayout::LEN {
            return Err(err(BackfillError::InvalidPda));
        }
        data_mut.copy_from_slice(bytemuck::bytes_of(&layout));
    }

    Ok(())
}
