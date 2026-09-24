//! Integration tests for `BackfillChainRegistration` — writes the `ChainRegistration` PDA
//! and the `RegisterChain` record PDA for each wormchain chain registration. Both PDAs are
//! the accounts the operational `register_chain` writes, so `submit_vaas` and
//! `submit_observations` find every registration at cutover.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        BackfillChainRegistrationEntry, ChainRegistrationLayout, GlobalAccountantError,
        RegisterChainLayout, CHAIN_REGISTRATION_SEED_PREFIX, REGISTER_CHAIN_SEED_PREFIX,
    },
    mollusk_svm::{
        program::keyed_account_for_system_program,
        result::{InstructionResult, ProgramResult},
    },
    solana_account::Account,
    solana_instruction::{error::InstructionError, AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

use crate::common::*;

const ETHEREUM: u16 = 2;
const BSC: u16 = 4;
const POLYGON: u16 = 5;

fn derive_registration_pda(chain: u16) -> Pubkey {
    let (pda, _) = Pubkey::find_program_address(
        &[CHAIN_REGISTRATION_SEED_PREFIX, &chain.to_be_bytes()],
        &program_id(),
    );
    pda
}

fn derive_record_pda(sequence: u64) -> Pubkey {
    let (pda, _) = Pubkey::find_program_address(
        &[REGISTER_CHAIN_SEED_PREFIX, &sequence.to_be_bytes()],
        &program_id(),
    );
    pda
}

/// Program-owned account of `len` zero bytes: stands in for a PDA a previous tx created.
fn existing_pda_account(len: usize) -> Account {
    Account {
        lamports: 1_000_000,
        data: vec![0u8; len],
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

/// Build `(accounts, metas)`: payer, system program, then per entry the
/// `ChainRegistration` PDA followed by the `RegisterChain` PDA.
fn build_invocation(
    signer: Pubkey,
    entries: &[BackfillChainRegistrationEntry],
) -> (Vec<(Pubkey, Account)>, Vec<AccountMeta>) {
    let (sys_id, sys_acc) = keyed_account_for_system_program();

    let mut accounts: Vec<(Pubkey, Account)> =
        vec![(signer, signer_account(10_000_000_000)), (sys_id, sys_acc)];
    let mut metas: Vec<AccountMeta> = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new_readonly(sys_id, false),
    ];
    for e in entries {
        for pda in [
            derive_registration_pda(e.chain()),
            derive_record_pda(e.sequence()),
        ] {
            accounts.push((pda, uninitialised_pda_account()));
            metas.push(AccountMeta::new(pda, false));
        }
    }
    (accounts, metas)
}

fn submit(
    entries: &[BackfillChainRegistrationEntry],
    accounts: &[(Pubkey, Account)],
    metas: Vec<AccountMeta>,
) -> InstructionResult {
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_chain_registration_batch(entries),
    };
    mollusk().process_instruction(&ix, accounts)
}

fn assert_custom_error(result: &InstructionResult, expected: GlobalAccountantError, label: &str) {
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code)) if *code == expected as u32
        ),
        "{label}: expected {expected:?}, got {:?}",
        result.raw_result
    );
}

/// Both PDAs exist, are program-owned, have the exact layout length, and hold the layouts
/// `register_chain` builds through the same `ChainRegistrationLayout::new` /
/// `RegisterChainLayout::new` constructors.
fn assert_both_pdas(result: &InstructionResult, entry: &BackfillChainRegistrationEntry) {
    let registration = result
        .get_account(&derive_registration_pda(entry.chain()))
        .expect("ChainRegistration PDA");
    assert_eq!(registration.owner, program_id());
    assert_eq!(registration.data.len(), ChainRegistrationLayout::LEN);
    let layout: &ChainRegistrationLayout = bytemuck::from_bytes(&registration.data);
    assert_eq!(
        *layout,
        ChainRegistrationLayout::new(entry.chain(), entry.emitter, entry.sequence())
    );

    let record = result
        .get_account(&derive_record_pda(entry.sequence()))
        .expect("RegisterChain PDA");
    assert_eq!(record.owner, program_id());
    assert_eq!(record.data.len(), RegisterChainLayout::LEN);
    let layout: &RegisterChainLayout = bytemuck::from_bytes(&record.data);
    assert_eq!(
        *layout,
        RegisterChainLayout::new(entry.chain(), entry.emitter, entry.sequence())
    );
}

// ============================================================================
// Tests
// ============================================================================

#[test]
fn backfill_chain_registration_single_entry_writes_both_pdas() {
    let entry = chain_registration_entry(ETHEREUM, 500, [0x11u8; 32]);
    let (accounts, metas) = build_invocation(test_authority_pubkey(), &[entry]);

    let result = submit(&[entry], &accounts, metas);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw_result={:?}",
        result.raw_result
    );
    assert_both_pdas(&result, &entry);
}

/// Three chains in one ix, ordered by `chain`. Governance sequences are random, so the
/// batch carries them in arbitrary order.
#[test]
fn backfill_chain_registration_bulk_writes_multiple_chains() {
    let entries = [
        chain_registration_entry(ETHEREUM, 900, [0x11u8; 32]),
        chain_registration_entry(BSC, 12, [0x22u8; 32]),
        chain_registration_entry(POLYGON, 4_000, [0x33u8; 32]),
    ];
    let (accounts, metas) = build_invocation(test_authority_pubkey(), &entries);

    let result = submit(&entries, &accounts, metas);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw_result={:?}",
        result.raw_result
    );
    for entry in &entries {
        assert_both_pdas(&result, entry);
    }
}

#[test]
fn backfill_chain_registration_wrong_signer_rejects() {
    let wrong_signer = Pubkey::new_from_array([0xDEu8; 32]);
    let entry = chain_registration_entry(ETHEREUM, 500, [0x11u8; 32]);
    let (accounts, metas) = build_invocation(wrong_signer, &[entry]);

    let result = submit(&[entry], &accounts, metas);
    assert_custom_error(
        &result,
        GlobalAccountantError::UnauthorizedCaller,
        "wrong signer",
    );
    let registration = result
        .get_account(&derive_registration_pda(ETHEREUM))
        .expect("stub still present");
    assert_eq!(registration.data.len(), 0, "no registration written");
}

/// The backfill is create-only. A chain that already has a registration (a previous batch
/// landed it, or the operational program wrote it) must fail rather than be overwritten.
#[test]
fn backfill_chain_registration_existing_registration_rejects() {
    let entry = chain_registration_entry(ETHEREUM, 500, [0x11u8; 32]);
    let (mut accounts, metas) = build_invocation(test_authority_pubkey(), &[entry]);
    accounts[2].1 = existing_pda_account(ChainRegistrationLayout::LEN);

    let result = submit(&[entry], &accounts, metas);
    assert_custom_error(
        &result,
        GlobalAccountantError::InvalidPda,
        "existing registration",
    );
}

/// A sequence whose record already exists is a replay of the installing VAA; reject it.
#[test]
fn backfill_chain_registration_existing_record_rejects() {
    let entry = chain_registration_entry(ETHEREUM, 500, [0x11u8; 32]);
    let (mut accounts, metas) = build_invocation(test_authority_pubkey(), &[entry]);
    accounts[3].1 = existing_pda_account(RegisterChainLayout::LEN);

    let result = submit(&[entry], &accounts, metas);
    assert_custom_error(
        &result,
        GlobalAccountantError::InvalidPda,
        "existing record",
    );
}

/// Remaining accounts must be exactly two per entry.
#[test]
fn backfill_chain_registration_account_count_mismatch_rejects() {
    let entries = [
        chain_registration_entry(ETHEREUM, 900, [0x11u8; 32]),
        chain_registration_entry(BSC, 12, [0x22u8; 32]),
    ];
    let (accounts, metas) = build_invocation(test_authority_pubkey(), &entries);

    let cases: [(&str, usize); 2] = [("one PDA short", 5), ("only the first pair", 4)];
    for (label, keep) in cases {
        let result = submit(&entries, &accounts[..keep], metas[..keep].to_vec());
        assert_custom_error(
            &result,
            GlobalAccountantError::InvalidInstructionData,
            label,
        );
    }
}

/// Registration and record PDAs passed in reverse order fail the address check.
#[test]
fn backfill_chain_registration_swapped_pdas_reject() {
    let entry = chain_registration_entry(ETHEREUM, 500, [0x11u8; 32]);
    let (mut accounts, mut metas) = build_invocation(test_authority_pubkey(), &[entry]);
    accounts.swap(2, 3);
    metas.swap(2, 3);

    let result = submit(&[entry], &accounts, metas);
    assert_custom_error(&result, GlobalAccountantError::InvalidPda, "swapped PDAs");
}
