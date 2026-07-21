//! Integration tests for the NTT-native backfill map instructions:
//! `BackfillRelayerRegistration`, `BackfillTransceiverHub`, and
//! `BackfillTransceiverPeer`. Each writes a tagged layout PDA per sorted entry;
//! mirrors the WTT `backfill_balance` suite.
//!
//! Also covers cross-program authority isolation: a WTT-signed instruction is
//! rejected by the NTT program, and vice versa.

use {
    global_accountant_definitions::{
        RelayerChainRegistrationLayout, TransceiverHubLayout, TransceiverPeerLayout,
        ACCOUNT_SEED_PREFIX, RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, TRANSCEIVER_HUB_SEED_PREFIX,
        TRANSCEIVER_PEER_SEED_PREFIX,
    },
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk},
    ntt_global_accountant_backfill::{BackfillError, Instruction as Ix, BACKFILL_AUTHORITY},
    solana_account::Account,
    solana_instruction::{error::InstructionError, AccountMeta, Instruction},
    solana_pubkey::Pubkey,
    solana_rent::Rent,
};

mod common;
use common::{
    mollusk, program_id, signer_account, system_owned_account, test_authority_pubkey,
    uninitialised_pda_account, wtt_authority_pubkey,
};

/// `(accounts, metas)` skeleton: signer + system program, then one PDA slot per
/// supplied address, in order. `is_signer` parameterised so
/// missing-signature tests can flip the meta's signer bit while keeping the
/// pubkey correct.
fn invocation_with_signer_flag(
    signer: Pubkey,
    is_signer: bool,
    pdas: &[Pubkey],
) -> (Vec<(Pubkey, Account)>, Vec<AccountMeta>) {
    let (sys_id, sys_acc) = keyed_account_for_system_program();
    let mut accounts = vec![(signer, signer_account(10_000_000_000)), (sys_id, sys_acc)];
    let mut metas = vec![
        AccountMeta::new(signer, is_signer),
        AccountMeta::new_readonly(sys_id, false),
    ];
    for pda in pdas {
        accounts.push((*pda, uninitialised_pda_account()));
        metas.push(AccountMeta::new(*pda, false));
    }
    (accounts, metas)
}

fn invocation(signer: Pubkey, pdas: &[Pubkey]) -> (Vec<(Pubkey, Account)>, Vec<AccountMeta>) {
    invocation_with_signer_flag(signer, true, pdas)
}

fn assert_custom(result: &mollusk_svm::result::InstructionResult, expected: BackfillError) {
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code)) if *code == expected as u32
        ),
        "expected {:?}, got {:?}",
        expected,
        result.raw_result
    );
}

fn assert_missing_signature(result: &mollusk_svm::result::InstructionResult) {
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::MissingRequiredSignature)
        ),
        "expected MissingRequiredSignature, got {:?}",
        result.raw_result
    );
}

/// `InstructionError::NotEnoughAccountKeys` is deprecated but still what the
/// runtime actually returns here.
#[allow(deprecated)]
fn assert_not_enough_account_keys(result: &mollusk_svm::result::InstructionResult) {
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::NotEnoughAccountKeys)
        ),
        "expected NotEnoughAccountKeys, got {:?}",
        result.raw_result
    );
}

// ============================================================================
// Authority const sanity + cross-program isolation
// ============================================================================

/// Sanity check: `BACKFILL_AUTHORITY` matches the deterministic test keypair
/// (seed `[2u8; 32]`).
#[test]
fn backfill_authority_const_matches_test_keypair() {
    let derived = test_authority_pubkey().to_bytes();
    assert_eq!(
        BACKFILL_AUTHORITY, derived,
        "\n\nBACKFILL_AUTHORITY drift!\n  hardcoded: {:?}\n  expected:  {:?}\n\
         Either restore the test default or rebuild test fixtures with the\n\
         operator's keypair (not recommended).",
        BACKFILL_AUTHORITY, derived
    );
}

/// NTT and WTT test authorities must differ, or the isolation tests below
/// are meaningless.
#[test]
fn ntt_and_wtt_test_authorities_are_distinct() {
    assert_ne!(
        test_authority_pubkey(),
        wtt_authority_pubkey(),
        "NTT and WTT test authorities must be distinct keys, or every \
         cross-program isolation test below is meaningless by construction"
    );
}

/// WTT's operator authority is rejected by the NTT program.
#[test]
fn cross_program_authority_rejected() {
    let mollusk = mollusk();
    let signer = wtt_authority_pubkey();
    let entries = [(2u16, [0x11u8; 32])];
    let pdas: Vec<Pubkey> = entries.iter().map(|(c, _)| relayer_pda(*c)).collect();
    let (accounts, metas) = invocation(signer, &pdas);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: relayer_ix_data(&entries),
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::UnauthorizedCaller,
    );
}

/// NTT's authority is rejected by the WTT program (reverse of
/// `cross_program_authority_rejected`).
#[test]
fn cross_program_authority_rejected_reverse() {
    // Arbitrary but deterministic WTT-side program id, distinct from the NTT
    // suite's own `program_id()` (`[9u8; 32]`).
    let wtt_program_id = Pubkey::new_from_array([8u8; 32]);
    let wtt_mollusk = Mollusk::new(&wtt_program_id, "global_accountant_backfill");

    let signer = test_authority_pubkey(); // NTT authority — wrong for WTT.
    let chain = 2u16;
    let token_chain = 2u16;
    let token_address = [0x11u8; 32];
    let balance_pda = Pubkey::find_program_address(
        &[
            ACCOUNT_SEED_PREFIX,
            &chain.to_be_bytes(),
            &token_chain.to_be_bytes(),
            &token_address,
        ],
        &wtt_program_id,
    )
    .0;

    let (sys_id, sys_acc) = keyed_account_for_system_program();
    let accounts = vec![
        (signer, signer_account(10_000_000_000)),
        (sys_id, sys_acc),
        (balance_pda, uninitialised_pda_account()),
    ];
    let metas = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new_readonly(sys_id, false),
        AccountMeta::new(balance_pda, false),
    ];

    // WTT's `BackfillBalance` (discriminator 1) wire format: count, then
    // chain/token_chain/token_address/balance per entry.
    let mut data = vec![global_accountant_backfill::Instruction::BackfillBalance as u8, 1u8];
    data.extend_from_slice(&chain.to_be_bytes());
    data.extend_from_slice(&token_chain.to_be_bytes());
    data.extend_from_slice(&token_address);
    data.extend_from_slice(&[0u8; 32]); // balance

    let ix = Instruction {
        program_id: wtt_program_id,
        accounts: metas,
        data,
    };
    let result = wtt_mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == global_accountant_backfill::BackfillError::UnauthorizedCaller as u32
        ),
        "expected UnauthorizedCaller from the WTT program when signed by the NTT authority, got {:?}",
        result.raw_result
    );
}

// ============================================================================
// Entrypoint dispatch: unknown / zero-byte discriminator
// ============================================================================

/// Zero-byte instruction data must reject with `InvalidInstructionData` (the
/// entrypoint's `split_first()` guard).
#[test]
fn entrypoint_empty_instruction_data_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let (accounts, metas) = invocation(signer, &[]);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: vec![],
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidInstructionData,
    );
}

/// Unknown discriminator byte rejects with `InvalidInstruction`, distinct
/// from `InvalidInstructionData` above.
#[test]
fn entrypoint_unknown_discriminator_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let (accounts, metas) = invocation(signer, &[]);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: vec![0xFFu8],
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidInstruction,
    );
}

// ============================================================================
// BackfillRelayerRegistration
// ============================================================================

fn relayer_pda(chain: u16) -> Pubkey {
    Pubkey::find_program_address(
        &[RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, &chain.to_be_bytes()],
        &program_id(),
    )
    .0
}

fn relayer_ix_data(entries: &[(u16, [u8; 32])]) -> Vec<u8> {
    let mut data = vec![Ix::BackfillRelayerRegistration as u8, entries.len() as u8];
    for (chain, emitter) in entries {
        data.extend_from_slice(&chain.to_be_bytes());
        data.extend_from_slice(emitter);
    }
    data
}

#[test]
fn relayer_registration_writes_pdas() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [(2u16, [0x11u8; 32]), (10u16, [0x22u8; 32])];
    let pdas: Vec<Pubkey> = entries.iter().map(|(c, _)| relayer_pda(*c)).collect();
    let (accounts, metas) = invocation(signer, &pdas);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: relayer_ix_data(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw={:?}",
        result.raw_result
    );

    for (chain, emitter) in &entries {
        let acc = result.get_account(&relayer_pda(*chain)).unwrap();
        assert_eq!(acc.data.len(), RelayerChainRegistrationLayout::LEN);
        assert_eq!(acc.owner, program_id());
        let layout: &RelayerChainRegistrationLayout = bytemuck::from_bytes(&acc.data);
        assert_eq!(layout.tag, RelayerChainRegistrationLayout::TAG);
        assert_eq!(layout.chain, *chain);
        assert_eq!(&layout.emitter_address, emitter);
    }
}

#[test]
fn relayer_registration_unsorted_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    // chain 10 then 2 — descending, must reject.
    let entries = [(10u16, [0x11u8; 32]), (2u16, [0x22u8; 32])];
    let pdas: Vec<Pubkey> = entries.iter().map(|(c, _)| relayer_pda(*c)).collect();
    let (accounts, metas) = invocation(signer, &pdas);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: relayer_ix_data(&entries),
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidInstructionData,
    );
}

#[test]
fn relayer_registration_wrong_signer_rejected() {
    let mollusk = mollusk();
    let wrong = Pubkey::new_from_array([0xDEu8; 32]);
    let entries = [(2u16, [0x11u8; 32])];
    let pdas: Vec<Pubkey> = entries.iter().map(|(c, _)| relayer_pda(*c)).collect();
    let (accounts, metas) = invocation(wrong, &pdas);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: relayer_ix_data(&entries),
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::UnauthorizedCaller,
    );
}

/// Correct authority pubkey, but the signer bit is unset — `require_authority`
/// must check `is_signer()` before comparing pubkeys.
#[test]
fn relayer_registration_missing_signature_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [(2u16, [0x11u8; 32])];
    let pdas: Vec<Pubkey> = entries.iter().map(|(c, _)| relayer_pda(*c)).collect();
    let (accounts, metas) = invocation_with_signer_flag(signer, false, &pdas);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: relayer_ix_data(&entries),
    };
    assert_missing_signature(&mollusk.process_instruction(&ix, &accounts));
}

/// Supplied PDA doesn't match the canonical derivation for the entry's
/// `chain` — must reject with `InvalidPda`.
#[test]
fn relayer_registration_pda_mismatch_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [(2u16, [0x11u8; 32])];
    // Canonical PDA for chain 2, but pass the PDA derived for chain 3 instead.
    let wrong_pda = relayer_pda(3);
    let (accounts, metas) = invocation(signer, &[wrong_pda]);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: relayer_ix_data(&entries),
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidPda,
    );
}

/// Wire `count` header declares 2 entries but only 1 PDA account is supplied.
#[test]
fn relayer_registration_account_count_mismatch_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [(2u16, [0x11u8; 32]), (10u16, [0x22u8; 32])];
    // Only supply the first PDA even though the wire data declares 2 entries.
    let pdas = [relayer_pda(entries[0].0)];
    let (accounts, metas) = invocation(signer, &pdas);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: relayer_ix_data(&entries),
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidInstructionData,
    );
}

/// Wire data truncated mid-entry — `data.len() != expected_len` guard.
#[test]
fn relayer_registration_truncated_data_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [(2u16, [0x11u8; 32])];
    let pdas: Vec<Pubkey> = entries.iter().map(|(c, _)| relayer_pda(*c)).collect();
    let (accounts, metas) = invocation(signer, &pdas);
    let mut data = relayer_ix_data(&entries);
    data.pop(); // chop the last byte of the single entry
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data,
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidInstructionData,
    );
}

/// Re-submitting an already-initialised PDA must hard-fail
/// (`init_or_upgrade_pda`'s data-len/owner guard).
#[test]
fn relayer_registration_resubmission_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [(2u16, [0x11u8; 32])];
    let pdas: Vec<Pubkey> = entries.iter().map(|(c, _)| relayer_pda(*c)).collect();

    // ---------- First submit — must succeed ----------
    let (accounts, metas) = invocation(signer, &pdas);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas.clone(),
        data: relayer_ix_data(&entries),
    };
    let first = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        first.program_result,
        ProgramResult::Success,
        "first submit must land cleanly: {:?}",
        first.raw_result
    );

    // ---------- Second submit — feed the now-initialised PDA back in ----------
    let pda = pdas[0];
    let pda_post = first
        .resulting_accounts
        .iter()
        .find(|(k, _)| k == &pda)
        .map(|(_, a)| a.clone())
        .expect("relayer PDA in resulting accounts");
    let signer_post = first
        .resulting_accounts
        .iter()
        .find(|(k, _)| k == &signer)
        .map(|(_, a)| a.clone())
        .expect("signer in resulting accounts");
    let (sys_id, sys_acc) = keyed_account_for_system_program();
    let accounts_replay: Vec<(Pubkey, Account)> =
        vec![(signer, signer_post), (sys_id, sys_acc), (pda, pda_post)];
    let second = mollusk.process_instruction(&ix, &accounts_replay);
    assert_custom(&second, BackfillError::InvalidPda);
}

/// Wire `count` header of 0 — no entries at all — must reject, not silently
/// succeed as a no-op.
#[test]
fn relayer_registration_zero_count_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let (accounts, metas) = invocation(signer, &[]);
    let data = vec![Ix::BackfillRelayerRegistration as u8, 0u8];
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data,
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidInstructionData,
    );
}

/// No bytes at all after the discriminator (not even `count`) — the
/// `data.len() < FIXED_HEAD` guard, distinct from the `count == 0` case above.
#[test]
fn relayer_registration_empty_data_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let (accounts, metas) = invocation(signer, &[]);
    let data = vec![Ix::BackfillRelayerRegistration as u8];
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data,
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidInstructionData,
    );
}

/// Fewer than 2 accounts supplied at all — the slice-pattern match itself
/// fails, distinct from `relayer_registration_account_count_mismatch_rejected`
/// (right slot count, wrong PDA count).
#[test]
fn relayer_registration_not_enough_account_keys_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [(2u16, [0x11u8; 32])];
    // Only the signer — no system program, no PDA.
    let accounts = vec![(signer, signer_account(10_000_000_000))];
    let metas = vec![AccountMeta::new(signer, true)];
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: relayer_ix_data(&entries),
    };
    assert_not_enough_account_keys(&mollusk.process_instruction(&ix, &accounts));
}

/// PDA arrives already rent-exempt — only Allocate+Assign CPIs fire, no
/// Transfer (verified via unchanged payer lamports).
#[test]
fn relayer_registration_prefunded_pda_allocate_assign_no_transfer() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [(2u16, [0x11u8; 32])];
    let pda = relayer_pda(entries[0].0);
    let rent_exempt_minimum = Rent::default().minimum_balance(RelayerChainRegistrationLayout::LEN);

    let (sys_id, sys_acc) = keyed_account_for_system_program();
    let payer_lamports = 10_000_000_000u64;
    let accounts = vec![
        (signer, signer_account(payer_lamports)),
        (sys_id, sys_acc),
        (pda, system_owned_account(rent_exempt_minimum)),
    ];
    let metas = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new_readonly(sys_id, false),
        AccountMeta::new(pda, false),
    ];
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: relayer_ix_data(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw={:?}",
        result.raw_result
    );

    let payer_post = result
        .resulting_accounts
        .iter()
        .find(|(k, _)| k == &signer)
        .map(|(_, a)| a.clone())
        .expect("signer in resulting accounts");
    assert_eq!(
        payer_post.lamports, payer_lamports,
        "payer lamports must be untouched when the PDA arrives already \
         rent-exempt — a Transfer CPI would have debited it"
    );

    let acc = result.get_account(&pda).unwrap();
    assert_eq!(acc.data.len(), RelayerChainRegistrationLayout::LEN);
    assert_eq!(acc.owner, program_id());
    assert_eq!(acc.lamports, rent_exempt_minimum);
    let layout: &RelayerChainRegistrationLayout = bytemuck::from_bytes(&acc.data);
    assert_eq!(layout.tag, RelayerChainRegistrationLayout::TAG);
    assert_eq!(layout.chain, entries[0].0);
    assert_eq!(layout.emitter_address, entries[0].1);
}

// ============================================================================
// BackfillTransceiverHub
// ============================================================================

fn hub_pda(chain: u16, address: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(
        &[TRANSCEIVER_HUB_SEED_PREFIX, &chain.to_be_bytes(), address],
        &program_id(),
    )
    .0
}

fn hub_ix_data(entries: &[(u16, [u8; 32], u16, [u8; 32])]) -> Vec<u8> {
    let mut data = vec![Ix::BackfillTransceiverHub as u8, entries.len() as u8];
    for (chain, address, hub_chain, hub_address) in entries {
        data.extend_from_slice(&chain.to_be_bytes());
        data.extend_from_slice(address);
        data.extend_from_slice(&hub_chain.to_be_bytes());
        data.extend_from_slice(hub_address);
    }
    data
}

#[test]
fn transceiver_hub_writes_pda() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = (2u16, [0x11u8; 32], 5u16, [0x99u8; 32]);
    let pda = hub_pda(entry.0, &entry.1);
    let (accounts, metas) = invocation(signer, &[pda]);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: hub_ix_data(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw={:?}",
        result.raw_result
    );

    let acc = result.get_account(&pda).unwrap();
    assert_eq!(acc.data.len(), TransceiverHubLayout::LEN);
    let layout: &TransceiverHubLayout = bytemuck::from_bytes(&acc.data);
    assert_eq!(layout.tag, TransceiverHubLayout::TAG);
    assert_eq!(layout.chain, entry.0);
    assert_eq!(layout.address, entry.1);
    assert_eq!(layout.hub_chain, entry.2);
    assert_eq!(layout.hub_address, entry.3);
}

#[test]
fn transceiver_hub_unsorted_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    // Same chain, descending address — must reject on the (chain, address) key.
    let entries = [
        (2u16, [0x22u8; 32], 5u16, [0u8; 32]),
        (2u16, [0x11u8; 32], 5u16, [0u8; 32]),
    ];
    let pdas: Vec<Pubkey> = entries.iter().map(|e| hub_pda(e.0, &e.1)).collect();
    let (accounts, metas) = invocation(signer, &pdas);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: hub_ix_data(&entries),
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidInstructionData,
    );
}

#[test]
fn transceiver_hub_wrong_signer_rejected() {
    let mollusk = mollusk();
    let wrong = Pubkey::new_from_array([0xDEu8; 32]);
    let entry = (2u16, [0x11u8; 32], 5u16, [0x99u8; 32]);
    let pda = hub_pda(entry.0, &entry.1);
    let (accounts, metas) = invocation(wrong, &[pda]);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: hub_ix_data(&[entry]),
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::UnauthorizedCaller,
    );
}

#[test]
fn transceiver_hub_missing_signature_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = (2u16, [0x11u8; 32], 5u16, [0x99u8; 32]);
    let pda = hub_pda(entry.0, &entry.1);
    let (accounts, metas) = invocation_with_signer_flag(signer, false, &[pda]);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: hub_ix_data(&[entry]),
    };
    assert_missing_signature(&mollusk.process_instruction(&ix, &accounts));
}

#[test]
fn transceiver_hub_pda_mismatch_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = (2u16, [0x11u8; 32], 5u16, [0x99u8; 32]);
    // Canonical PDA for (chain=2, address=0x11...), but pass the PDA derived
    // for a different address instead.
    let wrong_pda = hub_pda(entry.0, &[0x22u8; 32]);
    let (accounts, metas) = invocation(signer, &[wrong_pda]);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: hub_ix_data(&[entry]),
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidPda,
    );
}

#[test]
fn transceiver_hub_account_count_mismatch_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [
        (2u16, [0x11u8; 32], 5u16, [0u8; 32]),
        (3u16, [0x22u8; 32], 6u16, [0u8; 32]),
    ];
    let pdas = [hub_pda(entries[0].0, &entries[0].1)];
    let (accounts, metas) = invocation(signer, &pdas);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: hub_ix_data(&entries),
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidInstructionData,
    );
}

#[test]
fn transceiver_hub_truncated_data_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = (2u16, [0x11u8; 32], 5u16, [0x99u8; 32]);
    let pda = hub_pda(entry.0, &entry.1);
    let (accounts, metas) = invocation(signer, &[pda]);
    let mut data = hub_ix_data(&[entry]);
    data.pop();
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data,
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidInstructionData,
    );
}

/// Re-submitting an entry whose PDA is already program-owned must hard-fail,
/// same rationale/mechanism as `relayer_registration_resubmission_rejected`.
#[test]
fn transceiver_hub_resubmission_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = (2u16, [0x11u8; 32], 5u16, [0x99u8; 32]);
    let pda = hub_pda(entry.0, &entry.1);

    // ---------- First submit — must succeed ----------
    let (accounts, metas) = invocation(signer, &[pda]);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas.clone(),
        data: hub_ix_data(&[entry]),
    };
    let first = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        first.program_result,
        ProgramResult::Success,
        "first submit must land cleanly: {:?}",
        first.raw_result
    );

    // ---------- Second submit — feed the now-initialised PDA back in ----------
    let pda_post = first
        .resulting_accounts
        .iter()
        .find(|(k, _)| k == &pda)
        .map(|(_, a)| a.clone())
        .expect("hub PDA in resulting accounts");
    let signer_post = first
        .resulting_accounts
        .iter()
        .find(|(k, _)| k == &signer)
        .map(|(_, a)| a.clone())
        .expect("signer in resulting accounts");
    let (sys_id, sys_acc) = keyed_account_for_system_program();
    let accounts_replay: Vec<(Pubkey, Account)> =
        vec![(signer, signer_post), (sys_id, sys_acc), (pda, pda_post)];
    let second = mollusk.process_instruction(&ix, &accounts_replay);
    assert_custom(&second, BackfillError::InvalidPda);
}

/// Wire `count` header of 0 — must reject.
#[test]
fn transceiver_hub_zero_count_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let (accounts, metas) = invocation(signer, &[]);
    let data = vec![Ix::BackfillTransceiverHub as u8, 0u8];
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data,
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidInstructionData,
    );
}

/// No bytes at all after the dispatch discriminator.
#[test]
fn transceiver_hub_empty_data_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let (accounts, metas) = invocation(signer, &[]);
    let data = vec![Ix::BackfillTransceiverHub as u8];
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data,
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidInstructionData,
    );
}

/// Fewer than 2 accounts supplied at all — the fixed-slot slice pattern
/// itself fails to match.
#[test]
fn transceiver_hub_not_enough_account_keys_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = (2u16, [0x11u8; 32], 5u16, [0x99u8; 32]);
    let accounts = vec![(signer, signer_account(10_000_000_000))];
    let metas = vec![AccountMeta::new(signer, true)];
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: hub_ix_data(&[entry]),
    };
    assert_not_enough_account_keys(&mollusk.process_instruction(&ix, &accounts));
}

/// PDA arrives already prefunded to the rent-exempt minimum — only
/// `Allocate` + `Assign` fire, no `Transfer`.
#[test]
fn transceiver_hub_prefunded_pda_allocate_assign_no_transfer() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = (2u16, [0x11u8; 32], 5u16, [0x99u8; 32]);
    let pda = hub_pda(entry.0, &entry.1);
    let rent_exempt_minimum = Rent::default().minimum_balance(TransceiverHubLayout::LEN);

    let (sys_id, sys_acc) = keyed_account_for_system_program();
    let payer_lamports = 10_000_000_000u64;
    let accounts = vec![
        (signer, signer_account(payer_lamports)),
        (sys_id, sys_acc),
        (pda, system_owned_account(rent_exempt_minimum)),
    ];
    let metas = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new_readonly(sys_id, false),
        AccountMeta::new(pda, false),
    ];
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: hub_ix_data(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw={:?}",
        result.raw_result
    );

    let payer_post = result
        .resulting_accounts
        .iter()
        .find(|(k, _)| k == &signer)
        .map(|(_, a)| a.clone())
        .expect("signer in resulting accounts");
    assert_eq!(
        payer_post.lamports, payer_lamports,
        "payer lamports must be untouched when the PDA arrives already \
         rent-exempt — a Transfer CPI would have debited it"
    );

    let acc = result.get_account(&pda).unwrap();
    assert_eq!(acc.data.len(), TransceiverHubLayout::LEN);
    assert_eq!(acc.owner, program_id());
    assert_eq!(acc.lamports, rent_exempt_minimum);
    let layout: &TransceiverHubLayout = bytemuck::from_bytes(&acc.data);
    assert_eq!(layout.tag, TransceiverHubLayout::TAG);
    assert_eq!(layout.chain, entry.0);
    assert_eq!(layout.address, entry.1);
    assert_eq!(layout.hub_chain, entry.2);
    assert_eq!(layout.hub_address, entry.3);
}

// ============================================================================
// BackfillTransceiverPeer
// ============================================================================

fn peer_pda(chain: u16, address: &[u8; 32], dest_chain: u16) -> Pubkey {
    Pubkey::find_program_address(
        &[
            TRANSCEIVER_PEER_SEED_PREFIX,
            &chain.to_be_bytes(),
            address,
            &dest_chain.to_be_bytes(),
        ],
        &program_id(),
    )
    .0
}

fn peer_ix_data(entries: &[(u16, [u8; 32], u16, [u8; 32])]) -> Vec<u8> {
    let mut data = vec![Ix::BackfillTransceiverPeer as u8, entries.len() as u8];
    for (chain, address, dest_chain, peer_address) in entries {
        data.extend_from_slice(&chain.to_be_bytes());
        data.extend_from_slice(address);
        data.extend_from_slice(&dest_chain.to_be_bytes());
        data.extend_from_slice(peer_address);
    }
    data
}

#[test]
fn transceiver_peer_writes_pda() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = (2u16, [0x11u8; 32], 10u16, [0x77u8; 32]);
    let pda = peer_pda(entry.0, &entry.1, entry.2);
    let (accounts, metas) = invocation(signer, &[pda]);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: peer_ix_data(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw={:?}",
        result.raw_result
    );

    let acc = result.get_account(&pda).unwrap();
    assert_eq!(acc.data.len(), TransceiverPeerLayout::LEN);
    let layout: &TransceiverPeerLayout = bytemuck::from_bytes(&acc.data);
    assert_eq!(layout.tag, TransceiverPeerLayout::TAG);
    assert_eq!(layout.chain, entry.0);
    assert_eq!(layout.address, entry.1);
    assert_eq!(layout.dest_chain, entry.2);
    assert_eq!(layout.peer_address, entry.3);
}

#[test]
fn transceiver_peer_unsorted_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    // Same (chain, address), descending dest_chain — must reject.
    let entries = [
        (2u16, [0x11u8; 32], 10u16, [0u8; 32]),
        (2u16, [0x11u8; 32], 4u16, [0u8; 32]),
    ];
    let pdas: Vec<Pubkey> = entries.iter().map(|e| peer_pda(e.0, &e.1, e.2)).collect();
    let (accounts, metas) = invocation(signer, &pdas);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: peer_ix_data(&entries),
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidInstructionData,
    );
}

#[test]
fn transceiver_peer_wrong_signer_rejected() {
    let mollusk = mollusk();
    let wrong = Pubkey::new_from_array([0xDEu8; 32]);
    let entry = (2u16, [0x11u8; 32], 10u16, [0x77u8; 32]);
    let pda = peer_pda(entry.0, &entry.1, entry.2);
    let (accounts, metas) = invocation(wrong, &[pda]);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: peer_ix_data(&[entry]),
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::UnauthorizedCaller,
    );
}

#[test]
fn transceiver_peer_missing_signature_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = (2u16, [0x11u8; 32], 10u16, [0x77u8; 32]);
    let pda = peer_pda(entry.0, &entry.1, entry.2);
    let (accounts, metas) = invocation_with_signer_flag(signer, false, &[pda]);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: peer_ix_data(&[entry]),
    };
    assert_missing_signature(&mollusk.process_instruction(&ix, &accounts));
}

#[test]
fn transceiver_peer_pda_mismatch_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = (2u16, [0x11u8; 32], 10u16, [0x77u8; 32]);
    // Canonical PDA for dest_chain=10, but pass the PDA derived for dest_chain=4.
    let wrong_pda = peer_pda(entry.0, &entry.1, 4);
    let (accounts, metas) = invocation(signer, &[wrong_pda]);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: peer_ix_data(&[entry]),
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidPda,
    );
}

#[test]
fn transceiver_peer_account_count_mismatch_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [
        (2u16, [0x11u8; 32], 10u16, [0u8; 32]),
        (2u16, [0x11u8; 32], 20u16, [0u8; 32]),
    ];
    let pdas = [peer_pda(entries[0].0, &entries[0].1, entries[0].2)];
    let (accounts, metas) = invocation(signer, &pdas);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: peer_ix_data(&entries),
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidInstructionData,
    );
}

#[test]
fn transceiver_peer_truncated_data_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = (2u16, [0x11u8; 32], 10u16, [0x77u8; 32]);
    let pda = peer_pda(entry.0, &entry.1, entry.2);
    let (accounts, metas) = invocation(signer, &[pda]);
    let mut data = peer_ix_data(&[entry]);
    data.pop();
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data,
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidInstructionData,
    );
}

/// Re-submitting an entry whose PDA is already program-owned must hard-fail,
/// same rationale/mechanism as `relayer_registration_resubmission_rejected`.
#[test]
fn transceiver_peer_resubmission_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = (2u16, [0x11u8; 32], 10u16, [0x77u8; 32]);
    let pda = peer_pda(entry.0, &entry.1, entry.2);

    // ---------- First submit — must succeed ----------
    let (accounts, metas) = invocation(signer, &[pda]);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas.clone(),
        data: peer_ix_data(&[entry]),
    };
    let first = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        first.program_result,
        ProgramResult::Success,
        "first submit must land cleanly: {:?}",
        first.raw_result
    );

    // ---------- Second submit — feed the now-initialised PDA back in ----------
    let pda_post = first
        .resulting_accounts
        .iter()
        .find(|(k, _)| k == &pda)
        .map(|(_, a)| a.clone())
        .expect("peer PDA in resulting accounts");
    let signer_post = first
        .resulting_accounts
        .iter()
        .find(|(k, _)| k == &signer)
        .map(|(_, a)| a.clone())
        .expect("signer in resulting accounts");
    let (sys_id, sys_acc) = keyed_account_for_system_program();
    let accounts_replay: Vec<(Pubkey, Account)> =
        vec![(signer, signer_post), (sys_id, sys_acc), (pda, pda_post)];
    let second = mollusk.process_instruction(&ix, &accounts_replay);
    assert_custom(&second, BackfillError::InvalidPda);
}

/// Wire `count` header of 0 — must reject.
#[test]
fn transceiver_peer_zero_count_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let (accounts, metas) = invocation(signer, &[]);
    let data = vec![Ix::BackfillTransceiverPeer as u8, 0u8];
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data,
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidInstructionData,
    );
}

/// No bytes at all after the dispatch discriminator.
#[test]
fn transceiver_peer_empty_data_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let (accounts, metas) = invocation(signer, &[]);
    let data = vec![Ix::BackfillTransceiverPeer as u8];
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data,
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::InvalidInstructionData,
    );
}

/// Fewer than 2 accounts supplied at all — the fixed-slot slice pattern
/// itself fails to match.
#[test]
fn transceiver_peer_not_enough_account_keys_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = (2u16, [0x11u8; 32], 10u16, [0x77u8; 32]);
    let accounts = vec![(signer, signer_account(10_000_000_000))];
    let metas = vec![AccountMeta::new(signer, true)];
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: peer_ix_data(&[entry]),
    };
    assert_not_enough_account_keys(&mollusk.process_instruction(&ix, &accounts));
}

/// PDA arrives already prefunded to the rent-exempt minimum — only
/// `Allocate` + `Assign` fire, no `Transfer`.
#[test]
fn transceiver_peer_prefunded_pda_allocate_assign_no_transfer() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = (2u16, [0x11u8; 32], 10u16, [0x77u8; 32]);
    let pda = peer_pda(entry.0, &entry.1, entry.2);
    let rent_exempt_minimum = Rent::default().minimum_balance(TransceiverPeerLayout::LEN);

    let (sys_id, sys_acc) = keyed_account_for_system_program();
    let payer_lamports = 10_000_000_000u64;
    let accounts = vec![
        (signer, signer_account(payer_lamports)),
        (sys_id, sys_acc),
        (pda, system_owned_account(rent_exempt_minimum)),
    ];
    let metas = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new_readonly(sys_id, false),
        AccountMeta::new(pda, false),
    ];
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: peer_ix_data(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw={:?}",
        result.raw_result
    );

    let payer_post = result
        .resulting_accounts
        .iter()
        .find(|(k, _)| k == &signer)
        .map(|(_, a)| a.clone())
        .expect("signer in resulting accounts");
    assert_eq!(
        payer_post.lamports, payer_lamports,
        "payer lamports must be untouched when the PDA arrives already \
         rent-exempt — a Transfer CPI would have debited it"
    );

    let acc = result.get_account(&pda).unwrap();
    assert_eq!(acc.data.len(), TransceiverPeerLayout::LEN);
    assert_eq!(acc.owner, program_id());
    assert_eq!(acc.lamports, rent_exempt_minimum);
    let layout: &TransceiverPeerLayout = bytemuck::from_bytes(&acc.data);
    assert_eq!(layout.tag, TransceiverPeerLayout::TAG);
    assert_eq!(layout.chain, entry.0);
    assert_eq!(layout.address, entry.1);
    assert_eq!(layout.dest_chain, entry.2);
    assert_eq!(layout.peer_address, entry.3);
}
