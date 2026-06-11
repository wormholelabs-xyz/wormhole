//! Integration tests for `BackfillNoReplay` driven against the real
//! `solana_noreplay.so` co-deployed in mollusk.
//!
//! Contract under test (post-`MarkUsedBulk` refactor):
//! - Entries strictly ascending by `(chain, emitter, sequence)`.
//! - Caller passes one bucket account per unique
//!   `(chain, emitter, sequence / 1024)` in entry order.
//! - Handler CPIs `MarkUsedBulk` once per bucket (not once per entry).
//! - Each entry emits one canonical `ACCDGST\0` commit-log payload.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_backfill::{BackfillError, Instruction as IxDiscriminator},
    global_accountant_backfill::state::{BackfillAuthorityLayout, BACKFILL_AUTHORITY_SEED_PREFIX},
    global_accountant_definitions::{
        NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITMAP_OFFSET, NOREPLAY_BITS_PER_BUCKET,
        NOREPLAY_PROGRAM_ID,
    },
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk},
    solana_account::Account,
    solana_instruction::{error::InstructionError, AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

mod common;
use common::{keyed_account_for_noreplay_program, mollusk_with_noreplay};

fn program_id() -> Pubkey {
    Pubkey::new_from_array([8u8; 32])
}

fn mollusk() -> Mollusk {
    mollusk_with_noreplay(&program_id())
}

fn system_program_id() -> Pubkey {
    keyed_account_for_system_program().0
}

// ============================================================================
// PDA derivations
// ============================================================================

fn derive_backfill_authority_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[BACKFILL_AUTHORITY_SEED_PREFIX], &program_id())
}

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
// Account fixtures
// ============================================================================

fn system_owned_account(lamports: u64) -> Account {
    Account {
        lamports,
        data: vec![],
        owner: system_program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn signer_account(lamports: u64) -> Account {
    system_owned_account(lamports)
}

fn uninitialised_pda_account() -> Account {
    system_owned_account(0)
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

/// Emit the compact emitter-grouped wire format:
/// `[disc][group_count] [chain emitter entry_count [seq digest]...]...`
///
/// Walks consecutively-equal `(chain, emitter)` runs into groups. Caller is
/// responsible for sorting entries by `(chain, emitter, sequence)` first.
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

/// Derive the unique buckets for `entries`, preserving entry order. Each
/// returned `Pubkey` corresponds to one CPI `MarkUsedBulk` the handler will
/// perform.
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

/// Build the full `(accounts, metas)` pair for an N-entry `BackfillNoReplay`
/// invocation. Caller responsibility: entries are strictly ascending by
/// `(chain, emitter, sequence)`; `unique_buckets_in_order` derives the bucket
/// list in matching order.
fn build_invocation(
    signer: Pubkey,
    entries: &[Entry],
) -> (Vec<(Pubkey, Account)>, Vec<AccountMeta>) {
    let (backfill_auth_pda, _) = derive_backfill_authority_pda();
    let (noreplay_auth_pda, _) = derive_noreplay_authority_pda();
    let buckets = unique_buckets_in_order(&noreplay_auth_pda, entries);

    let (np_id, np_acc) = keyed_account_for_noreplay_program();
    let (sys_id, sys_acc) = keyed_account_for_system_program();

    let mut accounts: Vec<(Pubkey, Account)> = vec![
        (signer, signer_account(10_000_000_000)),
        (backfill_auth_pda, uninitialised_pda_account()),
        (np_id, np_acc),
        (noreplay_auth_pda, uninitialised_pda_account()),
        (sys_id, sys_acc),
    ];
    let mut metas: Vec<AccountMeta> = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new(backfill_auth_pda, false),
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

/// Happy path: single entry. Lazy-inits the backfill authority PDA, flips one
/// bit via `MarkUsedBulk`, emits one commit-log.
#[test]
fn backfill_noreplay_single_entry_flips_bit() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([42u8; 32]);
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

    let (auth_pda, _) = derive_backfill_authority_pda();
    let auth = result.resulting_accounts.iter().find(|(k, _)| k == &auth_pda).unwrap().1.clone();
    assert_eq!(auth.data.len(), BackfillAuthorityLayout::LEN);
    assert_eq!(&auth.data[..32], signer.as_array());
    assert_eq!(auth.data[32], 0);
    assert_eq!(auth.owner, program_id());

    let (np_auth, _) = derive_noreplay_authority_pda();
    let bucket = derive_noreplay_bucket(&np_auth, 2, &[0x11u8; 32], 42);
    let bucket_acc = result.resulting_accounts.iter().find(|(k, _)| k == &bucket).unwrap().1.clone();
    assert_bit_set(&bucket_acc, 42);
}

/// Three entries in the same bucket (all sequences in `[0, 1024)` for the
/// same `(chain, emitter)`). Handler must batch into one CPI; the test
/// asserts all three bits are set in the single bucket account.
#[test]
fn backfill_noreplay_multiple_entries_same_bucket_one_cpi() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([43u8; 32]);
    let emitter = [0x22u8; 32];
    let entries = [
        Entry { chain: 2, emitter, sequence: 10, digest: [0xaau8; 32] },
        Entry { chain: 2, emitter, sequence: 200, digest: [0xbbu8; 32] },
        Entry { chain: 2, emitter, sequence: 800, digest: [0xccu8; 32] },
    ];
    let (accounts, metas) = build_invocation(signer, &entries);
    // Sanity: one bucket only.
    assert_eq!(metas.len(), 5 + 1);

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
    let bucket_acc = result.resulting_accounts.iter().find(|(k, _)| k == &bucket).unwrap().1.clone();
    assert_bit_set(&bucket_acc, 10);
    assert_bit_set(&bucket_acc, 200);
    assert_bit_set(&bucket_acc, 800);
}

/// Three entries spanning two buckets — first two in bucket 0, third in
/// bucket 1. Handler must emit two `MarkUsedBulk` CPIs.
#[test]
fn backfill_noreplay_multiple_entries_different_buckets() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([44u8; 32]);
    let emitter = [0x33u8; 32];
    let entries = [
        Entry { chain: 2, emitter, sequence: 10, digest: [0xaau8; 32] },
        Entry { chain: 2, emitter, sequence: 500, digest: [0xbbu8; 32] },
        Entry { chain: 2, emitter, sequence: 1500, digest: [0xccu8; 32] },
    ];
    let (accounts, metas) = build_invocation(signer, &entries);
    assert_eq!(metas.len(), 5 + 2, "expected two unique buckets");

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

    let b0 = result.resulting_accounts.iter().find(|(k, _)| k == &bucket_0).unwrap().1.clone();
    assert_bit_set(&b0, 10);
    assert_bit_set(&b0, 500);

    let b1 = result.resulting_accounts.iter().find(|(k, _)| k == &bucket_1).unwrap().1.clone();
    assert_bit_set(&b1, 1500);
}

/// Entries with the wrong sort order are rejected before any CPI.
#[test]
fn backfill_noreplay_out_of_order_rejects() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([45u8; 32]);
    let emitter = [0x44u8; 32];
    let entries = [
        Entry { chain: 2, emitter, sequence: 100, digest: [0xaau8; 32] },
        Entry { chain: 2, emitter, sequence: 50, digest: [0xbbu8; 32] }, // out of order
    ];
    // Hand-build the invocation with one bucket account; the data describes
    // two entries in the same bucket, so the test expects an in-order failure.
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
        "expected InvalidInstructionData on out-of-order entries, got {:?}",
        result.raw_result
    );
}

/// Duplicate entries (same `(chain, emitter, sequence)` twice) are rejected
/// — strict-ascending forbids `==` as well as `<`.
#[test]
fn backfill_noreplay_duplicate_entries_reject() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([46u8; 32]);
    let emitter = [0x55u8; 32];
    let dup = Entry { chain: 2, emitter, sequence: 42, digest: [0xaau8; 32] };
    let entries = [dup, dup];
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
        "expected InvalidInstructionData on duplicate entries, got {:?}",
        result.raw_result
    );
}

/// Caller passes more bucket accounts than the handler will consume — must
/// reject so a buggy/malicious caller cannot grief us with stray accounts.
#[test]
fn backfill_noreplay_extra_bucket_account_rejects() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([47u8; 32]);
    let emitter = [0x66u8; 32];
    let entries = [Entry { chain: 2, emitter, sequence: 7, digest: [0xaau8; 32] }];

    let (mut accounts, mut metas) = build_invocation(signer, &entries);
    // Append one unused bucket account.
    let (np_auth, _) = derive_noreplay_authority_pda();
    let stray = derive_noreplay_bucket(&np_auth, 999, &[0x99u8; 32], 0);
    accounts.push((stray, uninitialised_pda_account()));
    metas.push(AccountMeta::new(stray, false));

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
        "expected InvalidInstructionData on extra bucket account, got {:?}",
        result.raw_result
    );
}

/// First call lazy-inits the authority PDA with the signer's pubkey. A
/// subsequent call from a DIFFERENT signer must be rejected as
/// `AuthorityMismatch` — the backfill operator pubkey is single-use post-init.
#[test]
fn backfill_noreplay_second_call_with_different_signer_rejects() {
    let mollusk = mollusk();
    let signer_a = Pubkey::new_from_array([42u8; 32]);
    let signer_b = Pubkey::new_from_array([99u8; 32]);

    // First call: signer_a inits the authority PDA.
    let entry_a = Entry {
        chain: 2,
        emitter: [0x11u8; 32],
        sequence: 42,
        digest: [0x77u8; 32],
    };
    let (accounts_a, metas_a) = build_invocation(signer_a, &[entry_a]);
    let ix_a = Instruction {
        program_id: program_id(),
        accounts: metas_a,
        data: build_ix_data(&[entry_a]),
    };
    let result_a = mollusk.process_instruction(&ix_a, &accounts_a);
    assert_eq!(
        result_a.program_result,
        ProgramResult::Success,
        "first call should succeed; raw_result={:?}",
        result_a.raw_result
    );

    // Second call: signer_b, but the auth PDA is already stamped with signer_a.
    let entry_b = Entry {
        chain: 3,
        emitter: [0x22u8; 32],
        sequence: 7,
        digest: [0x88u8; 32],
    };
    let (mut accounts_b, metas_b) = build_invocation(signer_b, &[entry_b]);

    // Carry forward the initialised auth PDA so the second call sees it.
    let (auth_pda, _) = derive_backfill_authority_pda();
    let auth_post = result_a.get_account(&auth_pda).unwrap().clone();
    for (k, acc) in accounts_b.iter_mut() {
        if *k == auth_pda {
            *acc = auth_post.clone();
            break;
        }
    }

    let ix_b = Instruction {
        program_id: program_id(),
        accounts: metas_b,
        data: build_ix_data(&[entry_b]),
    };
    let result_b = mollusk.process_instruction(&ix_b, &accounts_b);
    assert!(
        matches!(
            &result_b.raw_result,
            Err(InstructionError::Custom(code))
                if *code == BackfillError::AuthorityMismatch as u32
        ),
        "expected AuthorityMismatch on second-signer call, got {:?}",
        result_b.raw_result
    );
}

/// A pre-retired authority PDA (`retired = 1`) rejects all subsequent
/// invocations — even from the original signer. Simulates the post-`Retire`
/// kill-switch state ahead of `RetireAuthority`'s landing.
#[test]
fn backfill_noreplay_retired_authority_rejects() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([42u8; 32]);

    // Pre-populate the auth PDA with retired=1.
    let mut layout: BackfillAuthorityLayout = bytemuck::Zeroable::zeroed();
    layout.authority = *signer.as_array();
    layout.retired = 1;
    let (auth_pda, _) = derive_backfill_authority_pda();
    let retired_auth = Account {
        lamports: 1_000_000,
        data: bytemuck::bytes_of(&layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    };

    let entry = Entry {
        chain: 2,
        emitter: [0x11u8; 32],
        sequence: 42,
        digest: [0x77u8; 32],
    };
    let (mut accounts, metas) = build_invocation(signer, &[entry]);
    for (k, acc) in accounts.iter_mut() {
        if *k == auth_pda {
            *acc = retired_auth.clone();
            break;
        }
    }

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
                if *code == BackfillError::AuthorityRetired as u32
        ),
        "expected AuthorityRetired, got {:?}",
        result.raw_result
    );
}

/// Zero-entry call rejects before any account touch.
#[test]
fn backfill_noreplay_zero_entries_rejects() {
    let mollusk = mollusk();
    let signer = Pubkey::new_from_array([48u8; 32]);
    let (accounts, metas) = build_invocation(signer, &[]);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_ix_data(&[]),
    };
    let result = mollusk.process_instruction(&ix, &accounts);
    assert!(
        matches!(
            &result.raw_result,
            Err(InstructionError::Custom(code))
                if *code == BackfillError::InvalidInstructionData as u32
        ),
        "expected InvalidInstructionData on zero entries, got {:?}",
        result.raw_result
    );
}
