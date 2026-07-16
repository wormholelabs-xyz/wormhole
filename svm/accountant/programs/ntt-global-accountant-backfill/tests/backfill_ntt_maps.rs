//! Integration tests for the NTT-native backfill map instructions:
//! `BackfillRelayerRegistration`, `BackfillTransceiverHub`, and
//! `BackfillTransceiverPeer`. Each writes a tagged layout PDA per sorted entry;
//! mirrors the WTT `backfill_balance` suite.

use {
    global_accountant_definitions::{
        RelayerChainRegistrationLayout, TransceiverHubLayout, TransceiverPeerLayout,
        RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, TRANSCEIVER_HUB_SEED_PREFIX,
        TRANSCEIVER_PEER_SEED_PREFIX,
    },
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult},
    ntt_global_accountant_backfill::{BackfillError, Instruction as Ix},
    solana_account::Account,
    solana_instruction::{error::InstructionError, AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

mod common;
use common::{
    mollusk, program_id, signer_account, test_authority_pubkey, uninitialised_pda_account,
};

/// `(accounts, metas)` skeleton: signer + system program, then one PDA slot per
/// supplied address, in order.
fn invocation(signer: Pubkey, pdas: &[Pubkey]) -> (Vec<(Pubkey, Account)>, Vec<AccountMeta>) {
    let (sys_id, sys_acc) = keyed_account_for_system_program();
    let mut accounts = vec![(signer, signer_account(10_000_000_000)), (sys_id, sys_acc)];
    let mut metas = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new_readonly(sys_id, false),
    ];
    for pda in pdas {
        accounts.push((*pda, uninitialised_pda_account()));
        metas.push(AccountMeta::new(*pda, false));
    }
    (accounts, metas)
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
