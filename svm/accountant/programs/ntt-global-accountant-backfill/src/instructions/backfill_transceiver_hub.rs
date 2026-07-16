//! `BackfillTransceiverHub` — write `TransceiverHubLayout` PDAs from wormchain
//! `transceiver_to_hub` rows. Keyed by `(emitter_chain, transceiver_address)`,
//! storing the `(hub_chain, hub_address)` the transceiver belongs to.

use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    AccountView, Address, ProgramResult,
};

use accountant_backfill_core::instructions::authority::require_authority;
use accountant_backfill_core::instructions::pda_init::init_or_upgrade_pda;
use accountant_backfill_core::{err, BackfillError};
use global_accountant_definitions::{Pubkey, TransceiverHubLayout, TRANSCEIVER_HUB_SEED_PREFIX};

/// Wire format (after the 1-byte dispatch discriminator):
///
/// | offset  | size | field        |
/// |---------|------|--------------|
/// | 0       | 1    | count        |
/// | 1+i*68  | 2    | chain (BE)   |
/// | 3+i*68  | 32   | address      |
/// | 35+i*68 | 2    | hub_chain (BE) |
/// | 37+i*68 | 32   | hub_address  |
///
/// Entries MUST be strictly ascending by `(chain, address)`.
const ENTRY_BYTES: usize = 2 + 32 + 2 + 32;
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
    //   2..2+count.        transceiver-hub PDAs at canonical seeds, in order
    let [payer, _system_program, pdas @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if pdas.len() != count {
        return Err(err(BackfillError::InvalidInstructionData));
    }

    require_authority(payer, authority)?;
    let payer: &AccountView = payer;

    let mut prev_key: Option<(u16, [u8; 32])> = None;
    for (i, pda) in pdas.iter_mut().enumerate() {
        let off = FIXED_HEAD + i * ENTRY_BYTES;
        let chain = u16::from_be_bytes([data[off], data[off + 1]]);
        let mut address = [0u8; 32];
        address.copy_from_slice(&data[off + 2..off + 34]);
        let hub_chain = u16::from_be_bytes([data[off + 34], data[off + 35]]);
        let mut hub_address = [0u8; 32];
        hub_address.copy_from_slice(&data[off + 36..off + 68]);

        let cur_key = (chain, address);
        if let Some(prev) = prev_key {
            if cur_key <= prev {
                return Err(err(BackfillError::InvalidInstructionData));
            }
        }
        prev_key = Some(cur_key);

        let chain_be = chain.to_be_bytes();
        let (expected, canonical_bump) = Address::find_program_address(
            &[
                TRANSCEIVER_HUB_SEED_PREFIX,
                chain_be.as_slice(),
                address.as_slice(),
            ],
            program_id,
        );
        if pda.address() != &expected {
            return Err(err(BackfillError::InvalidPda));
        }

        let bump_seed = [canonical_bump];
        let seeds = [
            Seed::from(TRANSCEIVER_HUB_SEED_PREFIX),
            Seed::from(chain_be.as_slice()),
            Seed::from(address.as_slice()),
            Seed::from(bump_seed.as_slice()),
        ];
        let signer = Signer::from(&seeds);

        init_or_upgrade_pda(
            payer,
            pda,
            program_id,
            signer,
            TransceiverHubLayout::LEN as u64,
        )?;

        let mut layout: TransceiverHubLayout = bytemuck::Zeroable::zeroed();
        layout.tag = TransceiverHubLayout::TAG;
        layout.chain = chain;
        layout.hub_chain = hub_chain;
        layout.address = address;
        layout.hub_address = hub_address;
        // `_pad0` left zero — crate-private to definitions.

        let mut data_mut = pda.try_borrow_mut()?;
        if data_mut.len() != TransceiverHubLayout::LEN {
            return Err(err(BackfillError::InvalidPda));
        }
        data_mut.copy_from_slice(bytemuck::bytes_of(&layout));
    }

    Ok(())
}
