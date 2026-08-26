//! NoReplay bitmap pre-check (`is_marked`) and `MarkUsed` CPI (`mark_used`).
//!
//! `solana-noreplay` wire format:
//!
//! - Account: 129 bytes, `[bump: u8][bitmap: 128 B]`.
//! - Bit `sequence % 1024` of `bitmap` marks the sequence.
//! - `MarkUsed` data: `[disc=1u8][ns_len: u16 LE][ns][seq: u64 LE]`.
//! - `MarkUsed` accounts: payer (signer, writable), authority (signer, readonly),
//!   bitmap PDA (writable), system program (readonly).
//! - Bitmap PDA seeds: `[authority, ns[..min(len, 32)], ns[min(len, 32)..],
//!   (seq / 1024) LE]`.
//!
//! The authority is this program's PDA at `[NOREPLAY_AUTHORITY_SEED_PREFIX]`.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke_signed;

use crate::definitions::{GlobalAccountantError, NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID};
use crate::{err, ProgramResult};

/// Namespace: `chain_be (2 B) ‖ emitter (32 B)`. NoReplay splits seeds at byte 32.
const NAMESPACE_TOTAL_LEN: usize = 2 + 32;
const NAMESPACE_CHUNK_BOUNDARY: usize = 32;

/// Bitmap PDA `(address, bump)` for `(authority, chain, emitter, sequence)`.
pub fn derive_bucket_pda(
    noreplay_authority: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> (Pubkey, u8) {
    let mut namespace = [0u8; NAMESPACE_TOTAL_LEN];
    namespace[..2].copy_from_slice(&chain.to_be_bytes());
    namespace[2..].copy_from_slice(emitter);
    let bucket_index_bytes = (sequence / NOREPLAY_BITS_PER_BUCKET).to_le_bytes();
    let noreplay_program_id_addr = Pubkey::new_from_array(NOREPLAY_PROGRAM_ID);
    Pubkey::find_program_address(
        &[
            noreplay_authority.as_ref(),
            &namespace[..NAMESPACE_CHUNK_BOUNDARY],
            &namespace[NAMESPACE_CHUNK_BOUNDARY..],
            &bucket_index_bytes,
        ],
        &noreplay_program_id_addr,
    )
}

/// Read the bit for `sequence` from the bitmap PDA.
///
/// - `Ok(false)`: bucket uninitialised or bit clear.
/// - `Ok(true)`: bit set.
/// - `Err(InvalidPda)`: wrong address or malformed data.
///
/// SECURITY: the bucket address is re-derived; a wrong bucket would read another namespace.
pub fn is_marked(
    bucket: &AccountInfo,
    noreplay_authority: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> crate::ProgramCoreResult<bool> {
    let (expected_bucket, _) = derive_bucket_pda(noreplay_authority, chain, emitter, sequence);
    if bucket.key != &expected_bucket {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    if bucket.owner == &anchor_lang::solana_program::system_program::ID {
        return Ok(false);
    }
    let data = bucket.try_borrow_data()?;
    if data.len()
        != crate::definitions::NOREPLAY_BITMAP_OFFSET + crate::definitions::NOREPLAY_BITMAP_BYTES
    {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let bit = (sequence % NOREPLAY_BITS_PER_BUCKET) as usize;
    let byte = data[crate::definitions::NOREPLAY_BITMAP_OFFSET + bit / 8];
    Ok(byte & (1 << (bit % 8)) != 0)
}

/// `MarkUsed` data length: `[disc: u8][ns_len: u16 LE][ns: 34 B][seq: u64 LE]`.
const MARK_USED_DATA_LEN: usize = 1 + 2 + NAMESPACE_TOTAL_LEN + 8;

#[allow(clippy::too_many_arguments)]
pub fn mark_used<'info>(
    payer: &AccountInfo<'info>,
    bucket: &AccountInfo<'info>,
    _noreplay_program: &AccountInfo<'info>,
    noreplay_authority: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    program_id: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> ProgramResult {
    use crate::definitions::{NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_MARK_USED_DISCRIMINATOR};

    // SECURITY: CPI target is the constant `NOREPLAY_PROGRAM_ID`, never `_noreplay_program.key`.
    let noreplay_program_id_addr = Pubkey::new_from_array(NOREPLAY_PROGRAM_ID);
    let (expected_authority, authority_bump) =
        Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], program_id);
    if noreplay_authority.key != &expected_authority {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    let mut namespace = [0u8; NAMESPACE_TOTAL_LEN];
    namespace[..2].copy_from_slice(&chain.to_be_bytes());
    namespace[2..].copy_from_slice(emitter);

    let mut ix_data = [0u8; MARK_USED_DATA_LEN];
    ix_data[0] = NOREPLAY_MARK_USED_DISCRIMINATOR;
    ix_data[1..3].copy_from_slice(&(NAMESPACE_TOTAL_LEN as u16).to_le_bytes());
    ix_data[3..3 + NAMESPACE_TOTAL_LEN].copy_from_slice(&namespace);
    ix_data[3 + NAMESPACE_TOTAL_LEN..].copy_from_slice(&sequence.to_le_bytes());

    let ix_accounts = vec![
        AccountMeta::new(*payer.key, true),
        AccountMeta::new_readonly(*noreplay_authority.key, true),
        AccountMeta::new(*bucket.key, false),
        AccountMeta::new_readonly(*system_program.key, false),
    ];

    let instruction = Instruction {
        program_id: noreplay_program_id_addr,
        accounts: ix_accounts,
        data: ix_data.to_vec(),
    };

    let bump_seed = [authority_bump];
    let signer_seeds: &[&[u8]] = &[NOREPLAY_AUTHORITY_SEED_PREFIX, &bump_seed];

    // An inner-program error aborts this program directly; `Err` here is a pre-CPI failure.
    invoke_signed(
        &instruction,
        &[
            payer.clone(),
            noreplay_authority.clone(),
            bucket.clone(),
            system_program.clone(),
        ],
        &[signer_seeds],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Our seed layout must equal `solana_noreplay::BitmapPdaSeeds` for the same inputs.
    #[test]
    fn bucket_pda_matches_solana_noreplay_seeds() {
        let authority = Pubkey::new_unique();
        let chain: u16 = 2;
        let emitter = [0xABu8; 32];
        let sequence: u64 = 5_000;

        let mut namespace = [0u8; NAMESPACE_TOTAL_LEN];
        namespace[..2].copy_from_slice(&chain.to_be_bytes());
        namespace[2..].copy_from_slice(&emitter);
        let seeds = solana_noreplay::BitmapPdaSeeds::new(&namespace, sequence);
        let expected = Pubkey::find_program_address(
            &seeds.as_seeds(authority.as_ref()),
            &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        );

        assert_eq!(
            derive_bucket_pda(&authority, chain, &emitter, sequence),
            expected
        );
    }
}
