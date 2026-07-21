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
    mollusk, program_id, signer_account, system_owned_account, test_authority_pubkey,
    uninitialised_pda_account,
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

#[test]
fn backfill_balance_correct_signer_not_signed_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = BalanceEntry {
        chain: 2,
        token_chain: 2,
        token_address: [0x11u8; 32],
        balance: [0xaau8; 32],
    };
    let (accounts, mut metas) = build_invocation(signer, &[entry]);
    metas[0] = AccountMeta::new(signer, false);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_ix_data(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(&result.raw_result, Err(InstructionError::MissingRequiredSignature)),
        "expected MissingRequiredSignature, got {:?}",
        result.raw_result
    );
}

#[test]
fn backfill_balance_count_zero_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let (accounts, metas) = build_invocation(signer, &[]);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: vec![IxDiscriminator::BackfillBalance as u8, 0u8],
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == BackfillError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData for count == 0, got {:?}",
        result.raw_result
    );
}

#[test]
fn backfill_balance_data_length_mismatch_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = BalanceEntry {
        chain: 2,
        token_chain: 2,
        token_address: [0x11u8; 32],
        balance: [0xaau8; 32],
    };
    let (accounts, metas) = build_invocation(signer, &[entry]);

    // count = 1 declares 69 bytes (1 + 68); shortfall by one byte.
    let mut short_data = build_ix_data(&[entry]);
    short_data.pop();

    let short_ix = Instruction {
        program_id: program_id(),
        accounts: metas.clone(),
        data: short_data,
    };
    let short_result = mollusk.process_instruction(&short_ix, &accounts);
    assert!(
        matches!(
            &short_result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == BackfillError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData for truncated (shortfall) data, got {:?}",
        short_result.raw_result
    );

    let mut long_data = build_ix_data(&[entry]);
    long_data.push(0xFF);

    let long_ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: long_data,
    };
    let long_result = mollusk.process_instruction(&long_ix, &accounts);
    assert!(
        matches!(
            &long_result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == BackfillError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData for overrun data, got {:?}",
        long_result.raw_result
    );
}

#[test]
fn backfill_balance_account_count_mismatch_rejects() {
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
            chain: 4,
            token_chain: 4,
            token_address: [0x22u8; 32],
            balance: [0xbbu8; 32],
        },
    ];
    let (mut accounts, mut metas) = build_invocation(signer, &entries);
    // wire data still declares count = 2 after this pop
    accounts.pop();
    metas.pop();

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
        "expected InvalidInstructionData for balance_pdas.len() != count, got {:?}",
        result.raw_result
    );
}

#[test]
fn backfill_balance_descending_key_order_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
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
        },
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
        "expected InvalidInstructionData for descending key order, got {:?}",
        result.raw_result
    );
}

#[test]
fn backfill_balance_duplicate_key_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = BalanceEntry {
        chain: 2,
        token_chain: 2,
        token_address: [0x11u8; 32],
        balance: [0xaau8; 32],
    };
    let entries = [entry, entry];
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
        "expected InvalidInstructionData for duplicate key, got {:?}",
        result.raw_result
    );
}

#[test]
fn backfill_balance_non_canonical_pda_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = BalanceEntry {
        chain: 2,
        token_chain: 2,
        token_address: [0x11u8; 32],
        balance: [0xaau8; 32],
    };
    let (mut accounts, mut metas) = build_invocation(signer, &[entry]);

    // Swap the balance PDA (index 2) for an unrelated, non-canonical pubkey.
    let wrong_pda = Pubkey::new_from_array([0x66u8; 32]);
    accounts[2] = (wrong_pda, uninitialised_pda_account());
    metas[2] = AccountMeta::new(wrong_pda, false);

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
        "expected InvalidPda for non-canonical PDA, got {:?}",
        result.raw_result
    );
}

/// Fewer accounts than the handler's `[payer, _system_program, balance_pdas
/// @ ..]` destructure requires (minimum 2) must be rejected with the
/// built-in `NotEnoughAccountKeys`.
// Pinocchio's account destructure raises the deprecated `NotEnoughAccountKeys`, not `MissingAccount`.
#[allow(deprecated)]
#[test]
fn backfill_balance_not_enough_account_keys_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = BalanceEntry {
        chain: 2,
        token_chain: 2,
        token_address: [0x11u8; 32],
        balance: [0xaau8; 32],
    };

    // Only the payer — missing system_program and the balance PDA.
    let accounts: Vec<(Pubkey, Account)> = vec![(signer, signer_account(10_000_000_000))];
    let metas = vec![AccountMeta::new(signer, true)];

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_ix_data(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(&result.raw_result, Err(InstructionError::NotEnoughAccountKeys)),
        "expected NotEnoughAccountKeys, got {:?}",
        result.raw_result
    );
}

/// `count` is a single wire byte (max `255`), but each entry costs one
/// `CreateAccount` CPI and Solana's instruction trace cap
/// (`MAX_INSTRUCTION_TRACE_LENGTH = 64`) limits this to 63 entries per
/// transaction — the largest count this test can actually reach.
#[test]
fn backfill_balance_near_max_count_63_entries() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries: Vec<BalanceEntry> = (0u16..63)
        .map(|i| {
            let mut token_address = [0u8; 32];
            token_address[30..].copy_from_slice(&i.to_be_bytes());
            BalanceEntry {
                chain: 2,
                token_chain: 2,
                token_address,
                balance: [0xaau8; 32],
            }
        })
        .collect();
    let (accounts, metas) = build_invocation(signer, &entries);
    assert_eq!(metas.len(), 2 + 63);

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

    for e in [entries[0], entries[62]] {
        let pda = derive_account_pda(e.chain, e.token_chain, &e.token_address);
        let acc = result.get_account(&pda).unwrap();
        let layout: &BalanceAccountLayout = bytemuck::from_bytes(&acc.data);
        assert_eq!(layout.token_address, e.token_address);
    }
}

/// `Uint256` all-zero round-trips through the wire and on-disk layout untouched.
#[test]
fn backfill_balance_uint256_min_zero_value() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = BalanceEntry {
        chain: 2,
        token_chain: 2,
        token_address: [0x11u8; 32],
        balance: Uint256::ZERO.0,
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
    let acc = result.get_account(&pda).unwrap();
    let layout: &BalanceAccountLayout = bytemuck::from_bytes(&acc.data);
    assert_eq!(layout.balance, Uint256::ZERO);
}

/// `Uint256` all-`0xff` (`2^256 - 1`) round-trips through the wire and on-disk layout untouched.
#[test]
fn backfill_balance_uint256_max_all_ff_value() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = BalanceEntry {
        chain: 2,
        token_chain: 2,
        token_address: [0x11u8; 32],
        balance: Uint256::MAX.0,
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
    let acc = result.get_account(&pda).unwrap();
    let layout: &BalanceAccountLayout = bytemuck::from_bytes(&acc.data);
    assert_eq!(layout.balance, Uint256::MAX);
}

/// A dust-prefunded PDA (lamports > 0, `data_len == 0`, system-owned) takes
/// `init_or_upgrade_pda`'s top-up branch (Transfer + Allocate + Assign) and
/// still succeeds with the correct final state.
#[test]
fn backfill_balance_dust_prefunded_pda_top_up_succeeds() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = BalanceEntry {
        chain: 2,
        token_chain: 2,
        token_address: [0x11u8; 32],
        balance: [0xaau8; 32],
    };
    let (mut accounts, metas) = build_invocation(signer, &[entry]);

    // Pre-fund the balance PDA (index 2) below the rent-exempt minimum.
    let pda = accounts[2].0;
    accounts[2] = (pda, system_owned_account(1_000));

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_ix_data(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "dust pre-funding must not block a legitimate backfill write: raw_result={:?}",
        result.raw_result
    );

    let acc = result.get_account(&pda).unwrap();
    assert_eq!(acc.owner, program_id());
    assert_eq!(acc.data.len(), BalanceAccountLayout::LEN);
    let layout: &BalanceAccountLayout = bytemuck::from_bytes(&acc.data);
    assert_eq!(layout.chain, entry.chain);
    assert_eq!(layout.token_chain, entry.token_chain);
    assert_eq!(layout.token_address, entry.token_address);
    assert_eq!(layout.balance, Uint256::from_be_bytes(entry.balance));
}

/// A PDA already funded at or above the rent-exempt minimum makes
/// `init_or_upgrade_pda`'s `top_up` exactly zero, taking the pure
/// Allocate+Assign branch with no `Transfer` CPI.
#[test]
fn backfill_balance_over_funded_pda_no_transfer_needed() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = BalanceEntry {
        chain: 2,
        token_chain: 2,
        token_address: [0x11u8; 32],
        balance: [0xaau8; 32],
    };
    let (mut accounts, metas) = build_invocation(signer, &[entry]);

    // Pre-fund the balance PDA (index 2) above the rent-exempt minimum.
    const OVER_FUNDED_LAMPORTS: u64 = 10_000_000;
    let pda = accounts[2].0;
    accounts[2] = (pda, system_owned_account(OVER_FUNDED_LAMPORTS));

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_ix_data(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "over-funded prefund must still succeed via Allocate+Assign: raw_result={:?}",
        result.raw_result
    );

    let acc = result.get_account(&pda).unwrap();
    assert_eq!(acc.owner, program_id());
    assert_eq!(acc.data.len(), BalanceAccountLayout::LEN);
    // Allocate/Assign never move lamports.
    assert_eq!(
        acc.lamports, OVER_FUNDED_LAMPORTS,
        "over-funded PDA's lamports must be untouched (top_up == 0 ⇒ no Transfer CPI)"
    );
    let layout: &BalanceAccountLayout = bytemuck::from_bytes(&acc.data);
    assert_eq!(layout.chain, entry.chain);
    assert_eq!(layout.token_chain, entry.token_chain);
    assert_eq!(layout.token_address, entry.token_address);
    assert_eq!(layout.balance, Uint256::from_be_bytes(entry.balance));
}

/// `data_len == 0` owned by neither the system program nor this program:
/// `init_or_upgrade_pda`'s guard (`initial_data_len != 0 ||
/// !initial_owner_is_system`) must reject this before attempting
/// `CreateAccount`/`Allocate`/`Assign`.
#[test]
fn backfill_balance_wrong_owner_zero_data_len_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = BalanceEntry {
        chain: 2,
        token_chain: 2,
        token_address: [0x11u8; 32],
        balance: [0xaau8; 32],
    };
    let (mut accounts, metas) = build_invocation(signer, &[entry]);

    let pda = accounts[2].0;
    accounts[2] = (
        pda,
        Account {
            lamports: 1_000_000,
            data: vec![],
            owner: Pubkey::new_from_array([0x77u8; 32]), // arbitrary non-system, non-self owner
            executable: false,
            rent_epoch: 0,
        },
    );

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
        "expected InvalidPda for data_len==0 with a non-system owner, got {:?}",
        result.raw_result
    );
}

#[test]
fn backfill_balance_zero_length_data_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let (accounts, metas) = build_invocation(signer, &[]);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: vec![IxDiscriminator::BackfillBalance as u8],
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == BackfillError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData for zero-length sub-data, got {:?}",
        result.raw_result
    );
}

/// A trailing, unconsumed balance PDA account beyond what `count` declares is
/// rejected as dead state by the `balance_pdas.len() != count` check.
#[test]
fn backfill_balance_extra_unused_pda_account_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = BalanceEntry {
        chain: 2,
        token_chain: 2,
        token_address: [0x11u8; 32],
        balance: [0xaau8; 32],
    };
    let (mut accounts, mut metas) = build_invocation(signer, &[entry]);
    assert_eq!(metas.len(), 2 + 1, "expected exactly one balance PDA before the extra is added");

    let dead_pda = Pubkey::new_from_array([0x88u8; 32]);
    accounts.push((dead_pda, uninitialised_pda_account()));
    metas.push(AccountMeta::new(dead_pda, false));

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
                if *code == BackfillError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData for extra unused balance PDA account, got {:?}",
        result.raw_result
    );
}
