//! `BackfillTransceiverHub`: one `TransceiverHubLayout` PDA per wormchain
//! `transceiver_to_hub` row. The rows carry hubs and the spokes adopted into them, so the
//! operational `register_peer` finds both sides of a pair at cutover.

use accountant_operational_core::support::pda;
use anchor_lang::error::ErrorCode as AnchorError;
use global_accountant_definitions::ntt_global_accountant_backfill::Instruction;
use global_accountant_definitions::{
    BackfillTransceiverHubEntry, GlobalAccountantError, TransceiverHubKey, TransceiverHubLayout,
};
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::result::InstructionResult;
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction as SolanaInstruction};
use solana_pubkey::Pubkey;

use crate::common::*;

const SOLANA: u16 = 1;
const ETHEREUM: u16 = 2;
const POLYGON: u16 = 5;
const HUB: [u8; 32] = [0x7Bu8; 32];
const SPOKE: [u8; 32] = [0x7Au8; 32];
const OTHER: [u8; 32] = [0x7Cu8; 32];

fn hub_pda(entry: &BackfillTransceiverHubEntry) -> Pubkey {
    pda::derive(
        &program_id(),
        &TransceiverHubKey::new(entry.chain(), entry.address),
    )
    .0
}

/// Accounts: payer, system program, then one `TransceiverHub` PDA per entry in wire order.
struct Batch {
    signer: Pubkey,
    entries: Vec<BackfillTransceiverHubEntry>,
}

impl Batch {
    fn new(entries: &[BackfillTransceiverHubEntry]) -> Self {
        Self::signed_by(ntt_test_authority_pubkey(), entries)
    }

    fn signed_by(signer: Pubkey, entries: &[BackfillTransceiverHubEntry]) -> Self {
        Self {
            signer,
            entries: entries.to_vec(),
        }
    }

    fn data(&self) -> Vec<u8> {
        wire::encode_transceiver_hub_batch(Instruction::BackfillTransceiverHub as u8, &self.entries)
    }

    fn accounts(&self) -> Vec<(Pubkey, Account)> {
        let mut accounts = vec![
            (self.signer, system_owned_account(10_000_000_000)),
            keyed_account_for_system_program(),
        ];
        accounts.extend(
            self.entries
                .iter()
                .map(|entry| (hub_pda(entry), uninitialised_pda_account())),
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
                .map(|entry| AccountMeta::new(hub_pda(entry), false)),
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

/// The PDA holds the layout `register_hub` and `register_peer` build from the same
/// constructor, rent-exempt at the minimum.
fn assert_written(
    mollusk: &Mollusk,
    result: &InstructionResult,
    entry: &BackfillTransceiverHubEntry,
    label: &str,
) {
    let account = find_account(&result.resulting_accounts, &hub_pda(entry));
    assert_eq!(account.owner, program_id(), "{label}: owner");
    assert_eq!(
        account.data.len(),
        TransceiverHubLayout::LEN,
        "{label}: len"
    );
    assert_eq!(
        layout::<TransceiverHubLayout>(account),
        TransceiverHubLayout::new(
            TransceiverHubKey::new(entry.chain(), entry.address),
            TransceiverHubKey::new(entry.hub_chain(), entry.hub_address),
        ),
        "{label}: layout"
    );
    assert_eq!(
        account.lamports,
        mollusk
            .sysvars
            .rent
            .minimum_balance(TransceiverHubLayout::LEN),
        "{label}: lamports"
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
fn writes_transceiver_hub_pdas() {
    let mollusk = mollusk();
    // `count` is one wire byte (255 max), but the 64-entry instruction trace binds first:
    // each PDA creation is one System Program CPI. Measured by binary search: 63 entries
    // pass, 64 raises `MaxInstructionTraceLengthExceeded`.
    let trace_bound: Vec<BackfillTransceiverHubEntry> = (0u16..63)
        .map(|i| {
            let mut address = [0u8; 32];
            address[30..].copy_from_slice(&i.to_be_bytes());
            wire::transceiver_hub_entry(ETHEREUM, address, SOLANA, HUB)
        })
        .collect();

    let cases: [(&str, Vec<BackfillTransceiverHubEntry>); 4] = [
        (
            "self-referential hub",
            vec![wire::transceiver_hub_entry(SOLANA, HUB, SOLANA, HUB)],
        ),
        (
            "spoke under another chain's hub",
            vec![wire::transceiver_hub_entry(ETHEREUM, SPOKE, SOLANA, HUB)],
        ),
        (
            "hub and spoke in one batch",
            vec![
                wire::transceiver_hub_entry(SOLANA, HUB, SOLANA, HUB),
                wire::transceiver_hub_entry(ETHEREUM, SPOKE, SOLANA, HUB),
            ],
        ),
        ("63 entries, the instruction-trace bound", trace_bound),
    ];

    for (label, entries) in cases {
        let batch = Batch::new(&entries);
        let result = batch.submit(&mollusk);
        assert_success(&result, label);
        for entry in &entries {
            assert_written(&mollusk, &result, entry, label);
        }
    }
}

/// A prefunded PDA takes `create_pda_allow_prefund`'s top-up path, so a cheap lamport
/// transfer cannot block the migration.
#[test]
fn prefunded_pda_is_allocated_and_assigned() {
    let mollusk = mollusk();
    const OVER_FUNDED: u64 = 10_000_000;
    let entry = wire::transceiver_hub_entry(SOLANA, HUB, SOLANA, HUB);
    let batch = Batch::new(&[entry]);
    let mut accounts = batch.accounts();
    accounts[2].1 = system_owned_account(OVER_FUNDED);

    let result = submit(&mollusk, &batch.data(), &accounts, batch.metas());
    assert_success(&result, "prefunded PDA");

    let account = find_account(&result.resulting_accounts, &hub_pda(&entry));
    assert_eq!(account.owner, program_id(), "owner");
    assert_eq!(account.data.len(), TransceiverHubLayout::LEN, "len");
    assert_eq!(
        layout::<TransceiverHubLayout>(account),
        TransceiverHubLayout::new(
            TransceiverHubKey::new(SOLANA, HUB),
            TransceiverHubKey::new(SOLANA, HUB),
        ),
        "layout"
    );
    assert_eq!(account.lamports, OVER_FUNDED, "lamports");
}

/// Create-only: `create_pda_allow_prefund` rejects an initialised PDA, so a lagging
/// orchestrator cursor cannot re-apply a landed batch.
#[test]
fn resubmission_rejected() {
    let mollusk = mollusk();
    let entry = wire::transceiver_hub_entry(SOLANA, HUB, SOLANA, HUB);
    let batch = Batch::new(&[entry]);

    let first = batch.submit(&mollusk);
    assert_success(&first, "first submit");
    let written = find_account(&first.resulting_accounts, &hub_pda(&entry)).clone();

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
    assert_eq!(
        find_account(&second.resulting_accounts, &hub_pda(&entry)),
        &written,
        "resubmission leaves the record untouched"
    );
}

#[test]
fn rejects() {
    let mollusk = mollusk();
    let entry = wire::transceiver_hub_entry(SOLANA, HUB, SOLANA, HUB);
    let one = Batch::new(&[entry]);
    let two = Batch::new(&[
        entry,
        wire::transceiver_hub_entry(ETHEREUM, SPOKE, SOLANA, HUB),
    ]);

    let descending = Batch::new(&[
        wire::transceiver_hub_entry(POLYGON, OTHER, SOLANA, HUB),
        wire::transceiver_hub_entry(ETHEREUM, SPOKE, SOLANA, HUB),
    ]);
    let duplicate = Batch::new(&[
        wire::transceiver_hub_entry(ETHEREUM, SPOKE, SOLANA, HUB),
        wire::transceiver_hub_entry(ETHEREUM, SPOKE, ETHEREUM, SPOKE),
    ]);
    let wrong_signer = Batch::signed_by(Pubkey::new_from_array([0xDEu8; 32]), &[entry]);

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
    let (foreign_accounts, foreign_metas) = {
        let (mut accounts, mut metas) = (one.accounts(), one.metas());
        let foreign_pda = Pubkey::new_from_array([0x66u8; 32]);
        accounts[2] = (foreign_pda, uninitialised_pda_account());
        metas[2] = AccountMeta::new(foreign_pda, false);
        (accounts, metas)
    };

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
            label: "descending key order",
            data: descending.data(),
            accounts: descending.accounts(),
            metas: descending.metas(),
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "duplicate (chain, address)",
            data: duplicate.data(),
            accounts: duplicate.accounts(),
            metas: duplicate.metas(),
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "one hub PDA short of the wire count",
            data: two.data(),
            accounts: short_accounts,
            metas: short_metas,
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "data ends before the count byte",
            data: vec![Instruction::BackfillTransceiverHub as u8],
            accounts: one.accounts(),
            metas: one.metas(),
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "zero count",
            data: vec![Instruction::BackfillTransceiverHub as u8, 0],
            accounts: one.accounts(),
            metas: one.metas(),
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "trailing byte after the last entry",
            data: [one.data().as_slice(), &[0xFFu8]].concat(),
            accounts: one.accounts(),
            metas: one.metas(),
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "non-canonical hub PDA",
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
