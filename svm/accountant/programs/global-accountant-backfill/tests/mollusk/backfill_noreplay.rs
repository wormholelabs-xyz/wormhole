//! Integration tests for `BackfillNoReplay` driven against the real
//! `solana_noreplay.so` co-deployed in mollusk.
//!
//! Contract under test:
//! - Signer pubkey must equal the compile-time `BACKFILL_AUTHORITY` const.
//! - Entries strictly ascending by `(chain, emitter, sequence)`.
//! - Caller passes one bucket account per unique
//!   `(chain, emitter, sequence / 1024)` in entry order.
//! - Handler CPIs `MarkUsedBulk` once per bucket.
//! - Each entry emits one canonical `ACCDGST\0` commit-log payload.

#![allow(clippy::too_many_arguments)]

use {
    accountant_operational_core::cpi::noreplay::derive_bucket_pda,
    global_accountant_backfill::{Instruction as IxDiscriminator, BACKFILL_AUTHORITY},
    global_accountant_definitions::{
        GlobalAccountantError, NoReplayBitmapAccount, NOREPLAY_AUTHORITY_SEED_PREFIX,
        NOREPLAY_PROGRAM_ID,
    },
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult},
    solana_account::Account,
    solana_instruction::{error::InstructionError, AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

mod common;
use common::{
    keyed_account_for_noreplay_program,
    mollusk::{
        mollusk, program_id, signer_account, test_authority_pubkey, uninitialised_pda_account,
    },
    wire::{encode_noreplay_batch, encode_noreplay_batch_raw, NoReplayEntry as Entry},
};

// ============================================================================
// PDA derivations
// ============================================================================

fn derive_noreplay_authority_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id())
}

fn derive_noreplay_bucket(
    authority: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> Pubkey {
    derive_bucket_pda(authority, chain, emitter, sequence).0
}

fn unique_buckets_in_order(noreplay_authority: &Pubkey, entries: &[Entry]) -> Vec<Pubkey> {
    let mut out: Vec<Pubkey> = Vec::new();
    let mut prev_key: Option<(u16, [u8; 32], u64)> = None;
    for e in entries {
        let cur_key = (
            e.chain,
            e.emitter,
            NoReplayBitmapAccount::bucket_index(e.sequence),
        );
        if prev_key != Some(cur_key) {
            out.push(derive_noreplay_bucket(
                noreplay_authority,
                e.chain,
                &e.emitter,
                e.sequence,
            ));
            prev_key = Some(cur_key);
        }
    }
    out
}

/// Build `(accounts, metas)` for a BackfillNoReplay invocation.
/// `signer` parameterised so individual tests can probe wrong-pubkey paths.
fn build_invocation(
    signer: Pubkey,
    entries: &[Entry],
) -> (Vec<(Pubkey, Account)>, Vec<AccountMeta>) {
    let (noreplay_auth_pda, _) = derive_noreplay_authority_pda();
    let buckets = unique_buckets_in_order(&noreplay_auth_pda, entries);

    let (np_id, np_acc) = keyed_account_for_noreplay_program();
    let (sys_id, sys_acc) = keyed_account_for_system_program();

    let mut accounts: Vec<(Pubkey, Account)> = vec![
        (signer, signer_account(10_000_000_000)),
        (np_id, np_acc),
        (noreplay_auth_pda, uninitialised_pda_account()),
        (sys_id, sys_acc),
    ];
    let mut metas: Vec<AccountMeta> = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new_readonly(np_id, false),
        AccountMeta::new_readonly(noreplay_auth_pda, false),
        AccountMeta::new_readonly(sys_id, false),
    ];
    for bucket in &buckets {
        accounts.push((*bucket, uninitialised_pda_account()));
        metas.push(AccountMeta::new(*bucket, false));
    }
    (accounts, metas)
}

fn assert_bit_set(account: &Account, sequence: u64) {
    let bitmap = NoReplayBitmapAccount::from_bytes(&account.data).expect("bitmap account");
    assert!(
        bitmap.is_marked(sequence),
        "expected bit for {sequence} set"
    );
}

// ============================================================================
// Tests
// ============================================================================

/// Sanity check: the compile-time `BACKFILL_AUTHORITY` const matches the
/// deterministic test keypair's pubkey. A failure here means the const was
/// rebuilt against a real operator key — likely committed by mistake.
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

#[test]
fn backfill_noreplay_single_entry_flips_bit() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [Entry {
        chain: 2,
        emitter: [0x11u8; 32],
        sequence: 42,
        digest: [0x77u8; 32],
    }];
    let (accounts, metas) = build_invocation(signer, &entries);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_noreplay_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw_result={:?}",
        result.raw_result
    );

    let (np_auth, _) = derive_noreplay_authority_pda();
    let bucket = derive_noreplay_bucket(&np_auth, 2, &[0x11u8; 32], 42);
    let bucket_acc = result
        .resulting_accounts
        .iter()
        .find(|(k, _)| k == &bucket)
        .unwrap()
        .1
        .clone();
    assert_bit_set(&bucket_acc, 42);
}

#[test]
fn backfill_noreplay_multiple_entries_same_bucket_one_cpi() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let emitter = [0x22u8; 32];
    let entries = [
        Entry {
            chain: 2,
            emitter,
            sequence: 10,
            digest: [0xaau8; 32],
        },
        Entry {
            chain: 2,
            emitter,
            sequence: 200,
            digest: [0xbbu8; 32],
        },
        Entry {
            chain: 2,
            emitter,
            sequence: 800,
            digest: [0xccu8; 32],
        },
    ];
    let (accounts, metas) = build_invocation(signer, &entries);
    // Sanity: 4 fixed slots + 1 bucket.
    assert_eq!(metas.len(), 4 + 1);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_noreplay_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw_result={:?}",
        result.raw_result
    );

    let (np_auth, _) = derive_noreplay_authority_pda();
    let bucket = derive_noreplay_bucket(&np_auth, 2, &emitter, 0);
    let bucket_acc = result
        .resulting_accounts
        .iter()
        .find(|(k, _)| k == &bucket)
        .unwrap()
        .1
        .clone();
    assert_bit_set(&bucket_acc, 10);
    assert_bit_set(&bucket_acc, 200);
    assert_bit_set(&bucket_acc, 800);
}

#[test]
fn backfill_noreplay_multiple_entries_different_buckets() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let emitter = [0x33u8; 32];
    let entries = [
        Entry {
            chain: 2,
            emitter,
            sequence: 10,
            digest: [0xaau8; 32],
        },
        Entry {
            chain: 2,
            emitter,
            sequence: 500,
            digest: [0xbbu8; 32],
        },
        Entry {
            chain: 2,
            emitter,
            sequence: 1500,
            digest: [0xccu8; 32],
        },
    ];
    let (accounts, metas) = build_invocation(signer, &entries);
    assert_eq!(metas.len(), 4 + 2, "expected two unique buckets");

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_noreplay_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw_result={:?}",
        result.raw_result
    );

    let (np_auth, _) = derive_noreplay_authority_pda();
    let bucket_0 = derive_noreplay_bucket(&np_auth, 2, &emitter, 0);
    let bucket_1 = derive_noreplay_bucket(&np_auth, 2, &emitter, 1500);

    let b0 = result
        .resulting_accounts
        .iter()
        .find(|(k, _)| k == &bucket_0)
        .unwrap()
        .1
        .clone();
    assert_bit_set(&b0, 10);
    assert_bit_set(&b0, 500);

    let b1 = result
        .resulting_accounts
        .iter()
        .find(|(k, _)| k == &bucket_1)
        .unwrap()
        .1
        .clone();
    assert_bit_set(&b1, 1500);
}

/// Caller signs with a pubkey other than `BACKFILL_AUTHORITY`. Must reject
/// with `UnauthorizedCaller` — the const-check is the only thing standing
/// between an attacker and arbitrary state writes.
#[test]
fn backfill_noreplay_wrong_signer_rejects() {
    let mollusk = mollusk();
    let wrong_signer = Pubkey::new_from_array([0xDEu8; 32]); // arbitrary, unrelated to BACKFILL_AUTHORITY
    let entries = [Entry {
        chain: 2,
        emitter: [0x11u8; 32],
        sequence: 42,
        digest: [0x77u8; 32],
    }];
    let (accounts, metas) = build_invocation(wrong_signer, &entries);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_noreplay_batch(&entries),
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

/// Correct `BACKFILL_AUTHORITY` pubkey but not signed. Under Anchor,
/// `payer`'s `Signer<'info>` wrapper enforces `is_signer` during
/// `try_accounts`, ahead of
/// `accountant_operational_core::support::authority::require_authority`'s
/// own check. Rejection surfaces as Anchor's `AccountNotSigner` (3010).
#[test]
fn backfill_noreplay_correct_signer_not_signed_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [Entry {
        chain: 2,
        emitter: [0x11u8; 32],
        sequence: 42,
        digest: [0x77u8; 32],
    }];
    let (accounts, mut metas) = build_invocation(signer, &entries);
    // Correct pubkey, signer flag flipped off.
    metas[0] = AccountMeta::new(signer, false);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_noreplay_batch(&entries),
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

/// A `noreplay_authority` account that does not match the derived
/// `[NOREPLAY_AUTHORITY_SEED_PREFIX]` PDA must be rejected with `InvalidPda`
/// before any CPI is attempted.
#[test]
fn backfill_noreplay_spoofed_noreplay_authority_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [Entry {
        chain: 2,
        emitter: [0x11u8; 32],
        sequence: 42,
        digest: [0x77u8; 32],
    }];
    let (mut accounts, mut metas) = build_invocation(signer, &entries);

    // Swap the noreplay_authority slot (index 2) for an unrelated pubkey.
    let spoofed_authority = Pubkey::new_from_array([0x55u8; 32]);
    accounts[2] = (spoofed_authority, uninitialised_pda_account());
    metas[2] = AccountMeta::new_readonly(spoofed_authority, false);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_noreplay_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == GlobalAccountantError::InvalidPda as u32
        ),
        "expected InvalidPda, got {:?}",
        result.raw_result
    );
}

/// Malformed-wire e2e proof #1: instruction data ending before the
/// `group_count` byte rejects through the deployed `.so`. Matches
/// `noreplay_batch_rejects_malformed_wire`'s "empty data" case in
/// `crates/definitions/src/instructions/backfill.rs`.
#[test]
fn backfill_noreplay_zero_length_data_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let (accounts, metas) = build_invocation(signer, &[]);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: vec![IxDiscriminator::BackfillNoReplay as u8],
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

/// Malformed-wire e2e proof #2: a second group with a lower chain than the
/// first rejects through the deployed `.so`. Matches
/// `noreplay_batch_rejects_malformed_wire`'s "descending group key" case in
/// `crates/definitions/src/instructions/backfill.rs`.
#[test]
fn backfill_noreplay_descending_group_order_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let (accounts, metas) = build_invocation(signer, &[]);

    let group_a_entries = [(1u64, [0xaau8; 32])];
    let group_b_entries = [(1u64, [0xbbu8; 32])];
    let data = encode_noreplay_batch_raw(&[
        (5u16, [0x11u8; 32], &group_a_entries), // chain 5 first
        (2u16, [0x22u8; 32], &group_b_entries), // chain 2 second — descending
    ]);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data,
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == GlobalAccountantError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData for descending group order, got {:?}",
        result.raw_result
    );
}

/// Three entries across three buckets (sequences 0/1500/3000) but only one
/// bucket account supplied — the shortfall must surface mid-loop, on the
/// transition into the third bucket.
#[test]
fn backfill_noreplay_too_few_bucket_accounts_mid_flush_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let emitter = [0x44u8; 32];
    let entries = [
        Entry {
            chain: 2,
            emitter,
            sequence: 0,
            digest: [0xaau8; 32],
        },
        Entry {
            chain: 2,
            emitter,
            sequence: 1500,
            digest: [0xbbu8; 32],
        },
        Entry {
            chain: 2,
            emitter,
            sequence: 3000,
            digest: [0xccu8; 32],
        },
    ];
    let (mut accounts, mut metas) = build_invocation(signer, &entries);
    assert_eq!(
        metas.len(),
        4 + 3,
        "expected three unique buckets before truncation"
    );
    // Keep only the first bucket account (index 4); drop buckets 1 and 2.
    accounts.truncate(4 + 1);
    metas.truncate(4 + 1);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_noreplay_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == GlobalAccountantError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData for too-few bucket accounts mid-flush, got {:?}",
        result.raw_result
    );
}

/// A trailing, unconsumed bucket account beyond what the walk needs is
/// rejected as dead state (potential rent-griefing) by the exact-count check
/// after the final flush.
#[test]
fn backfill_noreplay_extra_unused_bucket_account_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [Entry {
        chain: 2,
        emitter: [0x11u8; 32],
        sequence: 42,
        digest: [0x77u8; 32],
    }];
    let (mut accounts, mut metas) = build_invocation(signer, &entries);
    assert_eq!(
        metas.len(),
        4 + 1,
        "expected exactly one bucket before the extra is added"
    );

    // Dead-state extra bucket account, beyond what the walk consumes.
    let dead_bucket = Pubkey::new_from_array([0x99u8; 32]);
    accounts.push((dead_bucket, uninitialised_pda_account()));
    metas.push(AccountMeta::new(dead_bucket, false));

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_noreplay_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == GlobalAccountantError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData for dead-state extra bucket account, got {:?}",
        result.raw_result
    );
}

/// Fewer accounts than `BackfillNoReplayAccounts` requires (the fixed
/// `payer`, `noreplay_program`, `noreplay_authority`, `system_program`
/// quartet, before `ctx.remaining_accounts`) must be rejected. Anchor's
/// `Accounts::try_accounts` raises `AccountNotEnoughKeys` (3005) ahead of
/// the handler's own `[payer, noreplay_program, noreplay_authority,
/// system_program, buckets @ ..]` destructure.
#[test]
fn backfill_noreplay_not_enough_account_keys_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [Entry {
        chain: 2,
        emitter: [0x11u8; 32],
        sequence: 42,
        digest: [0x77u8; 32],
    }];
    let (np_id, np_acc) = keyed_account_for_noreplay_program();

    // Only 2 of the required 4 fixed accounts.
    let accounts: Vec<(Pubkey, Account)> =
        vec![(signer, signer_account(10_000_000_000)), (np_id, np_acc)];
    let metas = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new_readonly(np_id, false),
    ];

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_noreplay_batch(&entries),
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

/// Boundary check on the bucket-crossing arithmetic: sequence 1023 stays in
/// bucket 0 while sequence 1024 crosses into bucket 1. An off-by-one in
/// `NoReplayBitmapAccount::bucket_index` would either merge these into one
/// CPI or misplace a bit.
#[test]
fn backfill_noreplay_bucket_boundary_1023_vs_1024() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let emitter = [0x66u8; 32];
    let entries = [
        Entry {
            chain: 2,
            emitter,
            sequence: 1023,
            digest: [0xaau8; 32],
        },
        Entry {
            chain: 2,
            emitter,
            sequence: 1024,
            digest: [0xbbu8; 32],
        },
    ];
    let (accounts, metas) = build_invocation(signer, &entries);
    assert_eq!(
        metas.len(),
        4 + 2,
        "1023 and 1024 must land in distinct buckets"
    );

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_noreplay_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw_result={:?}",
        result.raw_result
    );

    let (np_auth, _) = derive_noreplay_authority_pda();
    let bucket_0 = derive_noreplay_bucket(&np_auth, 2, &emitter, 1023);
    let bucket_1 = derive_noreplay_bucket(&np_auth, 2, &emitter, 1024);
    assert_ne!(
        bucket_0, bucket_1,
        "distinct buckets must derive to distinct PDAs"
    );

    let b0 = result.get_account(&bucket_0).unwrap().clone();
    assert_bit_set(&b0, 1023);
    let b1 = result.get_account(&bucket_1).unwrap().clone();
    assert_bit_set(&b1, 1024);
}

/// `entry_count` is a single wire byte; `255` (its max representable value)
/// must parse and execute cleanly within one bucket.
#[test]
fn backfill_noreplay_max_entry_count_255_single_bucket() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let emitter = [0x77u8; 32];
    let entries: Vec<Entry> = (0u64..255)
        .map(|i| Entry {
            chain: 2,
            emitter,
            sequence: i, // all within bucket 0 ([0, 1024))
            digest: [i as u8; 32],
        })
        .collect();
    let (accounts, metas) = build_invocation(signer, &entries);
    assert_eq!(
        metas.len(),
        4 + 1,
        "255 entries in one bucket ⇒ one bucket account"
    );

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_noreplay_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw_result={:?}",
        result.raw_result
    );

    let (np_auth, _) = derive_noreplay_authority_pda();
    let bucket = derive_noreplay_bucket(&np_auth, 2, &emitter, 0);
    let bucket_acc = result.get_account(&bucket).unwrap().clone();
    assert_bit_set(&bucket_acc, 0);
    assert_bit_set(&bucket_acc, 254);
}

/// `group_count` is a single wire byte (`255` max), but each distinct
/// bucket nests a `MarkUsedBulk` CPI, and Solana's instruction trace cap
/// (`MAX_INSTRUCTION_TRACE_LENGTH = 64`) limits this test to 30 distinct
/// buckets per transaction.
#[test]
fn backfill_noreplay_near_max_group_count_distinct_buckets() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries: Vec<Entry> = (0u16..30)
        .map(|i| {
            let mut emitter = [0u8; 32];
            emitter[30..].copy_from_slice(&i.to_be_bytes());
            Entry {
                chain: 2,
                emitter,
                sequence: 1,
                digest: [0xEEu8; 32],
            }
        })
        .collect();
    let (accounts, metas) = build_invocation(signer, &entries);
    assert_eq!(
        metas.len(),
        4 + 30,
        "30 distinct emitters ⇒ 30 distinct buckets"
    );

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_noreplay_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert_eq!(
        result.program_result,
        ProgramResult::Success,
        "raw_result={:?}",
        result.raw_result
    );

    let (np_auth, _) = derive_noreplay_authority_pda();
    let first_bucket = derive_noreplay_bucket(&np_auth, 2, &entries[0].emitter, 1);
    let last_bucket = derive_noreplay_bucket(&np_auth, 2, &entries[entries.len() - 1].emitter, 1);
    assert_bit_set(&result.get_account(&first_bucket).unwrap().clone(), 1);
    assert_bit_set(&result.get_account(&last_bucket).unwrap().clone(), 1);
}

/// Two entries in distinct buckets, but the bucket accounts are passed in
/// swapped positions — both are legitimately-derived PDAs, just for the
/// wrong bucket. Caught only by the vendored `solana_noreplay.so`
/// re-deriving its own expected PDA per `MarkUsedBulk` CPI.
#[test]
fn backfill_noreplay_swapped_bucket_accounts_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let emitter = [0x66u8; 32];
    let entries = [
        Entry {
            chain: 2,
            emitter,
            sequence: 10,
            digest: [0xaau8; 32],
        },
        Entry {
            chain: 2,
            emitter,
            sequence: 1500,
            digest: [0xbbu8; 32],
        },
    ];
    let (mut accounts, mut metas) = build_invocation(signer, &entries);
    assert_eq!(
        metas.len(),
        4 + 2,
        "expected two distinct buckets before the swap"
    );
    // Swap the two bucket accounts (indices 4 and 5) — each is a genuine PDA
    // for *some* bucket, just not the one the handler expects at that slot.
    accounts.swap(4, 5);
    metas.swap(4, 5);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_noreplay_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    // `solana-noreplay` rejects with `InvalidSeeds` when the supplied bucket
    // account doesn't match the PDA it re-derives from
    // `(authority, namespace, bucket_index)` for that specific CPI call.
    assert!(
        matches!(&result.raw_result, Err(InstructionError::InvalidSeeds)),
        "expected the vendored noreplay program to reject swapped bucket accounts \
         with InvalidSeeds, got {:?}",
        result.raw_result
    );
}

/// Three entries needing exactly one bucket total, with zero bucket accounts
/// supplied — the shortfall can only surface at the final flush step (`(5)`
/// in `backfill_noreplay.rs`).
#[test]
fn backfill_noreplay_zero_bucket_accounts_final_flush_shortfall_rejects() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [Entry {
        chain: 2,
        emitter: [0x11u8; 32],
        sequence: 42,
        digest: [0x77u8; 32],
    }];
    let (mut accounts, mut metas) = build_invocation(signer, &entries);
    assert_eq!(
        metas.len(),
        4 + 1,
        "expected exactly one bucket before truncation"
    );
    // Truncate to the fixed 4 accounts; the final flush needs a 5th bucket account.
    accounts.truncate(4);
    metas.truncate(4);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_noreplay_batch(&entries),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == GlobalAccountantError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData for final-flush bucket shortfall, got {:?}",
        result.raw_result
    );
}

/// `noreplay::mark_used_bulk` hardcodes `NOREPLAY_PROGRAM_ID`, ignoring the
/// `noreplay_program` account slot. Substituting a different executable
/// there removes the real noreplay program from every slot, so mollusk
/// panics resolving the CPI target — the panic names the real
/// `NOREPLAY_PROGRAM_ID`, proving the CPI target is compiled-in.
#[test]
fn backfill_noreplay_spoofed_cpi_target_program_is_hardcoded_not_caller_supplied() {
    let mollusk = mollusk();
    let signer = test_authority_pubkey();
    let entries = [Entry {
        chain: 2,
        emitter: [0x11u8; 32],
        sequence: 42,
        digest: [0x77u8; 32],
    }];
    let (mut accounts, metas) = build_invocation(signer, &entries);
    let (sys_id, sys_acc) = keyed_account_for_system_program();
    // Replace the noreplay_program slot (index 1) — the instruction's only
    // occurrence of the noreplay program's pubkey.
    accounts[1] = (sys_id, sys_acc);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: encode_noreplay_batch(&entries),
    };

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {})); // silence the expected panic's stderr dump
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        mollusk.process_instruction(&ix, &accounts)
    }));
    std::panic::set_hook(previous_hook);

    let panic_payload = outcome.expect_err(
        "expected mollusk to panic when the real noreplay program is absent from every \
         account slot — if this does NOT panic, the CPI must have silently accepted the \
         substituted system program as its target, meaning the target is NOT hardcoded",
    );
    let msg = panic_payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic_payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    let real_noreplay_id = Pubkey::new_from_array(NOREPLAY_PROGRAM_ID).to_string();
    assert!(
        msg.contains(&real_noreplay_id),
        "expected the panic to name the real (hardcoded) noreplay program id \
         {real_noreplay_id}, proving the CPI target is compiled-in and ignores the \
         caller-supplied account in slot 1 — got: {msg}"
    );
}
