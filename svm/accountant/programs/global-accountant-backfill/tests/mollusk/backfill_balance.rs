//! `BackfillBalance`: bulk `BalanceAccountLayout` writes from wormchain
//! `query_all_accounts` rows.

use accountant_operational_core::accounts::balance;
use anchor_lang::error::ErrorCode as AnchorError;
use global_accountant_definitions::global_accountant_backfill::Instruction;
use global_accountant_definitions::{
    BackfillBalanceEntry, BalanceAccountLayout, GlobalAccountantError, Uint256,
};
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::result::InstructionResult;
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction as SolanaInstruction};
use solana_pubkey::Pubkey;

use crate::common::*;

const TOKEN_ADDRESS: [u8; 32] = [0x11u8; 32];

fn balance_pda(entry: &BackfillBalanceEntry) -> Pubkey {
    balance::derive_pda(
        &program_id(),
        entry.chain(),
        entry.token_chain(),
        &entry.token_address,
    )
    .0
}

/// Accounts: payer, system program, then one balance PDA per entry in wire order.
struct Batch {
    signer: Pubkey,
    entries: Vec<BackfillBalanceEntry>,
}

impl Batch {
    fn new(entries: &[BackfillBalanceEntry]) -> Self {
        Self::signed_by(test_authority_pubkey(), entries)
    }

    fn signed_by(signer: Pubkey, entries: &[BackfillBalanceEntry]) -> Self {
        Self {
            signer,
            entries: entries.to_vec(),
        }
    }

    fn data(&self) -> Vec<u8> {
        wire::encode_balance_batch(Instruction::BackfillBalance as u8, &self.entries)
    }

    fn accounts(&self) -> Vec<(Pubkey, Account)> {
        let mut accounts = vec![
            (self.signer, system_owned_account(10_000_000_000)),
            keyed_account_for_system_program(),
        ];
        accounts.extend(
            self.entries
                .iter()
                .map(|entry| (balance_pda(entry), uninitialised_pda_account())),
        );
        accounts
    }

    fn metas(&self) -> Vec<AccountMeta> {
        let mut metas = vec![
            AccountMeta::new(self.signer, true),
            AccountMeta::new_readonly(system_program_id(), false),
        ];
        metas.extend(
            self.entries
                .iter()
                .map(|entry| AccountMeta::new(balance_pda(entry), false)),
        );
        metas
    }

    fn submit(&self, mollusk: &Mollusk) -> InstructionResult {
        submit(mollusk, &self.data(), &self.accounts(), self.metas())
    }
}

fn submit(
    mollusk: &Mollusk,
    data: &[u8],
    accounts: &[(Pubkey, Account)],
    metas: Vec<AccountMeta>,
) -> InstructionResult {
    let ix = SolanaInstruction::new_with_bytes(program_id(), data, metas);
    mollusk.process_instruction(&ix, accounts)
}

fn assert_written(result: &InstructionResult, entry: &BackfillBalanceEntry, label: &str) {
    let account = find_account(&result.resulting_accounts, &balance_pda(entry));
    assert_eq!(account.owner, program_id(), "{label}: owner");
    assert_eq!(
        account.data.len(),
        BalanceAccountLayout::LEN,
        "{label}: len"
    );
    assert_eq!(
        layout::<BalanceAccountLayout>(account),
        BalanceAccountLayout::new(
            entry.chain(),
            entry.token_chain(),
            entry.token_address,
            entry.balance()
        ),
        "{label}: layout"
    );
}

struct Case {
    label: &'static str,
    data: Vec<u8>,
    accounts: Vec<(Pubkey, Account)>,
    metas: Vec<AccountMeta>,
    expected: u64,
}

#[test]
fn writes_balance_pdas() {
    let mollusk = mollusk();
    // `count` is one wire byte (255 max), but the 32 KiB BPF heap binds first: Anchor's
    // `Context` plus the per-CPI `Vec<AccountMeta>` accumulate on the bump allocator.
    // Measured by binary search: 58 entries pass, 59 hits "out of memory".
    let heap_bound: Vec<BackfillBalanceEntry> = (0u16..58)
        .map(|i| {
            let mut token_address = [0u8; 32];
            token_address[30..].copy_from_slice(&i.to_be_bytes());
            wire::balance_entry(2, 2, token_address, [0xaau8; 32])
        })
        .collect();

    let cases: [(&str, Vec<BackfillBalanceEntry>); 5] = [
        (
            "single entry",
            vec![wire::balance_entry(
                2,
                2,
                TOKEN_ADDRESS,
                Uint256::from_u128(256).0,
            )],
        ),
        (
            "three distinct balances",
            vec![
                wire::balance_entry(2, 2, TOKEN_ADDRESS, [0xaau8; 32]),
                wire::balance_entry(2, 4, [0x22u8; 32], [0xbbu8; 32]),
                wire::balance_entry(5, 5, [0x33u8; 32], [0xccu8; 32]),
            ],
        ),
        (
            "zero balance",
            vec![wire::balance_entry(2, 2, TOKEN_ADDRESS, Uint256::ZERO.0)],
        ),
        (
            "maximum balance",
            vec![wire::balance_entry(2, 2, TOKEN_ADDRESS, Uint256::MAX.0)],
        ),
        ("58 entries, the heap bound", heap_bound),
    ];

    for (label, entries) in cases {
        let batch = Batch::new(&entries);
        let result = batch.submit(&mollusk);
        assert_success(&result, label);
        for entry in &entries {
            assert_written(&result, entry, label);
        }
    }
}

/// A prefunded PDA takes `create_pda_allow_prefund`'s top-up path rather than failing, so a
/// cheap lamport transfer cannot block the migration.
#[test]
fn prefunded_pda_is_written() {
    let mollusk = mollusk();
    const OVER_FUNDED: u64 = 10_000_000;
    // (label, prefund, lamports untouched)
    let cases: [(&str, u64, bool); 2] = [
        ("dust below rent exemption", 1_000, false),
        ("above rent exemption", OVER_FUNDED, true),
    ];

    for (label, prefund, unchanged) in cases {
        let entry = wire::balance_entry(2, 2, TOKEN_ADDRESS, [0xaau8; 32]);
        let batch = Batch::new(&[entry]);
        let mut accounts = batch.accounts();
        accounts[2].1 = system_owned_account(prefund);

        let result = submit(&mollusk, &batch.data(), &accounts, batch.metas());
        assert_success(&result, label);
        assert_written(&result, &entry, label);
        if unchanged {
            let account = find_account(&result.resulting_accounts, &balance_pda(&entry));
            assert_eq!(account.lamports, OVER_FUNDED, "{label}: lamports");
        }
    }
}

/// Create-only: `create_pda_allow_prefund` rejects an already-initialised PDA, so a lagging
/// orchestrator cursor cannot re-apply a batch over a landed one.
#[test]
fn resubmission_rejected() {
    let mollusk = mollusk();
    let entry = wire::balance_entry(2, 2, TOKEN_ADDRESS, [0xaau8; 32]);
    let batch = Batch::new(&[entry]);

    let first = batch.submit(&mollusk);
    assert_success(&first, "first submit");

    let second = submit(
        &mollusk,
        &batch.data(),
        &first.resulting_accounts,
        batch.metas(),
    );
    assert_error(
        &second,
        GlobalAccountantError::InvalidPda as u64,
        "resubmission",
    );
}

#[test]
fn rejects() {
    let mollusk = mollusk();
    let entry = wire::balance_entry(2, 2, TOKEN_ADDRESS, [0xaau8; 32]);
    let one = Batch::new(&[entry]);
    let two = Batch::new(&[entry, wire::balance_entry(4, 4, [0x22u8; 32], [0xbbu8; 32])]);

    let unsigned_metas = {
        let mut metas = one.metas();
        metas[0] = AccountMeta::new(one.signer, false);
        metas
    };
    let (short_accounts, short_metas) = {
        let (mut accounts, mut metas) = (two.accounts(), two.metas());
        accounts.pop();
        metas.pop();
        (accounts, metas)
    };
    let (trailing_accounts, trailing_metas) = {
        let (mut accounts, mut metas) = (one.accounts(), one.metas());
        let dead_pda = Pubkey::new_from_array([0x88u8; 32]);
        accounts.push((dead_pda, uninitialised_pda_account()));
        metas.push(AccountMeta::new(dead_pda, false));
        (accounts, metas)
    };
    let descending = Batch::new(&[
        wire::balance_entry(5, 5, [0x33u8; 32], [0xaau8; 32]),
        wire::balance_entry(2, 2, TOKEN_ADDRESS, [0xbbu8; 32]),
    ]);
    let (foreign_accounts, foreign_metas) = {
        let (mut accounts, mut metas) = (one.accounts(), one.metas());
        let foreign_pda = Pubkey::new_from_array([0x66u8; 32]);
        accounts[2] = (foreign_pda, uninitialised_pda_account());
        metas[2] = AccountMeta::new(foreign_pda, false);
        (accounts, metas)
    };
    let third_party_owned = {
        let mut accounts = one.accounts();
        accounts[2].1 = Account {
            lamports: 1_000_000,
            data: vec![],
            owner: Pubkey::new_from_array([0x77u8; 32]),
            executable: false,
            rent_epoch: 0,
        };
        accounts
    };
    let wrong_signer = Batch::signed_by(Pubkey::new_from_array([0xDEu8; 32]), &[entry]);

    let cases: [Case; 9] = [
        Case {
            label: "signer is not the backfill authority",
            data: wrong_signer.data(),
            accounts: wrong_signer.accounts(),
            metas: wrong_signer.metas(),
            expected: GlobalAccountantError::UnauthorizedCaller as u64,
        },
        Case {
            label: "authority present but not signing",
            data: one.data(),
            accounts: one.accounts(),
            metas: unsigned_metas,
            expected: AnchorError::AccountNotSigner as u64,
        },
        Case {
            label: "payer alone, context accounts missing",
            data: one.data(),
            accounts: vec![(one.signer, system_owned_account(10_000_000_000))],
            metas: vec![AccountMeta::new(one.signer, true)],
            expected: AnchorError::AccountNotEnoughKeys as u64,
        },
        Case {
            label: "one balance PDA short of the wire count",
            data: two.data(),
            accounts: short_accounts,
            metas: short_metas,
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "balance PDA beyond the wire count",
            data: one.data(),
            accounts: trailing_accounts,
            metas: trailing_metas,
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "descending key order",
            data: descending.data(),
            accounts: descending.accounts(),
            metas: descending.metas(),
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "data ends before the count byte",
            data: vec![Instruction::BackfillBalance as u8],
            accounts: one.accounts(),
            metas: one.metas(),
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "non-canonical balance PDA",
            data: one.data(),
            accounts: foreign_accounts,
            metas: foreign_metas,
            expected: GlobalAccountantError::InvalidPda as u64,
        },
        Case {
            label: "empty PDA owned by a third party",
            data: one.data(),
            accounts: third_party_owned,
            metas: one.metas(),
            expected: GlobalAccountantError::InvalidPda as u64,
        },
    ];

    for case in cases {
        let result = submit(&mollusk, &case.data, &case.accounts, case.metas);
        assert_error(&result, case.expected, case.label);
    }
}
