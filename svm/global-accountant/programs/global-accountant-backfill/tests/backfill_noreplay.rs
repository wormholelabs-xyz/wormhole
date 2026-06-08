//! Integration tests for `BackfillNoReplay` driven against the real
//! `solana_noreplay.so` co-deployed in mollusk.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_backfill::{Instruction as IxDiscriminator, BackfillError},
    global_accountant_definitions::{
        NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITMAP_OFFSET, NOREPLAY_BITS_PER_BUCKET,
        NOREPLAY_PROGRAM_ID,
    },
    global_accountant_backfill::state::{BackfillAuthorityLayout, BACKFILL_AUTHORITY_SEED_PREFIX},
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk},
    solana_account::Account,
    solana_instruction::{error::InstructionError, AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

mod common;
use common::{keyed_account_for_noreplay_program, mollusk_with_noreplay};

// ============================================================================
// Fixed test program ID. Distinct from the operational program's [7u8; 32] so
// PDA derivations don't collide across the two test suites.
// ============================================================================
fn program_id() -> Pubkey {
    Pubkey::new_from_array([8u8; 32])
}

fn mollusk() -> Mollusk {
    mollusk_with_noreplay(&program_id())
}

fn system_program_id() -> Pubkey {
    keyed_account_for_system_program().0
}

// ============================================================================
// PDA derivations
// ============================================================================

fn derive_backfill_authority_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[BACKFILL_AUTHORITY_SEED_PREFIX], &program_id())
}

fn derive_noreplay_authority_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id())
}

fn derive_noreplay_bucket(
    authority: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> Pubkey {
    let mut namespace = [0u8; 34];
    namespace[..2].copy_from_slice(&chain.to_be_bytes());
    namespace[2..].copy_from_slice(emitter);
    let bucket_index = (sequence / NOREPLAY_BITS_PER_BUCKET).to_le_bytes();
    let (pda, _) = Pubkey::find_program_address(
        &[
            authority.as_ref(),
            &namespace[..32],
            &namespace[32..],
            &bucket_index,
        ],
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
    );
    pda
}

// ============================================================================
// Account fixtures
// ============================================================================

fn system_owned_account(lamports: u64) -> Account {
    Account {
        lamports,
        data: vec![],
        owner: system_program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn signer_account(lamports: u64) -> Account {
    system_owned_account(lamports)
}

fn uninitialised_pda_account() -> Account {
    system_owned_account(0)
}

// ============================================================================
// Wire format builder
// ============================================================================

/// Wire shape (post-discriminator):
///
/// | offset | size  | field            |
/// |--------|-------|------------------|
/// | 0      | 1     | count (u8)       |
/// | 1+i*74 | 2     | chain (u16 BE)   |
/// | 3+i*74 | 32    | emitter          |
/// | 35+i*74| 8     | sequence (u64 BE)|
/// | 43+i*74| 32    | digest           |
fn build_ix_data(entries: &[(u16, [u8; 32], u64, [u8; 32])]) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 1 + entries.len() * 74);
    data.push(IxDiscriminator::BackfillNoReplay as u8);
    data.push(entries.len() as u8);
    for (chain, emitter, sequence, digest) in entries {
        data.extend_from_slice(&chain.to_be_bytes());
        data.extend_from_slice(emitter);
        data.extend_from_slice(&sequence.to_be_bytes());
        data.extend_from_slice(digest);
    }
    data
}

// ============================================================================
// Tests
// ============================================================================

/// Happy path: a single-entry `BackfillNoReplay` from a fresh state lazy-inits
/// the backfill authority PDA with the signer's pubkey, flips the canonical
/// NoReplay bit, and emits one canonical `ACCDGST\0` commit-log payload
/// indexer-identical to the operational program's emission.
#[test]
fn backfill_noreplay_single_entry_flips_bit_and_emits_canonical_log() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([42u8; 32]);

    let (backfill_auth_pda, _backfill_bump) = derive_backfill_authority_pda();
    let (noreplay_auth_pda, _noreplay_bump) = derive_noreplay_authority_pda();

    let chain = 2u16;
    let emitter = [0x11u8; 32];
    let sequence = 42u64;
    let digest = [0x77u8; 32];

    let bucket = derive_noreplay_bucket(&noreplay_auth_pda, chain, &emitter, sequence);

    let (np_id, np_acc) = keyed_account_for_noreplay_program();
    let (sys_id, sys_acc) = keyed_account_for_system_program();

    let accounts = vec![
        (signer, signer_account(10_000_000_000)),
        (backfill_auth_pda, uninitialised_pda_account()),
        (np_id, np_acc),
        (noreplay_auth_pda, uninitialised_pda_account()),
        (sys_id, sys_acc),
        (bucket, uninitialised_pda_account()),
    ];

    let metas = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new(backfill_auth_pda, false),
        AccountMeta::new_readonly(np_id, false),
        AccountMeta::new_readonly(noreplay_auth_pda, false),
        AccountMeta::new_readonly(sys_id, false),
        AccountMeta::new(bucket, false),
    ];

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_ix_data(&[(chain, emitter, sequence, digest)]),
    };

    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "BackfillNoReplay should succeed; raw_result={:?}",
        result.raw_result
    );

    // Backfill authority PDA must hold the signer's pubkey, retired=0.
    let auth_acc = result
        .resulting_accounts
        .iter()
        .find(|(k, _)| k == &backfill_auth_pda)
        .map(|(_, a)| a)
        .expect("backfill auth PDA in resulting accounts");
    assert_eq!(
        auth_acc.data.len(),
        BackfillAuthorityLayout::LEN,
        "backfill auth PDA size"
    );
    assert_eq!(
        &auth_acc.data[..32],
        signer.as_array(),
        "backfill auth PDA authority field"
    );
    assert_eq!(auth_acc.data[32], 0, "backfill auth PDA not retired");
    assert_eq!(auth_acc.owner, program_id(), "backfill auth PDA owner");

    // NoReplay bucket must be initialised and the canonical bit set. Log
    // emission is asserted in the surfpool e2e test (mollusk does not expose
    // program logs in `InstructionResult`).
    let bucket_acc = result
        .resulting_accounts
        .iter()
        .find(|(k, _)| k == &bucket)
        .map(|(_, a)| a)
        .expect("bucket PDA in resulting accounts");
    let expected_len = NOREPLAY_BITMAP_OFFSET + global_accountant_definitions::NOREPLAY_BITMAP_BYTES;
    assert_eq!(bucket_acc.data.len(), expected_len, "bucket PDA size");
    let bit = (sequence % NOREPLAY_BITS_PER_BUCKET) as usize;
    let byte_idx = NOREPLAY_BITMAP_OFFSET + bit / 8;
    let bitmask = 1u8 << (bit % 8);
    assert_eq!(
        bucket_acc.data[byte_idx] & bitmask,
        bitmask,
        "expected bit {bit} set in noreplay bitmap byte {byte_idx}"
    );
}

/// `BackfillError::InvalidInstructionData` if no entries are encoded.
#[test]
fn backfill_noreplay_zero_entries_rejects() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([42u8; 32]);
    let (backfill_auth_pda, _) = derive_backfill_authority_pda();
    let (noreplay_auth_pda, _) = derive_noreplay_authority_pda();

    let (np_id, np_acc) = keyed_account_for_noreplay_program();
    let (sys_id, sys_acc) = keyed_account_for_system_program();

    let accounts = vec![
        (signer, signer_account(10_000_000_000)),
        (backfill_auth_pda, uninitialised_pda_account()),
        (np_id, np_acc),
        (noreplay_auth_pda, uninitialised_pda_account()),
        (sys_id, sys_acc),
    ];
    let metas = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new(backfill_auth_pda, false),
        AccountMeta::new_readonly(np_id, false),
        AccountMeta::new_readonly(noreplay_auth_pda, false),
        AccountMeta::new_readonly(sys_id, false),
    ];

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_ix_data(&[]),
    };

    let result = mollusk.process_instruction(&ix, &accounts);
    // `raw_result` carries the `InstructionError`; mollusk only converts
    // `Custom(u32)` to `ProgramError::Custom` when the variant fits. Either is
    // fine to match on — `raw_result` keeps us off the `solana-program-error`
    // dev-dep surface.
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code)) if *code == BackfillError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData on empty entry list, got {:?}",
        result.raw_result
    );
}
