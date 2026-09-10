//! Integration tests for `BackfillModifyBalance` — writes `ModifyBalance` record PDAs
//! for the wormchain governance modifications the snapshot's balances already reflect.
//! Closes PR 63 Bugbot HIGH finding: without these records, an archived
//! Wormchain-targeted `ModifyBalance` VAA can replay against `global-accountant` and
//! apply its delta a second time.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_backfill::Instruction as IxDiscriminator,
    global_accountant_definitions::{
        BackfillModifyBalanceEntry, GlobalAccountantError, ModificationKind, ModifyBalanceLayout,
        Uint256, MODIFY_BALANCE_SEED_PREFIX,
    },
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult},
    solana_account::Account,
    solana_instruction::{error::InstructionError, AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

use crate::common::*;

fn derive_record_pda(sequence: u64) -> Pubkey {
    let sequence_be = sequence.to_be_bytes();
    let (pda, _) =
        Pubkey::find_program_address(&[MODIFY_BALANCE_SEED_PREFIX, &sequence_be], &program_id());
    pda
}

/// Build `(accounts, metas)` for a BackfillModifyBalance invocation.
/// `signer` parameterised so individual tests can probe wrong-pubkey paths.
fn build_invocation(
    signer: Pubkey,
    entries: &[BackfillModifyBalanceEntry],
) -> (Vec<(Pubkey, Account)>, Vec<AccountMeta>) {
    let (sys_id, sys_acc) = keyed_account_for_system_program();

    let mut accounts: Vec<(Pubkey, Account)> =
        vec![(signer, signer_account(10_000_000_000)), (sys_id, sys_acc)];
    let mut metas: Vec<AccountMeta> = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new_readonly(sys_id, false),
    ];
    for e in entries {
        let pda = derive_record_pda(e.sequence());
        accounts.push((pda, uninitialised_pda_account()));
        metas.push(AccountMeta::new(pda, false));
    }
    (accounts, metas)
}

fn expected_layout(entry: &BackfillModifyBalanceEntry) -> ModifyBalanceLayout {
    let kind = ModificationKind::from_u8(entry.kind).expect("test entry has a valid kind");
    ModifyBalanceLayout::new(
        kind,
        entry.chain_id(),
        entry.token_chain(),
        entry.sequence(),
        entry.token_address,
        entry.amount(),
        entry.reason,
    )
}

// ============================================================================
// Tests
// ============================================================================

/// Single entry: record PDA created at canonical seeds with the supplied fields.
#[test]
fn backfill_modify_balance_single_entry_writes_record() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = modify_balance_entry(
        ModificationKind::Add as u8,
        2,
        2,
        500,
        [0x11u8; 32],
        Uint256::from_u128(1_000_000).0,
        [0x01u8; 32],
    );
    let (accounts, metas) = build_invocation(signer, &[entry]);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_modify_balance_batch(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw_result={:?}",
        result.raw_result
    );

    let pda = derive_record_pda(entry.sequence());
    let acc = result.get_account(&pda).unwrap();
    assert_eq!(acc.data.len(), ModifyBalanceLayout::LEN);
    assert_eq!(acc.owner, program_id());

    let layout: &ModifyBalanceLayout = bytemuck::from_bytes(&acc.data);
    assert_eq!(*layout, expected_layout(&entry));
}

/// Six modification records in one ix (the real migration's record count).
#[test]
fn backfill_modify_balance_bulk_writes_multiple_records() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [
        modify_balance_entry(
            ModificationKind::Add as u8,
            2,
            2,
            100,
            [0x11u8; 32],
            Uint256::from_u128(10).0,
            [0x01u8; 32],
        ),
        modify_balance_entry(
            ModificationKind::Subtract as u8,
            2,
            4,
            101,
            [0x22u8; 32],
            Uint256::from_u128(20).0,
            [0x02u8; 32],
        ),
        modify_balance_entry(
            ModificationKind::Add as u8,
            5,
            5,
            102,
            [0x33u8; 32],
            Uint256::from_u128(30).0,
            [0x03u8; 32],
        ),
        modify_balance_entry(
            ModificationKind::Subtract as u8,
            5,
            2,
            103,
            [0x44u8; 32],
            Uint256::from_u128(40).0,
            [0x04u8; 32],
        ),
        modify_balance_entry(
            ModificationKind::Add as u8,
            8,
            8,
            104,
            [0x55u8; 32],
            Uint256::from_u128(50).0,
            [0x05u8; 32],
        ),
        modify_balance_entry(
            ModificationKind::Add as u8,
            8,
            2,
            105,
            [0x66u8; 32],
            Uint256::from_u128(60).0,
            [0x06u8; 32],
        ),
    ];
    let (accounts, metas) = build_invocation(signer, &entries);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_modify_balance_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw_result={:?}",
        result.raw_result
    );

    for e in &entries {
        let pda = derive_record_pda(e.sequence());
        let acc = result.get_account(&pda).unwrap();
        let layout: &ModifyBalanceLayout = bytemuck::from_bytes(&acc.data);
        assert_eq!(*layout, expected_layout(e));
    }
}

/// Caller signs with a pubkey other than `BACKFILL_AUTHORITY` -> `UnauthorizedCaller`.
#[test]
fn backfill_modify_balance_wrong_signer_rejects() {
    let mollusk = mollusk();
    let wrong_signer = Pubkey::new_from_array([0xDEu8; 32]); // arbitrary, unrelated to BACKFILL_AUTHORITY
    let entry = modify_balance_entry(
        ModificationKind::Add as u8,
        2,
        2,
        500,
        [0x11u8; 32],
        Uint256::from_u128(1_000).0,
        [0x01u8; 32],
    );
    let (accounts, metas) = build_invocation(wrong_signer, &[entry]);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_modify_balance_batch(&[entry]),
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

/// Re-submitting an entry whose record PDA is already owned by the program (a previous
/// backfill tx landed it, or `modify_balance` already recorded this sequence) must
/// hard-fail via `pda_init::create_pda_allow_prefund`'s `data_len != 0 ||
/// !initial_owner_is_system` guard — the same backstop `BackfillBalance` relies on.
#[test]
fn backfill_modify_balance_resubmission_rejected() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = modify_balance_entry(
        ModificationKind::Add as u8,
        2,
        2,
        500,
        [0x11u8; 32],
        Uint256::from_u128(1_000).0,
        [0x01u8; 32],
    );

    // ---------- First submit (fresh PDA) — must succeed ----------
    let (accounts, metas) = build_invocation(signer, &[entry]);
    let ix = Instruction {
        program_id: program_id(),
        accounts: metas.clone(),
        data: encode_modify_balance_batch(&[entry]),
    };
    let first = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        first.program_result,
        ProgramResult::Success,
        "first submit must land cleanly: {:?}",
        first.raw_result
    );

    // ---------- Second submit — pipe the resulting account state back in ----------
    let pda = derive_record_pda(entry.sequence());
    let pda_post = first
        .resulting_accounts
        .iter()
        .find(|(k, _)| k == &pda)
        .map(|(_, a)| a.clone())
        .expect("record PDA in resulting accounts");
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

/// `kind` byte outside `{1, 2}` -> `InvalidModificationKind`, rejected before any write.
#[test]
fn backfill_modify_balance_unknown_kind_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = modify_balance_entry(
        0xFF,
        2,
        2,
        500,
        [0x11u8; 32],
        Uint256::from_u128(1_000).0,
        [0x01u8; 32],
    );
    let (accounts, metas) = build_invocation(signer, &[entry]);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_modify_balance_batch(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == GlobalAccountantError::InvalidModificationKind as u32
        ),
        "expected InvalidModificationKind, got {:?}",
        result.raw_result
    );
}

/// Correct `BACKFILL_AUTHORITY` pubkey, but the meta is not marked as a signer ->
/// Anchor's `AccountNotSigner`, before the handler runs.
#[test]
fn backfill_modify_balance_correct_signer_not_signed_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = modify_balance_entry(
        ModificationKind::Add as u8,
        2,
        2,
        500,
        [0x11u8; 32],
        Uint256::from_u128(1_000).0,
        [0x01u8; 32],
    );
    let (accounts, mut metas) = build_invocation(signer, &[entry]);
    metas[0] = AccountMeta::new(signer, false);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_modify_balance_batch(&[entry]),
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

/// Wire count exceeds the record PDAs supplied -> `InvalidInstructionData`.
#[test]
fn backfill_modify_balance_account_count_mismatch_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [
        modify_balance_entry(
            ModificationKind::Add as u8,
            2,
            2,
            500,
            [0x11u8; 32],
            Uint256::from_u128(10).0,
            [0x01u8; 32],
        ),
        modify_balance_entry(
            ModificationKind::Subtract as u8,
            4,
            4,
            501,
            [0x22u8; 32],
            Uint256::from_u128(20).0,
            [0x02u8; 32],
        ),
    ];
    let (mut accounts, mut metas) = build_invocation(signer, &entries);
    // wire data still declares count = 2 after this pop
    accounts.pop();
    metas.pop();

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_modify_balance_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == GlobalAccountantError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData for record_pdas.len() != count, got {:?}",
        result.raw_result
    );
}

/// A record PDA beyond the wire count is dead state the payer funds rent for ->
/// `InvalidInstructionData`.
#[test]
fn backfill_modify_balance_extra_unused_pda_account_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = modify_balance_entry(
        ModificationKind::Add as u8,
        2,
        2,
        500,
        [0x11u8; 32],
        Uint256::from_u128(1_000).0,
        [0x01u8; 32],
    );
    let (mut accounts, mut metas) = build_invocation(signer, &[entry]);
    assert_eq!(
        metas.len(),
        2 + 1,
        "expected exactly one record PDA before the extra is added"
    );

    let dead_pda = Pubkey::new_from_array([0x88u8; 32]);
    accounts.push((dead_pda, uninitialised_pda_account()));
    metas.push(AccountMeta::new(dead_pda, false));

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_modify_balance_batch(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == GlobalAccountantError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData for extra unused record PDA account, got {:?}",
        result.raw_result
    );
}

/// `ModifyBalanceBatch::parse` requires strictly ascending sequences. Descending entries
/// -> `InvalidInstructionData`, before any PDA is touched.
#[test]
fn backfill_modify_balance_descending_sequence_order_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [
        modify_balance_entry(
            ModificationKind::Add as u8,
            2,
            2,
            501,
            [0x11u8; 32],
            Uint256::from_u128(10).0,
            [0x01u8; 32],
        ),
        modify_balance_entry(
            ModificationKind::Add as u8,
            2,
            2,
            500,
            [0x22u8; 32],
            Uint256::from_u128(20).0,
            [0x02u8; 32],
        ),
    ];
    let (accounts, metas) = build_invocation(signer, &entries);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_modify_balance_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == GlobalAccountantError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData for descending sequence order, got {:?}",
        result.raw_result
    );
}

/// A duplicate sequence is not strictly ascending -> `InvalidInstructionData`. Guards the
/// double-record path the migration must never take.
#[test]
fn backfill_modify_balance_duplicate_sequence_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = modify_balance_entry(
        ModificationKind::Add as u8,
        2,
        2,
        500,
        [0x11u8; 32],
        Uint256::from_u128(10).0,
        [0x01u8; 32],
    );
    let entries = [entry, entry];
    let (accounts, metas) = build_invocation(signer, &entries);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_modify_balance_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == GlobalAccountantError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData for duplicate sequence, got {:?}",
        result.raw_result
    );
}

/// A record PDA at a pubkey that is not the canonical derivation -> `InvalidPda`.
#[test]
fn backfill_modify_balance_non_canonical_pda_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = modify_balance_entry(
        ModificationKind::Add as u8,
        2,
        2,
        500,
        [0x11u8; 32],
        Uint256::from_u128(1_000).0,
        [0x01u8; 32],
    );
    let (mut accounts, mut metas) = build_invocation(signer, &[entry]);

    // Swap the record PDA (index 2) for an unrelated, non-canonical pubkey.
    let wrong_pda = Pubkey::new_from_array([0x66u8; 32]);
    accounts[2] = (wrong_pda, uninitialised_pda_account());
    metas[2] = AccountMeta::new(wrong_pda, false);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_modify_balance_batch(&[entry]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == GlobalAccountantError::InvalidPda as u32
        ),
        "expected InvalidPda for non-canonical record PDA, got {:?}",
        result.raw_result
    );
}

/// Payer alone, with system_program and the record PDA absent -> Anchor's
/// `AccountNotEnoughKeys`.
#[test]
fn backfill_modify_balance_not_enough_account_keys_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = modify_balance_entry(
        ModificationKind::Add as u8,
        2,
        2,
        500,
        [0x11u8; 32],
        Uint256::from_u128(1_000).0,
        [0x01u8; 32],
    );

    let accounts: Vec<(Pubkey, Account)> = vec![(signer, signer_account(10_000_000_000))];
    let metas = vec![AccountMeta::new(signer, true)];

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_modify_balance_batch(&[entry]),
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

/// Discriminator with no batch body -> `InvalidInstructionData`.
#[test]
fn backfill_modify_balance_zero_length_data_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let (accounts, metas) = build_invocation(signer, &[]);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: vec![IxDiscriminator::BackfillModifyBalance as u8],
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

/// Dust pre-funding a record PDA must not block the write. `create_pda_allow_prefund`
/// tops it up to rent-exemption instead of failing, which is what makes the migration
/// resistant to a cheap griefing transfer.
#[test]
fn backfill_modify_balance_dust_prefunded_pda_top_up_succeeds() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entry = modify_balance_entry(
        ModificationKind::Add as u8,
        2,
        2,
        500,
        [0x11u8; 32],
        Uint256::from_u128(1_000).0,
        [0x01u8; 32],
    );
    let (mut accounts, metas) = build_invocation(signer, &[entry]);

    let pda = accounts[2].0;
    accounts[2] = (pda, system_owned_account(1_000));

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_modify_balance_batch(&[entry]),
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
    assert_eq!(acc.data.len(), ModifyBalanceLayout::LEN);
    let layout: &ModifyBalanceLayout = bytemuck::from_bytes(&acc.data);
    assert_eq!(*layout, expected_layout(&entry));
}

/// 58 records in one ix, matching `BackfillBalance`'s near-max case. Bounds the
/// per-transaction account list the migration tooling may build.
#[test]
fn backfill_modify_balance_near_max_count_58_entries() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries: Vec<BackfillModifyBalanceEntry> = (0u64..58)
        .map(|i| {
            modify_balance_entry(
                ModificationKind::Add as u8,
                2,
                2,
                1_000 + i,
                [0x11u8; 32],
                Uint256::from_u128(u128::from(i) + 1).0,
                [0x01u8; 32],
            )
        })
        .collect();
    let (accounts, metas) = build_invocation(signer, &entries);
    assert_eq!(metas.len(), 2 + 58);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_modify_balance_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw_result={:?}",
        result.raw_result
    );

    for e in [entries[0], entries[57]] {
        let pda = derive_record_pda(e.sequence());
        let acc = result.get_account(&pda).unwrap();
        let layout: &ModifyBalanceLayout = bytemuck::from_bytes(&acc.data);
        assert_eq!(*layout, expected_layout(&e));
    }
}

/// Edge values recorded verbatim. `BackfillModifyBalance` stores a record and applies no
/// arithmetic, so every edge must survive the round trip unchanged.
#[test]
fn backfill_modify_balance_edge_values_round_trip() {
    // (label, kind, chain_id, token_chain, sequence, amount)
    // TODO(human): fill in the edge cases worth pinning for a record-only instruction.
    let cases: &[(&str, u8, u16, u16, u64, Uint256)] = &[];

    assert!(
        !cases.is_empty(),
        "edge-value table must not be empty: iterating an empty table asserts nothing"
    );

    let mollusk = mollusk();
    let signer = test_authority_pubkey();

    for (label, kind, chain_id, token_chain, sequence, amount) in cases {
        let entry = modify_balance_entry(
            *kind,
            *chain_id,
            *token_chain,
            *sequence,
            [0x11u8; 32],
            amount.0,
            [0x01u8; 32],
        );
        let (accounts, metas) = build_invocation(signer, &[entry]);

        let ix = Instruction {
            program_id: program_id(),
            accounts: metas,
            data: encode_modify_balance_batch(&[entry]),
        };
        let result = mollusk.process_instruction(&ix, &accounts);
        assert_eq!(
            result.program_result,
            ProgramResult::Success,
            "{label}: raw_result={:?}",
            result.raw_result
        );

        let pda = derive_record_pda(*sequence);
        let acc = result.get_account(&pda).unwrap();
        let layout: &ModifyBalanceLayout = bytemuck::from_bytes(&acc.data);
        assert_eq!(*layout, expected_layout(&entry), "{label}");
    }
}
