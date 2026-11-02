//! `BackfillRegisterChainRecord`: one `RegisterChain` record PDA per superseded wormchain
//! registration sequence, so the older VAA cannot apply after the cutover.

use accountant_operational_core::instructions::register_chain::derive_register_chain_pda;
use global_accountant_definitions::global_accountant_backfill::Instruction;
use global_accountant_definitions::{
    BackfillChainRegistrationEntry, GlobalAccountantError, RegisterChainLayout, MAX_BATCH_ENTRIES,
};
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::result::InstructionResult;
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction as SolanaInstruction};
use solana_pubkey::Pubkey;

use crate::common::*;

const BSC: u16 = 4;

fn record_pda(entry: &BackfillChainRegistrationEntry) -> Pubkey {
    derive_register_chain_pda(&program_id(), entry.sequence()).0
}

/// Accounts: payer, system program, then one `RegisterChain` record PDA per entry.
struct Batch {
    signer: Pubkey,
    entries: Vec<BackfillChainRegistrationEntry>,
}

impl Batch {
    fn new(entries: &[BackfillChainRegistrationEntry]) -> Self {
        Self::signed_by(test_authority_pubkey(), entries)
    }

    fn signed_by(signer: Pubkey, entries: &[BackfillChainRegistrationEntry]) -> Self {
        Self {
            signer,
            entries: entries.to_vec(),
        }
    }

    fn data(&self) -> Vec<u8> {
        wire::encode_register_chain_record_batch(
            Instruction::BackfillRegisterChainRecord as u8,
            &self.entries,
        )
    }

    fn accounts(&self) -> Vec<(Pubkey, Account)> {
        let mut accounts = vec![
            (self.signer, system_owned_account(10_000_000_000)),
            keyed_account_for_system_program(),
        ];
        for entry in &self.entries {
            accounts.push((record_pda(entry), uninitialised_pda_account()));
        }
        accounts
    }

    fn metas(&self) -> Vec<AccountMeta> {
        let mut metas = vec![
            AccountMeta::new(self.signer, true),
            AccountMeta::new_readonly(system_program_id(), false),
        ];
        for entry in &self.entries {
            metas.push(AccountMeta::new(record_pda(entry), false));
        }
        metas
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

struct Case {
    label: &'static str,
    data: Vec<u8>,
    accounts: Vec<(Pubkey, Account)>,
    metas: Vec<AccountMeta>,
    expected: u64,
}

#[test]
fn writes_record_only() {
    let mollusk = mollusk();
    // Two superseded sequences of one chain and one of another, ascending by sequence.
    let entries = [
        wire::chain_registration_entry(ETHEREUM, 5, [0x11u8; 32]),
        wire::chain_registration_entry(BSC, 6, [0x22u8; 32]),
        wire::chain_registration_entry(ETHEREUM, 9, [0x33u8; 32]),
    ];
    let batch = Batch::new(&entries);

    let result = submit(&mollusk, &batch.data(), &batch.accounts(), batch.metas());
    assert_success(&result, "record-only batch");
    for entry in &entries {
        let record = find_account(&result.resulting_accounts, &record_pda(entry));
        assert_eq!(record.owner, program_id(), "record owner");
        assert_eq!(
            layout::<RegisterChainLayout>(record),
            RegisterChainLayout::new(entry.chain(), entry.emitter, entry.sequence()),
            "record layout"
        );
    }
}

#[test]
fn wrong_signer_writes_nothing() {
    let mollusk = mollusk();
    let entry = wire::chain_registration_entry(ETHEREUM, 5, [0x11u8; 32]);
    let batch = Batch::signed_by(Pubkey::new_from_array([0xDEu8; 32]), &[entry]);
    let accounts = batch.accounts();

    let result = submit(&mollusk, &batch.data(), &accounts, batch.metas());
    assert_error(
        &result,
        GlobalAccountantError::UnauthorizedCaller as u64,
        "signer is not the backfill authority",
    );
    assert_eq!(
        result.resulting_accounts, accounts,
        "no PDA may be touched on rejection"
    );
}

#[test]
fn rejects() {
    let mollusk = mollusk();
    let first = wire::chain_registration_entry(ETHEREUM, 5, [0x11u8; 32]);
    let second = wire::chain_registration_entry(ETHEREUM, 9, [0x33u8; 32]);
    let one = Batch::new(&[first]);
    let two = Batch::new(&[first, second]);

    let existing_record = {
        let mut accounts = one.accounts();
        accounts[2].1 = Account {
            lamports: 1_000_000,
            data: vec![0u8; RegisterChainLayout::LEN],
            owner: program_id(),
            executable: false,
            rent_epoch: 0,
        };
        accounts
    };
    let (swapped_accounts, swapped_metas) = {
        let (mut accounts, mut metas) = (two.accounts(), two.metas());
        accounts.swap(2, 3);
        metas.swap(2, 3);
        (accounts, metas)
    };
    let unsorted = Batch::new(&[second, first]);

    let cases: [Case; 5] = [
        Case {
            label: "sequence already recorded",
            data: one.data(),
            accounts: existing_record,
            metas: one.metas(),
            expected: GlobalAccountantError::InvalidPda as u64,
        },
        Case {
            label: "record PDAs swapped",
            data: two.data(),
            accounts: swapped_accounts,
            metas: swapped_metas,
            expected: GlobalAccountantError::InvalidPda as u64,
        },
        Case {
            label: "one PDA short",
            data: two.data(),
            accounts: two.accounts()[..3].to_vec(),
            metas: two.metas()[..3].to_vec(),
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "one PDA extra",
            data: one.data(),
            accounts: two.accounts(),
            metas: two.metas(),
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "descending sequences",
            data: unsorted.data(),
            accounts: unsorted.accounts(),
            metas: unsorted.metas(),
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
    ];
    for case in cases {
        let result = submit(&mollusk, &case.data, &case.accounts, case.metas);
        assert_error(&result, case.expected, case.label);
    }
}

/// One create per entry and a 48-byte record: `MAX_BATCH_ENTRIES` fit both the 64-entry
/// instruction trace and the heap, and one more fails in the parser ahead of any write.
#[test]
fn batch_bound_is_max_batch_entries() {
    let mollusk = mollusk();
    let entries = |count: u64| -> Vec<BackfillChainRegistrationEntry> {
        (0..count)
            .map(|offset| wire::chain_registration_entry(ETHEREUM, 1_000 + offset, [0x11u8; 32]))
            .collect()
    };

    let at_bound = Batch::new(&entries(u64::from(MAX_BATCH_ENTRIES)));
    let result = submit(
        &mollusk,
        &at_bound.data(),
        &at_bound.accounts(),
        at_bound.metas(),
    );
    assert_success(&result, "at the batch bound");

    let over_bound = Batch::new(&entries(u64::from(MAX_BATCH_ENTRIES) + 1));
    let accounts = over_bound.accounts();
    let result = submit(&mollusk, &over_bound.data(), &accounts, over_bound.metas());
    assert_error(
        &result,
        GlobalAccountantError::InvalidInstructionData as u64,
        "one entry above the batch bound",
    );
    assert_eq!(
        result.resulting_accounts, accounts,
        "no PDA may be touched on rejection"
    );
}

/// Dust pre-funding must not block a write: `create_pda_allow_prefund` tops the PDA up
/// instead of failing, so a cheap transfer cannot grief the migration.
#[test]
fn prefunded_pda_is_written() {
    let mollusk = mollusk();
    let entry = wire::chain_registration_entry(ETHEREUM, 5, [0x11u8; 32]);
    let batch = Batch::new(&[entry]);
    let mut accounts = batch.accounts();
    accounts[2].1 = system_owned_account(1_000);

    let result = submit(&mollusk, &batch.data(), &accounts, batch.metas());
    assert_success(&result, "dust prefunded record PDA");
    assert_eq!(
        layout::<RegisterChainLayout>(find_account(
            &result.resulting_accounts,
            &record_pda(&entry)
        )),
        RegisterChainLayout::new(ETHEREUM, entry.emitter, 5),
        "dust prefunded record PDA"
    );
}

/// Create-only, whichever wrote the record first: a previous batch, the installing sequence
/// of `BackfillChainRegistration`, or the operational `register_chain`.
#[test]
fn resubmission_rejected() {
    let mollusk = mollusk();
    let batch = Batch::new(&[wire::chain_registration_entry(ETHEREUM, 5, [0x11u8; 32])]);

    let first = submit(&mollusk, &batch.data(), &batch.accounts(), batch.metas());
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
