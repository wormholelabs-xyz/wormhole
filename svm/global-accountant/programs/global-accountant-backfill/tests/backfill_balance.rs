//! Integration tests for `BackfillBalance` — bulk-batched `BalanceAccountLayout`
//! writes from a wormchain `query_all_accounts` row.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_backfill::{BackfillError, Instruction as IxDiscriminator},
    global_accountant_definitions::{BalanceAccountLayout, Uint256, ACCOUNT_SEED_PREFIX},
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult},
    solana_account::Account,
    solana_instruction::{error::InstructionError, AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

mod common;
use common::mollusk::{
    mollusk, program_id, signer_account, test_authority_pubkey, uninitialised_pda_account,
};

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

/// Build `(accounts, metas)` for a BackfillBalance invocation.
/// `signer` parameterised so individual tests can probe wrong-pubkey paths.
fn build_invocation(
    signer: Pubkey,
    entries: &[BalanceEntry],
) -> (Vec<(Pubkey, Account)>, Vec<AccountMeta>) {
    let (sys_id, sys_acc) = keyed_account_for_system_program();

    let mut accounts: Vec<(Pubkey, Account)> = vec![
        (signer, signer_account(10_000_000_000)),
        (sys_id, sys_acc),
    ];
    let mut metas: Vec<AccountMeta> = vec![
        AccountMeta::new(signer, true),
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

/// Single entry: balance PDA created at canonical seeds with the supplied fields.
#[test]
fn backfill_balance_single_entry_writes_pda() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
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
}

/// Three distinct balances in one ix.
#[test]
fn backfill_balance_bulk_writes_multiple_pdas() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
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

/// Re-submitting an entry whose PDA is already owned by the program (because
/// a previous backfill tx landed it) must hard-fail. `BackfillBalance` is not
/// idempotent — unlike `BackfillNoReplay` which gets idempotency for free via
/// `solana-noreplay`'s `AlreadyAccounted` short-circuit, the only thing
/// stopping a duplicate write here is `pda_init::init_or_upgrade_pda`'s
/// `data_len != 0 || !initial_owner_is_system` guard. The orchestrator's
/// cursor is what *normally* prevents re-submission, but if the cursor lags or
/// an operator manually re-runs a chunk this guard is the backstop.
#[test]
fn backfill_balance_resubmission_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = BalanceEntry {
        chain: 2,
        token_chain: 2,
        token_address: [0x11u8; 32],
        balance: [0xaau8; 32],
    };

    // ---------- First submit (fresh PDA) — must succeed ----------
    let (accounts, metas) = build_invocation(signer, &[entry]);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas.clone(),
        data: build_ix_data(&[entry]),
    };
    let first = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        first.program_result,
        ProgramResult::Success,
        "first submit must land cleanly: {:?}",
        first.raw_result
    );

    // ---------- Second submit — pipe the resulting account state back in ----------
    //
    // Pull the now-initialised balance PDA out of the first run's resulting
    // accounts and use it as the input to the second invocation. The signer's
    // post-state (with lamports debited for rent) also feeds back in so
    // CreateAccount accounting stays consistent.
    let pda = derive_account_pda(entry.chain, entry.token_chain, &entry.token_address);
    let pda_post = first
        .resulting_accounts
        .iter()
        .find(|(k, _)| k == &pda)
        .map(|(_, a)| a.clone())
        .expect("balance PDA in resulting accounts");
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
    assert!(
        matches!(
            &second.raw_result,
            Err(InstructionError::Custom(code))
                if *code == BackfillError::InvalidPda as u32
        ),
        "expected InvalidPda on re-submission, got {:?}",
        second.raw_result
    );
}

/// Caller signs with a pubkey other than `BACKFILL_AUTHORITY` → `UnauthorizedCaller`.
#[test]
fn backfill_balance_wrong_signer_rejects() {
    let mollusk = mollusk();
    let wrong_signer = Pubkey::new_from_array([0xDEu8; 32]); // not the test authority
    let entry = BalanceEntry {
        chain: 2,
        token_chain: 2,
        token_address: [0x11u8; 32],
        balance: [0xaau8; 32],
    };
    let (accounts, metas) = build_invocation(wrong_signer, &[entry]);

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
                if *code == BackfillError::UnauthorizedCaller as u32
        ),
        "expected UnauthorizedCaller, got {:?}",
        result.raw_result
    );
}
