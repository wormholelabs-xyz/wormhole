//! NoReplay bitmap pre-check (`is_marked`) and `MarkUsed` CPI (`mark_used`).
//! Wire layouts: [`NoReplayNamespace`], [`NoReplayMarkUsedData`], [`NoReplayBitmapAccount`].
//! The authority is this program's PDA at `[NOREPLAY_AUTHORITY_SEED_PREFIX]`.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke_signed;
use anchor_lang::solana_program::system_program;

use crate::definitions::{
    GlobalAccountantError, NoReplayBitmapAccount, NoReplayMarkUsedData, NoReplayNamespace,
    NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_PROGRAM_ID,
};
use crate::{err, ProgramCoreResult, ProgramResult};

/// Bitmap PDA `(address, bump)` for `(authority, chain, emitter, sequence)`.
pub fn derive_bucket_pda(
    noreplay_authority: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> (Pubkey, u8) {
    let namespace = NoReplayNamespace::new(chain, *emitter);
    let (seed_a, seed_b) = namespace.seed_chunks();
    let bucket_index = NoReplayBitmapAccount::bucket_index(sequence).to_le_bytes();
    Pubkey::find_program_address(
        &[noreplay_authority.as_ref(), seed_a, seed_b, &bucket_index],
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
    )
}

/// This program's NoReplay authority PDA `(address, bump)`.
pub fn derive_authority(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], program_id)
}

/// Read the bit for `sequence` from the bitmap PDA.
///
/// - `Ok(false)`: bucket uninitialised or bit clear.
/// - `Ok(true)`: bit set.
/// - `Err(InvalidPda)`: wrong address or malformed data.
///
/// SECURITY: the bucket address is re-derived from `program_id`, never from a
/// caller-supplied authority key. A bucket under any other authority is `InvalidPda`.
pub fn is_marked(
    bucket: &AccountInfo,
    program_id: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> ProgramCoreResult<bool> {
    let (noreplay_authority, _) = derive_authority(program_id);
    let (expected_bucket, _) = derive_bucket_pda(&noreplay_authority, chain, emitter, sequence);
    if bucket.key != &expected_bucket {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    if bucket.owner == &system_program::ID {
        return Ok(false);
    }
    let data = bucket.try_borrow_data()?;
    let account = NoReplayBitmapAccount::from_bytes(&data)
        .ok_or_else(|| err(GlobalAccountantError::InvalidPda))?;
    Ok(account.is_marked(sequence))
}

/// Check `noreplay_authority` is this program's authority PDA; returns its bump.
fn verify_authority(
    program_id: &Pubkey,
    noreplay_authority: &AccountInfo,
) -> ProgramCoreResult<u8> {
    let (expected, bump) = derive_authority(program_id);
    if noreplay_authority.key != &expected {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(bump)
}

/// `MarkUsed` instruction. Accounts: payer (signer, writable), authority (signer),
/// bitmap PDA (writable), system program.
///
/// SECURITY: the CPI target is the constant `NOREPLAY_PROGRAM_ID`, never a caller-supplied
/// account.
fn mark_used_instruction(
    payer: &Pubkey,
    noreplay_authority: &Pubkey,
    bucket: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> Instruction {
    let data = NoReplayMarkUsedData::new(NoReplayNamespace::new(chain, *emitter), sequence);
    Instruction {
        program_id: Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(*noreplay_authority, true),
            AccountMeta::new(*bucket, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: data.as_bytes().to_vec(),
    }
}

/// Mark `(chain, emitter, sequence)` used via CPI, signed by the authority PDA.
///
/// An inner-program error aborts this program directly; `Err` here is a pre-CPI failure.
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
    let authority_bump = verify_authority(program_id, noreplay_authority)?;
    let instruction = mark_used_instruction(
        payer.key,
        noreplay_authority.key,
        bucket.key,
        chain,
        emitter,
        sequence,
    );
    // `invoke_signed` takes owned `AccountInfo`s; the clone is `Rc` refcount bumps, no data copy.
    invoke_signed(
        &instruction,
        &[
            payer.clone(),
            noreplay_authority.clone(),
            bucket.clone(),
            system_program.clone(),
        ],
        &[&[NOREPLAY_AUTHORITY_SEED_PREFIX, &[authority_bump]]],
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

        let namespace = NoReplayNamespace::new(chain, emitter);
        let seeds = solana_noreplay::BitmapPdaSeeds::new(namespace.as_bytes(), sequence);
        let expected = Pubkey::find_program_address(
            &seeds.as_seeds(authority.as_ref()),
            &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        );

        assert_eq!(
            derive_bucket_pda(&authority, chain, &emitter, sequence),
            expected
        );
    }

    #[test]
    fn mark_used_instruction_shape() {
        let payer = Pubkey::new_unique();
        let authority = Pubkey::new_unique();
        let bucket = Pubkey::new_unique();
        let ix = mark_used_instruction(&payer, &authority, &bucket, 2, &[0xAB; 32], 5_000);
        assert_eq!(ix.program_id, Pubkey::new_from_array(NOREPLAY_PROGRAM_ID));
        assert_eq!(ix.data.len(), NoReplayMarkUsedData::LEN);
        let metas: Vec<(Pubkey, bool, bool)> = ix
            .accounts
            .iter()
            .map(|m| (m.pubkey, m.is_signer, m.is_writable))
            .collect();
        assert_eq!(
            metas,
            vec![
                (payer, true, true),
                (authority, true, false),
                (bucket, false, true),
                (system_program::ID, false, false),
            ]
        );
    }
}
