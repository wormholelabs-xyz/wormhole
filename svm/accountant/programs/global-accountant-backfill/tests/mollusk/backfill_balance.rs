//! Integration tests for `BackfillBalance` — bulk-batched `BalanceAccountLayout`
//! writes from a wormchain `query_all_accounts` row.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_backfill::Instruction as IxDiscriminator,
    global_accountant_definitions::{
        BackfillBalanceEntry, BalanceAccountLayout, GlobalAccountantError, Uint256,
        ACCOUNT_SEED_PREFIX,
    },
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult},
    solana_account::Account,
    solana_instruction::{error::InstructionError, AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

use crate::common::*;

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

/// Build `(accounts, metas)` for a BackfillBalance invocation.
/// `signer` parameterised so individual tests can probe wrong-pubkey paths.
fn build_invocation(
    signer: Pubkey,
    entries: &[BackfillBalanceEntry],
) -> (Vec<(Pubkey, Account)>, Vec<AccountMeta>) {
    let (sys_id, sys_acc) = keyed_account_for_system_program();

    let mut accounts: Vec<(Pubkey, Account)> =
        vec![(signer, signer_account(10_000_000_000)), (sys_id, sys_acc)];
    let mut metas: Vec<AccountMeta> = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new_readonly(sys_id, false),
    ];
    for e in entries {
        let pda = derive_account_pda(e.chain(), e.token_chain(), &e.token_address);
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
    let entry = balance_entry(2, 2, [0x11u8; 32], Uint256::from_u128(256).0);
    let (accounts, metas) = build_invocation(signer, &[entry]);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_balance_batch(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw_result={:?}",
        result.raw_result
    );

    let pda = derive_account_pda(entry.chain(), entry.token_chain(), &entry.token_address);
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
    assert_eq!(layout.chain, entry.chain());
    assert_eq!(layout.token_chain, entry.token_chain());
    assert_eq!(layout.token_address, entry.token_address);
    assert_eq!(layout.balance, entry.balance());
}

/// Three distinct balances in one ix.
#[test]
fn backfill_balance_bulk_writes_multiple_pdas() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [
        balance_entry(2, 2, [0x11u8; 32], [0xaau8; 32]),
        balance_entry(2, 4, [0x22u8; 32], [0xbbu8; 32]),
        balance_entry(5, 5, [0x33u8; 32], [0xccu8; 32]),
    ];
    let (accounts, metas) = build_invocation(signer, &entries);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_balance_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw_result={:?}",
        result.raw_result
    );

    for e in &entries {
        let pda = derive_account_pda(e.chain(), e.token_chain(), &e.token_address);
        let acc = result.get_account(&pda).unwrap();
        let layout: &BalanceAccountLayout = bytemuck::from_bytes(&acc.data);
        assert_eq!(layout.chain, e.chain());
        assert_eq!(layout.token_chain, e.token_chain());
        assert_eq!(layout.token_address, e.token_address);
        assert_eq!(layout.balance, e.balance());
    }
}

/// Re-submitting an entry whose PDA is already owned by the program (a
/// previous backfill tx landed it) must hard-fail. `BackfillNoReplay` gets
/// idempotency for free via `solana-noreplay`'s `AlreadyAccounted`
/// short-circuit; `BackfillBalance` relies on
/// `pda_init::create_pda_allow_prefund`'s `data_len != 0 ||
/// !initial_owner_is_system` guard instead. The orchestrator's cursor
/// normally prevents re-submission; this guard is the backstop for a
/// lagging cursor or a manual re-run.
#[test]
fn backfill_balance_resubmission_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = balance_entry(2, 2, [0x11u8; 32], [0xaau8; 32]);

    // ---------- First submit (fresh PDA) — must succeed ----------
    let (accounts, metas) = build_invocation(signer, &[entry]);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas.clone(),
        data: encode_balance_batch(&[entry]),
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
    let pda = derive_account_pda(entry.chain(), entry.token_chain(), &entry.token_address);
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
                if *code == GlobalAccountantError::InvalidPda as u32
        ),
        "expected InvalidPda on re-submission, got {:?}",
        second.raw_result
    );
}

/// Caller signs with a pubkey other than `BACKFILL_AUTHORITY` → `UnauthorizedCaller`.
#[test]
fn backfill_balance_wrong_signer_rejects() {
    let mollusk = mollusk();
    let wrong_signer = Pubkey::new_from_array([0xDEu8; 32]); // arbitrary, unrelated to BACKFILL_AUTHORITY
    let entry = balance_entry(2, 2, [0x11u8; 32], [0xaau8; 32]);
    let (accounts, metas) = build_invocation(wrong_signer, &[entry]);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_balance_batch(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == GlobalAccountantError::UnauthorizedCaller as u32
        ),
        "expected UnauthorizedCaller, got {:?}",
        result.raw_result
    );
}

/// Under Anchor, `payer`'s `Signer<'info>` wrapper enforces `is_signer`
/// during `try_accounts`, ahead of
/// `accountant_backfill_core::support::authority::require_authority`'s
/// own check. Rejection surfaces as Anchor's `AccountNotSigner` (3010).
#[test]
fn backfill_balance_correct_signer_not_signed_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = balance_entry(2, 2, [0x11u8; 32], [0xaau8; 32]);
    let (accounts, mut metas) = build_invocation(signer, &[entry]);
    metas[0] = AccountMeta::new(signer, false);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_balance_batch(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == anchor_lang::error::ErrorCode::AccountNotSigner as u32
        ),
        "expected Anchor's AccountNotSigner, got {:?}",
        result.raw_result
    );
}

/// Account-level check: `balance_pdas.len() != count` shortfall, verified against the
/// deployed `.so`. Wire-only framing/ordering cases live in
/// `crates/definitions/src/instructions/backfill.rs`.
#[test]
fn backfill_balance_account_count_mismatch_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [
        balance_entry(2, 2, [0x11u8; 32], [0xaau8; 32]),
        balance_entry(4, 4, [0x22u8; 32], [0xbbu8; 32]),
    ];
    let (mut accounts, mut metas) = build_invocation(signer, &entries);
    // wire data still declares count = 2 after this pop
    accounts.pop();
    metas.pop();

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_balance_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == GlobalAccountantError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData for balance_pdas.len() != count, got {:?}",
        result.raw_result
    );
}

/// Malformed-wire e2e proof #1: descending key order rejects through the
/// deployed `.so`. Matches `balance_batch_rejects_malformed_wire`'s
/// "descending key" case in `crates/definitions/src/instructions/backfill.rs`.
#[test]
fn backfill_balance_descending_key_order_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [
        balance_entry(5, 5, [0x33u8; 32], [0xaau8; 32]),
        balance_entry(2, 2, [0x11u8; 32], [0xbbu8; 32]),
    ];
    let (accounts, metas) = build_invocation(signer, &entries);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_balance_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == GlobalAccountantError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData for descending key order, got {:?}",
        result.raw_result
    );
}

#[test]
fn backfill_balance_non_canonical_pda_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = balance_entry(2, 2, [0x11u8; 32], [0xaau8; 32]);
    let (mut accounts, mut metas) = build_invocation(signer, &[entry]);

    // Swap the balance PDA (index 2) for an unrelated, non-canonical pubkey.
    let wrong_pda = Pubkey::new_from_array([0x66u8; 32]);
    accounts[2] = (wrong_pda, uninitialised_pda_account());
    metas[2] = AccountMeta::new(wrong_pda, false);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_balance_batch(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == GlobalAccountantError::InvalidPda as u32
        ),
        "expected InvalidPda for non-canonical PDA, got {:?}",
        result.raw_result
    );
}

/// Fewer accounts than `BackfillBalanceAccounts` requires (the fixed
/// `payer` + `system_program` pair, before `ctx.remaining_accounts`) must
/// be rejected. Anchor's `Accounts::try_accounts` raises
/// `AccountNotEnoughKeys` (3005) ahead of the handler's own
/// `[payer, _system_program, balance_pdas @ ..]` destructure.
#[test]
fn backfill_balance_not_enough_account_keys_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = balance_entry(2, 2, [0x11u8; 32], [0xaau8; 32]);

    // Only the payer — missing system_program and the balance PDA.
    let accounts: Vec<(Pubkey, Account)> = vec![(signer, signer_account(10_000_000_000))];
    let metas = vec![AccountMeta::new(signer, true)];

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_balance_batch(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == anchor_lang::error::ErrorCode::AccountNotEnoughKeys as u32
        ),
        "expected Anchor's AccountNotEnoughKeys, got {:?}",
        result.raw_result
    );
}

/// `count` is a single wire byte (max `255`). The binding constraint is the
/// default 32 KiB BPF heap: Anchor's `Accounts`/`Context` machinery plus
/// `create_account_allow_prefund`'s heap-allocated `Vec<AccountMeta>`/
/// bincode buffer accumulate on the bump allocator across sequential CPIs
/// within one instruction. Measured empirically (binary search): 58
/// entries succeed, 59 hits "memory allocation failed, out of memory".
#[test]
fn backfill_balance_near_max_count_58_entries() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries: Vec<BackfillBalanceEntry> = (0u16..58)
        .map(|i| {
            let mut token_address = [0u8; 32];
            token_address[30..].copy_from_slice(&i.to_be_bytes());
            balance_entry(2, 2, token_address, [0xaau8; 32])
        })
        .collect();
    let (accounts, metas) = build_invocation(signer, &entries);
    assert_eq!(metas.len(), 2 + 58);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_balance_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw_result={:?}",
        result.raw_result
    );

    for e in [entries[0], entries[57]] {
        let pda = derive_account_pda(e.chain(), e.token_chain(), &e.token_address);
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
    let entry = balance_entry(2, 2, [0x11u8; 32], Uint256::ZERO.0);
    let (accounts, metas) = build_invocation(signer, &[entry]);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_balance_batch(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw_result={:?}",
        result.raw_result
    );

    let pda = derive_account_pda(entry.chain(), entry.token_chain(), &entry.token_address);
    let acc = result.get_account(&pda).unwrap();
    let layout: &BalanceAccountLayout = bytemuck::from_bytes(&acc.data);
    assert_eq!(layout.balance, Uint256::ZERO);
}

/// `Uint256` all-`0xff` (`2^256 - 1`) round-trips through the wire and on-disk layout untouched.
#[test]
fn backfill_balance_uint256_max_all_ff_value() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = balance_entry(2, 2, [0x11u8; 32], Uint256::MAX.0);
    let (accounts, metas) = build_invocation(signer, &[entry]);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_balance_batch(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw_result={:?}",
        result.raw_result
    );

    let pda = derive_account_pda(entry.chain(), entry.token_chain(), &entry.token_address);
    let acc = result.get_account(&pda).unwrap();
    let layout: &BalanceAccountLayout = bytemuck::from_bytes(&acc.data);
    assert_eq!(layout.balance, Uint256::MAX);
}

/// A dust-prefunded PDA (lamports > 0, `data_len == 0`, system-owned) takes
/// `create_pda_allow_prefund`'s `CreateAccountAllowPrefund` top-up path and still
/// succeeds with the correct final state.
#[test]
fn backfill_balance_dust_prefunded_pda_top_up_succeeds() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = balance_entry(2, 2, [0x11u8; 32], [0xaau8; 32]);
    let (mut accounts, metas) = build_invocation(signer, &[entry]);

    // Pre-fund the balance PDA (index 2) below the rent-exempt minimum.
    let pda = accounts[2].0;
    accounts[2] = (pda, system_owned_account(1_000));

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_balance_batch(&[entry]),
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
    assert_eq!(layout.chain, entry.chain());
    assert_eq!(layout.token_chain, entry.token_chain());
    assert_eq!(layout.token_address, entry.token_address);
    assert_eq!(layout.balance, entry.balance());
}

/// A PDA already funded at or above the rent-exempt minimum makes
/// `create_pda_allow_prefund`'s `top_up` exactly zero, taking
/// `CreateAccountAllowPrefund`'s zero-`top_up` path; lamports stay fixed.
#[test]
fn backfill_balance_over_funded_pda_no_transfer_needed() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = balance_entry(2, 2, [0x11u8; 32], [0xaau8; 32]);
    let (mut accounts, metas) = build_invocation(signer, &[entry]);

    // Pre-fund the balance PDA (index 2) above the rent-exempt minimum.
    const OVER_FUNDED_LAMPORTS: u64 = 10_000_000;
    let pda = accounts[2].0;
    accounts[2] = (pda, system_owned_account(OVER_FUNDED_LAMPORTS));

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_balance_batch(&[entry]),
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
    // Allocate/Assign leave lamports fixed.
    assert_eq!(
        acc.lamports, OVER_FUNDED_LAMPORTS,
        "over-funded PDA's lamports must be untouched (top_up == 0 ⇒ no Transfer CPI)"
    );
    let layout: &BalanceAccountLayout = bytemuck::from_bytes(&acc.data);
    assert_eq!(layout.chain, entry.chain());
    assert_eq!(layout.token_chain, entry.token_chain());
    assert_eq!(layout.token_address, entry.token_address);
    assert_eq!(layout.balance, entry.balance());
}

/// `data_len == 0` owned by neither the system program nor this program:
/// `create_pda_allow_prefund`'s guard (`initial_data_len != 0 ||
/// !initial_owner_is_system`) must reject this before attempting
/// `CreateAccount`/`Allocate`/`Assign`.
#[test]
fn backfill_balance_wrong_owner_zero_data_len_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = balance_entry(2, 2, [0x11u8; 32], [0xaau8; 32]);
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
        data: encode_balance_batch(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == GlobalAccountantError::InvalidPda as u32
        ),
        "expected InvalidPda for data_len==0 with a non-system owner, got {:?}",
        result.raw_result
    );
}

/// Malformed-wire e2e proof #2: instruction data ending before the `count`
/// byte rejects through the deployed `.so`. Matches
/// `balance_batch_rejects_malformed_wire`'s "empty data" case in
/// `crates/definitions/src/instructions/backfill.rs`.
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
                if *code == GlobalAccountantError::InvalidInstructionData as u32
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
    let entry = balance_entry(2, 2, [0x11u8; 32], [0xaau8; 32]);
    let (mut accounts, mut metas) = build_invocation(signer, &[entry]);
    assert_eq!(
        metas.len(),
        2 + 1,
        "expected exactly one balance PDA before the extra is added"
    );

    let dead_pda = Pubkey::new_from_array([0x88u8; 32]);
    accounts.push((dead_pda, uninitialised_pda_account()));
    metas.push(AccountMeta::new(dead_pda, false));

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_balance_batch(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == GlobalAccountantError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData for extra unused balance PDA account, got {:?}",
        result.raw_result
    );
}
