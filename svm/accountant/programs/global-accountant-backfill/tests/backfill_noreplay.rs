//! Integration tests for `BackfillNoReplay` driven against the real
//! `solana_noreplay.so` co-deployed in mollusk.
//!
//! Contract under test:
//! - Signer pubkey MUST equal the compile-time `BACKFILL_AUTHORITY` const.
//! - Entries strictly ascending by `(chain, emitter, sequence)`.
//! - Caller passes one bucket account per unique
//!   `(chain, emitter, sequence / 1024)` in entry order.
//! - Handler CPIs `MarkUsedBulk` once per bucket (not once per entry).
//! - Each entry emits one canonical `ACCDGST\0` commit-log payload.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_backfill::{BackfillError, Instruction as IxDiscriminator, BACKFILL_AUTHORITY},
    global_accountant_definitions::{
        NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITMAP_OFFSET, NOREPLAY_BITS_PER_BUCKET,
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
// Wire format builder
// ============================================================================

#[derive(Clone, Copy)]
struct Entry {
    chain: u16,
    emitter: [u8; 32],
    sequence: u64,
    digest: [u8; 32],
}

fn build_ix_data(entries: &[Entry]) -> Vec<u8> {
    let mut groups: Vec<Vec<&Entry>> = Vec::new();
    let mut current: Vec<&Entry> = Vec::new();
    let mut current_key: Option<(u16, [u8; 32])> = None;
    for e in entries {
        let key = (e.chain, e.emitter);
        if current_key != Some(key) {
            if !current.is_empty() {
                groups.push(std::mem::take(&mut current));
            }
            current_key = Some(key);
        }
        current.push(e);
    }
    if !current.is_empty() {
        groups.push(current);
    }
    let mut data = Vec::new();
    data.push(IxDiscriminator::BackfillNoReplay as u8);
    data.push(groups.len() as u8);
    for group in &groups {
        let first = group[0];
        data.extend_from_slice(&first.chain.to_be_bytes());
        data.extend_from_slice(&first.emitter);
        data.push(group.len() as u8);
        for e in group {
            data.extend_from_slice(&e.sequence.to_be_bytes());
            data.extend_from_slice(&e.digest);
        }
    }
    data
}

fn unique_buckets_in_order(noreplay_authority: &Pubkey, entries: &[Entry]) -> Vec<Pubkey> {
    let mut out: Vec<Pubkey> = Vec::new();
    let mut prev_key: Option<(u16, [u8; 32], u64)> = None;
    for e in entries {
        let cur_key = (e.chain, e.emitter, e.sequence / NOREPLAY_BITS_PER_BUCKET);
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
    let bit = (sequence % NOREPLAY_BITS_PER_BUCKET) as usize;
    let byte_idx = NOREPLAY_BITMAP_OFFSET + bit / 8;
    let mask = 1u8 << (bit % 8);
    assert_eq!(
        account.data[byte_idx] & mask,
        mask,
        "expected bit {bit} set in bitmap byte {byte_idx}; got {:02x}",
        account.data[byte_idx]
    );
}

// ============================================================================
// Tests
// ============================================================================

/// Sanity check: the compile-time `BACKFILL_AUTHORITY` const matches the
/// deterministic test keypair's pubkey. If this fails, the const was
/// rebuilt against a real-operator key — the test suite cannot proceed
/// (and you almost certainly didn't mean to commit the prod const).
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
        data: build_ix_data(&entries),
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
        Entry { chain: 2, emitter, sequence: 10, digest: [0xaau8; 32] },
        Entry { chain: 2, emitter, sequence: 200, digest: [0xbbu8; 32] },
        Entry { chain: 2, emitter, sequence: 800, digest: [0xccu8; 32] },
    ];
    let (accounts, metas) = build_invocation(signer, &entries);
    // Sanity: 4 fixed slots + 1 bucket.
    assert_eq!(metas.len(), 4 + 1);

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
        Entry { chain: 2, emitter, sequence: 10, digest: [0xaau8; 32] },
        Entry { chain: 2, emitter, sequence: 500, digest: [0xbbu8; 32] },
        Entry { chain: 2, emitter, sequence: 1500, digest: [0xccu8; 32] },
    ];
    let (accounts, metas) = build_invocation(signer, &entries);
    assert_eq!(metas.len(), 4 + 2, "expected two unique buckets");

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
    let wrong_signer = Pubkey::new_from_array([0xDEu8; 32]); // not the test authority
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
        data: build_ix_data(&entries),
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

