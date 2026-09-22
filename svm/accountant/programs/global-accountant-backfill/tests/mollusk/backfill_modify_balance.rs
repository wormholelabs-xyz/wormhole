//! `BackfillModifyBalance`: `ModifyBalanceLayout` records for the wormchain governance
//! modifications the snapshot balances already carry. Each record arms `modify_balance`'s
//! per-sequence replay guard, so an archived VAA cannot apply its delta a second time.

use accountant_operational_core::instructions::modify_balance::derive_modify_balance_pda;
use anchor_lang::error::ErrorCode as AnchorError;
use global_accountant_definitions::global_accountant_backfill::Instruction;
use global_accountant_definitions::{
    BackfillModifyBalanceEntry, GlobalAccountantError, ModificationKind, ModifyBalanceLayout,
    Uint256,
};
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::result::InstructionResult;
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction as SolanaInstruction};
use solana_pubkey::Pubkey;

use crate::common::*;

const ETHEREUM: u16 = 2;
const TOKEN_ADDRESS: [u8; 32] = [0x11u8; 32];
const REASON: [u8; 32] = [0x01u8; 32];

fn record_pda(entry: &BackfillModifyBalanceEntry) -> Pubkey {
    derive_modify_balance_pda(&program_id(), entry.sequence()).0
}

fn add(sequence: u64, amount: Uint256) -> BackfillModifyBalanceEntry {
    wire::modify_balance_entry(
        ModificationKind::Add as u8,
        ETHEREUM,
        ETHEREUM,
        sequence,
        TOKEN_ADDRESS,
        amount.0,
        REASON,
    )
}

fn expected_layout(entry: &BackfillModifyBalanceEntry) -> ModifyBalanceLayout {
    ModifyBalanceLayout::new(
        ModificationKind::from_u8(entry.kind).expect("valid kind"),
        entry.chain_id(),
        entry.token_chain(),
        entry.sequence(),
        entry.token_address,
        entry.amount(),
        entry.reason,
    )
}

/// Accounts: payer, system program, then one record PDA per entry in wire order.
struct Batch {
    signer: Pubkey,
    entries: Vec<BackfillModifyBalanceEntry>,
}

impl Batch {
    fn new(entries: &[BackfillModifyBalanceEntry]) -> Self {
        Self::signed_by(test_authority_pubkey(), entries)
    }

    fn signed_by(signer: Pubkey, entries: &[BackfillModifyBalanceEntry]) -> Self {
        Self {
            signer,
            entries: entries.to_vec(),
        }
    }

    fn data(&self) -> Vec<u8> {
        wire::encode_modify_balance_batch(Instruction::BackfillModifyBalance as u8, &self.entries)
    }

    fn accounts(&self) -> Vec<(Pubkey, Account)> {
        let mut accounts = vec![
            (self.signer, system_owned_account(10_000_000_000)),
            keyed_account_for_system_program(),
        ];
        accounts.extend(
            self.entries
                .iter()
                .map(|entry| (record_pda(entry), uninitialised_pda_account())),
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
                .map(|entry| AccountMeta::new(record_pda(entry), false)),
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

fn assert_written(result: &InstructionResult, entry: &BackfillModifyBalanceEntry, label: &str) {
    let account = find_account(&result.resulting_accounts, &record_pda(entry));
    assert_eq!(account.owner, program_id(), "{label}: owner");
    assert_eq!(account.data.len(), ModifyBalanceLayout::LEN, "{label}: len");
    assert_eq!(
        layout::<ModifyBalanceLayout>(account),
        expected_layout(entry),
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

/// The handler stores a record and applies no arithmetic, so every field must survive the
/// round trip byte for byte.
#[test]
fn writes_modify_balance_records() {
    let mollusk = mollusk();
    let migration_batch: Vec<BackfillModifyBalanceEntry> = (0u64..6)
        .map(|i| {
            wire::modify_balance_entry(
                if i % 2 == 0 {
                    ModificationKind::Add as u8
                } else {
                    ModificationKind::Subtract as u8
                },
                ETHEREUM + i as u16,
                ETHEREUM,
                100 + i,
                [i as u8 + 0x11; 32],
                Uint256::from_u128(u128::from(i) * 10).0,
                [i as u8 + 1; 32],
            )
        })
        .collect();
    // Matches `BackfillBalance`'s heap bound; both handlers create one PDA per entry.
    let heap_bound: Vec<BackfillModifyBalanceEntry> = (0u64..58)
        .map(|i| add(1_000 + i, Uint256::from_u128(u128::from(i) + 1)))
        .collect();

    let cases: [(&str, Vec<BackfillModifyBalanceEntry>); 8] = [
        (
            "single record",
            vec![add(500, Uint256::from_u128(1_000_000))],
        ),
        ("six records, the migration's own count", migration_batch),
        ("58 records, the heap bound", heap_bound),
        (
            "maximum subtract",
            vec![wire::modify_balance_entry(
                ModificationKind::Subtract as u8,
                1,
                1,
                200_000,
                TOKEN_ADDRESS,
                Uint256::MAX.0,
                REASON,
            )],
        ),
        ("zero amount", vec![add(1, Uint256::ZERO)]),
        (
            "maximum chain ids",
            vec![wire::modify_balance_entry(
                ModificationKind::Subtract as u8,
                u16::MAX,
                u16::MAX,
                42,
                TOKEN_ADDRESS,
                Uint256::from_u128(1).0,
                REASON,
            )],
        ),
        (
            "maximum sequence",
            vec![add(u64::MAX, Uint256::from_u128(1))],
        ),
        (
            "distinct amount bytes",
            vec![add(
                7,
                Uint256::from_u128(0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10),
            )],
        ),
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

/// Dust pre-funding must not block a write: `create_pda_allow_prefund` tops the PDA up
/// instead of failing, so a cheap transfer cannot grief the migration.
#[test]
fn prefunded_pda_is_written() {
    let mollusk = mollusk();
    let entry = add(500, Uint256::from_u128(1_000));
    let batch = Batch::new(&[entry]);
    let mut accounts = batch.accounts();
    accounts[2].1 = system_owned_account(1_000);

    let result = submit(&mollusk, &batch.data(), &accounts, batch.metas());
    assert_success(&result, "dust prefunded record PDA");
    assert_written(&result, &entry, "dust prefunded record PDA");
}

/// Create-only, whichever wrote the record first: a previous batch or the operational
/// `modify_balance`.
#[test]
fn resubmission_rejected() {
    let mollusk = mollusk();
    let batch = Batch::new(&[add(500, Uint256::from_u128(1_000))]);

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
    let entry = add(500, Uint256::from_u128(1_000));
    let one = Batch::new(&[entry]);
    let two = Batch::new(&[entry, add(501, Uint256::from_u128(20))]);

    let wrong_signer = Batch::signed_by(Pubkey::new_from_array([0xDEu8; 32]), &[entry]);
    let unknown_kind = Batch::new(&[wire::modify_balance_entry(
        0xFF,
        ETHEREUM,
        ETHEREUM,
        500,
        TOKEN_ADDRESS,
        Uint256::from_u128(1_000).0,
        REASON,
    )]);
    let descending = Batch::new(&[
        add(501, Uint256::from_u128(10)),
        add(500, Uint256::from_u128(20)),
    ]);
    let duplicate = Batch::new(&[entry, entry]);
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
    let (foreign_accounts, foreign_metas) = {
        let (mut accounts, mut metas) = (one.accounts(), one.metas());
        let foreign_pda = Pubkey::new_from_array([0x66u8; 32]);
        accounts[2] = (foreign_pda, uninitialised_pda_account());
        metas[2] = AccountMeta::new(foreign_pda, false);
        (accounts, metas)
    };

    let cases: [Case; 10] = [
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
            label: "kind outside Add and Subtract",
            data: unknown_kind.data(),
            accounts: unknown_kind.accounts(),
            metas: unknown_kind.metas(),
            expected: GlobalAccountantError::InvalidModificationKind as u64,
        },
        Case {
            label: "one record PDA short of the wire count",
            data: two.data(),
            accounts: short_accounts,
            metas: short_metas,
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "record PDA beyond the wire count",
            data: one.data(),
            accounts: trailing_accounts,
            metas: trailing_metas,
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "descending sequence order",
            data: descending.data(),
            accounts: descending.accounts(),
            metas: descending.metas(),
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "duplicate sequence",
            data: duplicate.data(),
            accounts: duplicate.accounts(),
            metas: duplicate.metas(),
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "data ends before the count byte",
            data: vec![Instruction::BackfillModifyBalance as u8],
            accounts: one.accounts(),
            metas: one.metas(),
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "non-canonical record PDA",
            data: one.data(),
            accounts: foreign_accounts,
            metas: foreign_metas,
            expected: GlobalAccountantError::InvalidPda as u64,
        },
    ];

    for case in cases {
        let result = submit(&mollusk, &case.data, &case.accounts, case.metas);
        assert_error(&result, case.expected, case.label);
    }
}
