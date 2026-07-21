//! Integration tests for the handlers the NTT program reuses from
//! `accountant-backfill-core` — `BackfillNoReplay` (discriminator 0) and
//! `BackfillBalance` (discriminator 1) — driven through the NTT program's own
//! entrypoint and `BACKFILL_AUTHORITY` rather than WTT's.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        BalanceAccountLayout, Uint256, ACCOUNT_SEED_PREFIX, NOREPLAY_AUTHORITY_SEED_PREFIX,
        NOREPLAY_BITMAP_OFFSET, NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID,
    },
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult},
    ntt_global_accountant_backfill::{BackfillError, Instruction as Ix},
    solana_account::Account,
    solana_instruction::{error::InstructionError, AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

mod common;
use common::{
    keyed_account_for_noreplay_program, mollusk_with_noreplay, program_id, signer_account,
    test_authority_pubkey, uninitialised_pda_account,
};

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
// BackfillNoReplay (discriminator 0, dispatched via accountant-backfill-core)
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

#[derive(Clone, Copy)]
struct NoReplayEntry {
    chain: u16,
    emitter: [u8; 32],
    sequence: u64,
    digest: [u8; 32],
}

fn build_noreplay_ix_data(entries: &[NoReplayEntry]) -> Vec<u8> {
    let mut groups: Vec<Vec<&NoReplayEntry>> = Vec::new();
    let mut current: Vec<&NoReplayEntry> = Vec::new();
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
    data.push(Ix::BackfillNoReplay as u8);
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

fn unique_buckets_in_order(noreplay_authority: &Pubkey, entries: &[NoReplayEntry]) -> Vec<Pubkey> {
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

fn build_noreplay_invocation(
    signer: Pubkey,
    entries: &[NoReplayEntry],
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

/// Happy path through the NTT program's own entrypoint and
/// `BACKFILL_AUTHORITY` (discriminator 0).
#[test]
fn ntt_backfill_noreplay_single_entry_flips_bit() {
    let mollusk = mollusk_with_noreplay();
    let signer = test_authority_pubkey();
    let entries = [NoReplayEntry {
        chain: 2,
        emitter: [0x11u8; 32],
        sequence: 42,
        digest: [0x77u8; 32],
    }];
    let (accounts, metas) = build_noreplay_invocation(signer, &entries);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_noreplay_ix_data(&entries),
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

/// Caller signs with a pubkey other than the NTT program's `BACKFILL_AUTHORITY`.
#[test]
fn ntt_backfill_noreplay_wrong_signer_rejects() {
    let mollusk = mollusk_with_noreplay();
    let wrong_signer = Pubkey::new_from_array([0xDEu8; 32]);
    let entries = [NoReplayEntry {
        chain: 2,
        emitter: [0x11u8; 32],
        sequence: 42,
        digest: [0x77u8; 32],
    }];
    let (accounts, metas) = build_noreplay_invocation(wrong_signer, &entries);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_noreplay_ix_data(&entries),
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::UnauthorizedCaller,
    );
}

// ============================================================================
// BackfillBalance (discriminator 1, dispatched via accountant-backfill-core)
// ============================================================================

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

#[derive(Clone, Copy)]
struct BalanceEntry {
    chain: u16,
    token_chain: u16,
    token_address: [u8; 32],
    balance: [u8; 32],
}

fn build_balance_ix_data(entries: &[BalanceEntry]) -> Vec<u8> {
    let mut data = Vec::with_capacity(2 + entries.len() * 68);
    data.push(Ix::BackfillBalance as u8);
    data.push(entries.len() as u8);
    for e in entries {
        data.extend_from_slice(&e.chain.to_be_bytes());
        data.extend_from_slice(&e.token_chain.to_be_bytes());
        data.extend_from_slice(&e.token_address);
        data.extend_from_slice(&e.balance);
    }
    data
}

fn build_balance_invocation(
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

/// Happy path through the NTT program's own entrypoint and
/// `BACKFILL_AUTHORITY` (discriminator 1).
#[test]
fn ntt_backfill_balance_single_entry_writes_pda() {
    // BackfillBalance needs no CPI; reusing mollusk_with_noreplay() avoids a
    // second constructor.
    let mollusk = mollusk_with_noreplay();
    let signer = test_authority_pubkey();
    let entry = BalanceEntry {
        chain: 2,
        token_chain: 2,
        token_address: [0x11u8; 32],
        balance: [0xaau8; 32],
    };
    let (accounts, metas) = build_balance_invocation(signer, &[entry]);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_balance_ix_data(&[entry]),
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

/// Caller signs with a pubkey other than the NTT program's `BACKFILL_AUTHORITY`.
#[test]
fn ntt_backfill_balance_wrong_signer_rejects() {
    let mollusk = mollusk_with_noreplay();
    let wrong_signer = Pubkey::new_from_array([0xDEu8; 32]);
    let entry = BalanceEntry {
        chain: 2,
        token_chain: 2,
        token_address: [0x11u8; 32],
        balance: [0xaau8; 32],
    };
    let (accounts, metas) = build_balance_invocation(wrong_signer, &[entry]);

    let ix = Instruction {
        program_id: program_id(),
        accounts: metas,
        data: build_balance_ix_data(&[entry]),
    };
    assert_custom(
        &mollusk.process_instruction(&ix, &accounts),
        BackfillError::UnauthorizedCaller,
    );
}
