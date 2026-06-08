//! Integration tests for `BackfillBalance` — bulk-batched `BalanceAccountLayout`
//! writes from a wormchain `query_all_accounts` row.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_backfill::{BackfillError, Instruction as IxDiscriminator},
    global_accountant_backfill::state::{BackfillAuthorityLayout, BACKFILL_AUTHORITY_SEED_PREFIX},
    global_accountant_definitions::{BalanceAccountLayout, Uint256, ACCOUNT_SEED_PREFIX},
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk},
    solana_account::Account,
    solana_instruction::{error::InstructionError, AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

mod common;
use common::mollusk_with_noreplay;

fn program_id() -> Pubkey {
    Pubkey::new_from_array([8u8; 32])
}

fn mollusk() -> Mollusk {
    // BackfillBalance does not use noreplay, but the test harness still loads
    // it for parity with the rest of the backfill test suite.
    mollusk_with_noreplay(&program_id())
}

fn system_program_id() -> Pubkey {
    keyed_account_for_system_program().0
}

fn derive_backfill_authority_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[BACKFILL_AUTHORITY_SEED_PREFIX], &program_id())
}

fn derive_account_pda(chain: u16, token_chain: u16, token_address: &[u8; 32]) -> Pubkey {
    let chain_be = chain.to_be_bytes();
    let token_chain_be = token_chain.to_be_bytes();
    let (pda, _) = Pubkey::find_program_address(
        &[
            ACCOUNT_SEED_PREFIX,
            &chain_be,
            &token_chain_be,
            token_address,
        ],
        &program_id(),
    );
    pda
}

// ============================================================================
// Account fixtures
// ============================================================================

fn signer_account(lamports: u64) -> Account {
    Account {
        lamports,
        data: vec![],
        owner: system_program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn uninitialised_pda_account() -> Account {
    Account {
        lamports: 0,
        data: vec![],
        owner: system_program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

// ============================================================================
// Wire format
// ============================================================================

#[derive(Clone, Copy)]
struct BalanceEntry {
    chain: u16,
    token_chain: u16,
    token_address: [u8; 32],
    /// Big-endian Uint256 bytes.
    balance: [u8; 32],
}

fn build_ix_data(entries: &[BalanceEntry]) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 1 + entries.len() * 68);
    data.push(IxDiscriminator::BackfillBalance as u8);
    data.push(entries.len() as u8);
    for e in entries {
        data.extend_from_slice(&e.chain.to_be_bytes());
        data.extend_from_slice(&e.token_chain.to_be_bytes());
        data.extend_from_slice(&e.token_address);
        data.extend_from_slice(&e.balance);
    }
    data
}

fn build_invocation(
    signer: Pubkey,
    entries: &[BalanceEntry],
) -> (Vec<(Pubkey, Account)>, Vec<AccountMeta>) {
    let (backfill_auth_pda, _) = derive_backfill_authority_pda();
    let (sys_id, sys_acc) = keyed_account_for_system_program();

    let mut accounts: Vec<(Pubkey, Account)> = vec![
        (signer, signer_account(10_000_000_000)),
        (backfill_auth_pda, uninitialised_pda_account()),
        (sys_id, sys_acc),
    ];
    let mut metas: Vec<AccountMeta> = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new(backfill_auth_pda, false),
        AccountMeta::new_readonly(sys_id, false),
    ];
    for e in entries {
        let pda = derive_account_pda(e.chain, e.token_chain, &e.token_address);
        accounts.push((pda, uninitialised_pda_account()));
        metas.push(AccountMeta::new(pda, false));
    }
    (accounts, metas)
}

// ============================================================================
// Tests
// ============================================================================

/// Single entry: authority PDA lazy-init, balance PDA created at canonical
/// seeds with the supplied fields.
#[test]
fn backfill_balance_single_entry_writes_pda() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([42u8; 32]);
    let entry = BalanceEntry {
        chain: 2,
        token_chain: 2,
        token_address: [0x11u8; 32],
        balance: [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 1, 0,
        ], // 256 (low byte at position 30)
    };
    let (accounts, metas) = build_invocation(signer, &[entry]);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_ix_data(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw_result={:?}",
        result.raw_result
    );

    let pda = derive_account_pda(entry.chain, entry.token_chain, &entry.token_address);
    let acc = result
        .resulting_accounts
        .iter()
        .find(|(k, _)| k == &pda)
        .unwrap()
        .1
        .clone();
    assert_eq!(acc.data.len(), BalanceAccountLayout::LEN);
    assert_eq!(acc.owner, program_id());

    let layout: &BalanceAccountLayout = bytemuck::from_bytes(&acc.data);
    assert_eq!(layout.chain, entry.chain);
    assert_eq!(layout.token_chain, entry.token_chain);
    assert_eq!(layout.token_address, entry.token_address);
    assert_eq!(layout.balance, Uint256::from_be_bytes(entry.balance));

    // Backfill authority PDA initialised with signer's pubkey, retired=0.
    let (auth_pda, _) = derive_backfill_authority_pda();
    let auth = result.get_account(&auth_pda).unwrap();
    assert_eq!(auth.data.len(), BackfillAuthorityLayout::LEN);
    assert_eq!(&auth.data[..32], signer.as_array());
    assert_eq!(auth.data[32], 0);
}

/// Three distinct balances in one ix.
#[test]
fn backfill_balance_bulk_writes_multiple_pdas() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([43u8; 32]);
    let entries = [
        BalanceEntry {
            chain: 2,
            token_chain: 2,
            token_address: [0x11u8; 32],
            balance: [0xaau8; 32],
        },
        BalanceEntry {
            chain: 2,
            token_chain: 4,
            token_address: [0x22u8; 32],
            balance: [0xbbu8; 32],
        },
        BalanceEntry {
            chain: 5,
            token_chain: 5,
            token_address: [0x33u8; 32],
            balance: [0xccu8; 32],
        },
    ];
    let (accounts, metas) = build_invocation(signer, &entries);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_ix_data(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw_result={:?}",
        result.raw_result
    );

    for e in &entries {
        let pda = derive_account_pda(e.chain, e.token_chain, &e.token_address);
        let acc = result.get_account(&pda).unwrap();
        let layout: &BalanceAccountLayout = bytemuck::from_bytes(&acc.data);
        assert_eq!(layout.chain, e.chain);
        assert_eq!(layout.token_chain, e.token_chain);
        assert_eq!(layout.token_address, e.token_address);
        assert_eq!(layout.balance, Uint256::from_be_bytes(e.balance));
    }
}

/// Out-of-order entries rejected before any PDA touch.
#[test]
fn backfill_balance_out_of_order_rejects() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([44u8; 32]);
    let entries = [
        BalanceEntry {
            chain: 5,
            token_chain: 5,
            token_address: [0x33u8; 32],
            balance: [0xaau8; 32],
        },
        BalanceEntry {
            chain: 2,
            token_chain: 2,
            token_address: [0x11u8; 32],
            balance: [0xbbu8; 32],
        }, // out of order
    ];
    let (accounts, metas) = build_invocation(signer, &entries);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_ix_data(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == BackfillError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData, got {:?}",
        result.raw_result
    );
}

/// Same `(chain, token_chain, token_address)` twice → strict-ascending fails.
#[test]
fn backfill_balance_duplicate_entries_reject() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([45u8; 32]);
    let dup = BalanceEntry {
        chain: 2,
        token_chain: 2,
        token_address: [0x11u8; 32],
        balance: [0xaau8; 32],
    };
    let entries = [dup, dup];
    let (accounts, metas) = build_invocation(signer, &entries);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_ix_data(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(matches!(
        &result.raw_result,
        Err(InstructionError::Custom(code))
            if *code == BackfillError::InvalidInstructionData as u32
    ));
}

/// Non-canonical PDA address → `InvalidPda`.
#[test]
fn backfill_balance_non_canonical_pda_rejects() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([46u8; 32]);
    let entry = BalanceEntry {
        chain: 2,
        token_chain: 2,
        token_address: [0x11u8; 32],
        balance: [0xaau8; 32],
    };
    let (backfill_auth_pda, _) = derive_backfill_authority_pda();
    let (sys_id, sys_acc) = keyed_account_for_system_program();
    let stray = Pubkey::new_from_array([0x99u8; 32]); // not the canonical balance PDA

    let accounts: Vec<(Pubkey, Account)> = vec![
        (signer, signer_account(10_000_000_000)),
        (backfill_auth_pda, uninitialised_pda_account()),
        (sys_id, sys_acc),
        (stray, uninitialised_pda_account()),
    ];
    let metas = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new(backfill_auth_pda, false),
        AccountMeta::new_readonly(sys_id, false),
        AccountMeta::new(stray, false),
    ];
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_ix_data(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == BackfillError::InvalidPda as u32
        ),
        "expected InvalidPda, got {:?}",
        result.raw_result
    );
}

/// Caller passes more balance PDAs than entries — must reject.
#[test]
fn backfill_balance_extra_pda_account_rejects() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([47u8; 32]);
    let entry = BalanceEntry {
        chain: 2,
        token_chain: 2,
        token_address: [0x11u8; 32],
        balance: [0xaau8; 32],
    };
    let (mut accounts, mut metas) = build_invocation(signer, &[entry]);
    // Append one stray PDA.
    let stray = derive_account_pda(999, 999, &[0x99u8; 32]);
    accounts.push((stray, uninitialised_pda_account()));
    metas.push(AccountMeta::new(stray, false));

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_ix_data(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(matches!(
        &result.raw_result,
        Err(InstructionError::Custom(code))
            if *code == BackfillError::InvalidInstructionData as u32
    ));
}

/// Zero-entry ix rejects.
#[test]
fn backfill_balance_zero_entries_rejects() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([48u8; 32]);
    let (accounts, metas) = build_invocation(signer, &[]);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_ix_data(&[]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(matches!(
        &result.raw_result,
        Err(InstructionError::Custom(code))
            if *code == BackfillError::InvalidInstructionData as u32
    ));
}
