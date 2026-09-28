//! `BackfillChainRegistration`: the `ChainRegistration` PDA plus the `RegisterChain` record
//! PDA per wormchain registration row. Both are the accounts the operational
//! `register_chain` writes, so `submit_vaas` finds every registration at cutover and the
//! installing VAA cannot apply again.

use accountant_operational_core::accounts::chain_registration;
use accountant_operational_core::instructions::register_chain::derive_register_chain_pda;
use global_accountant_definitions::global_accountant_backfill::Instruction;
use global_accountant_definitions::{
    BackfillChainRegistrationEntry, ChainRegistrationLayout, GlobalAccountantError,
    RegisterChainLayout,
};
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::result::InstructionResult;
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction as SolanaInstruction};
use solana_pubkey::Pubkey;

use crate::common::*;

const BSC: u16 = 4;
const POLYGON: u16 = 5;

fn registration_pda(entry: &BackfillChainRegistrationEntry) -> Pubkey {
    chain_registration::derive_pda(&program_id(), entry.chain()).0
}

fn record_pda(entry: &BackfillChainRegistrationEntry) -> Pubkey {
    derive_register_chain_pda(&program_id(), entry.sequence()).0
}

/// A PDA a previous transaction created: program-owned, `len` zero bytes.
fn existing_pda_account(len: usize) -> Account {
    Account {
        lamports: 1_000_000,
        data: vec![0u8; len],
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

/// Accounts: payer, system program, then per entry the `ChainRegistration` PDA followed by
/// the `RegisterChain` record PDA.
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
        wire::encode_chain_registration_batch(
            Instruction::BackfillChainRegistration as u8,
            &self.entries,
        )
    }

    fn accounts(&self) -> Vec<(Pubkey, Account)> {
        let mut accounts = vec![
            (self.signer, system_owned_account(10_000_000_000)),
            keyed_account_for_system_program(),
        ];
        for entry in &self.entries {
            accounts.push((registration_pda(entry), uninitialised_pda_account()));
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
            metas.push(AccountMeta::new(registration_pda(entry), false));
            metas.push(AccountMeta::new(record_pda(entry), false));
        }
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

/// Both PDAs hold the layouts the operational `register_chain` builds from the same
/// constructors.
fn assert_written(result: &InstructionResult, entry: &BackfillChainRegistrationEntry, label: &str) {
    let (chain, sequence, emitter) = (entry.chain(), entry.sequence(), entry.emitter);

    let registration = find_account(&result.resulting_accounts, &registration_pda(entry));
    assert_eq!(
        registration.owner,
        program_id(),
        "{label}: registration owner"
    );
    assert_eq!(
        registration.data.len(),
        ChainRegistrationLayout::LEN,
        "{label}: registration len"
    );
    assert_eq!(
        layout::<ChainRegistrationLayout>(registration),
        ChainRegistrationLayout::new(chain, emitter, sequence),
        "{label}: registration layout"
    );

    let record = find_account(&result.resulting_accounts, &record_pda(entry));
    assert_eq!(record.owner, program_id(), "{label}: record owner");
    assert_eq!(
        record.data.len(),
        RegisterChainLayout::LEN,
        "{label}: record len"
    );
    assert_eq!(
        layout::<RegisterChainLayout>(record),
        RegisterChainLayout::new(chain, emitter, sequence),
        "{label}: record layout"
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
fn writes_registration_and_record_pdas() {
    let mollusk = mollusk();
    // Governance sequences are unrelated to chain order, so a batch sorted by chain carries
    // them in arbitrary order.
    let cases: [(&str, Vec<BackfillChainRegistrationEntry>); 2] = [
        (
            "single registration",
            vec![wire::chain_registration_entry(ETHEREUM, 500, [0x11u8; 32])],
        ),
        (
            "three chains",
            vec![
                wire::chain_registration_entry(ETHEREUM, 900, [0x11u8; 32]),
                wire::chain_registration_entry(BSC, 12, [0x22u8; 32]),
                wire::chain_registration_entry(POLYGON, 4_000, [0x33u8; 32]),
            ],
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

#[test]
fn wrong_signer_writes_nothing() {
    let mollusk = mollusk();
    let entry = wire::chain_registration_entry(ETHEREUM, 500, [0x11u8; 32]);
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
    let entry = wire::chain_registration_entry(ETHEREUM, 500, [0x11u8; 32]);
    let one = Batch::new(&[entry]);
    let two = Batch::new(&[entry, wire::chain_registration_entry(BSC, 12, [0x22u8; 32])]);

    let existing_registration = {
        let mut accounts = one.accounts();
        accounts[2].1 = existing_pda_account(ChainRegistrationLayout::LEN);
        accounts
    };
    let existing_record = {
        let mut accounts = one.accounts();
        accounts[3].1 = existing_pda_account(RegisterChainLayout::LEN);
        accounts
    };
    let (swapped_accounts, swapped_metas) = {
        let (mut accounts, mut metas) = (one.accounts(), one.metas());
        accounts.swap(2, 3);
        metas.swap(2, 3);
        (accounts, metas)
    };

    let mut cases: Vec<Case> = vec![
        Case {
            label: "chain already registered",
            data: one.data(),
            accounts: existing_registration,
            metas: one.metas(),
            expected: GlobalAccountantError::InvalidPda as u64,
        },
        Case {
            label: "installing sequence already recorded",
            data: one.data(),
            accounts: existing_record,
            metas: one.metas(),
            expected: GlobalAccountantError::InvalidPda as u64,
        },
        Case {
            label: "registration and record PDAs swapped",
            data: one.data(),
            accounts: swapped_accounts,
            metas: swapped_metas,
            expected: GlobalAccountantError::InvalidPda as u64,
        },
    ];
    // Remaining accounts must be exactly two per entry.
    for (label, keep) in [("one PDA short", 5usize), ("only the first pair", 4)] {
        cases.push(Case {
            label,
            data: two.data(),
            accounts: two.accounts()[..keep].to_vec(),
            metas: two.metas()[..keep].to_vec(),
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        });
    }

    for case in cases {
        let result = submit(&mollusk, &case.data, &case.accounts, case.metas);
        assert_error(&result, case.expected, case.label);
    }
}
