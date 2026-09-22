//! `BackfillNoReplay` against the pinned `solana_noreplay.so`: one `MarkUsedBulk` CPI per
//! unique `(chain, emitter, sequence / 1024)` bucket, one `ACCDGST\0` log per entry.

use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use anchor_lang::error::ErrorCode as AnchorError;
use global_accountant_backfill::BACKFILL_AUTHORITY;
use global_accountant_definitions::global_accountant_backfill::Instruction;
use global_accountant_definitions::{GlobalAccountantError, NoReplayBitmapAccount};
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::result::InstructionResult;
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction as SolanaInstruction};
use solana_pubkey::Pubkey;

use crate::common::wire::NoReplayEntry as Entry;
use crate::common::*;

fn bucket_pda(chain: u16, emitter: &[u8; 32], sequence: u64) -> Pubkey {
    derive_bucket_pda(
        &noreplay_authority_pda(&program_id()),
        chain,
        emitter,
        sequence,
    )
    .0
}

fn entry(chain: u16, emitter: [u8; 32], sequence: u64, digest: [u8; 32]) -> Entry {
    Entry {
        chain,
        emitter,
        sequence,
        digest,
    }
}

/// Accounts: payer, NoReplay program, NoReplay authority PDA, system program, then one
/// bucket PDA per unique bucket in walk order.
struct Batch {
    signer: Pubkey,
    entries: Vec<Entry>,
}

impl Batch {
    fn new(entries: &[Entry]) -> Self {
        Self::signed_by(test_authority_pubkey(), entries)
    }

    fn signed_by(signer: Pubkey, entries: &[Entry]) -> Self {
        Self {
            signer,
            entries: entries.to_vec(),
        }
    }

    /// Unique buckets in walk order: a change of `(chain, emitter)` or of bucket index ends
    /// the current bucket, exactly as the handler's flush does.
    fn buckets(&self) -> Vec<Pubkey> {
        let mut buckets = Vec::new();
        let mut previous: Option<(u16, [u8; 32], u64)> = None;
        for e in &self.entries {
            let key = (
                e.chain,
                e.emitter,
                NoReplayBitmapAccount::bucket_index(e.sequence),
            );
            if previous != Some(key) {
                buckets.push(bucket_pda(e.chain, &e.emitter, e.sequence));
                previous = Some(key);
            }
        }
        buckets
    }

    fn data(&self) -> Vec<u8> {
        wire::encode_noreplay_batch(Instruction::BackfillNoReplay as u8, &self.entries)
    }

    fn accounts(&self) -> Vec<(Pubkey, Account)> {
        let mut accounts = vec![
            (self.signer, system_owned_account(10_000_000_000)),
            keyed_account_for_noreplay_program(),
            (
                noreplay_authority_pda(&program_id()),
                uninitialised_pda_account(),
            ),
            keyed_account_for_system_program(),
        ];
        accounts.extend(
            self.buckets()
                .into_iter()
                .map(|bucket| (bucket, noreplay_bucket_unmarked())),
        );
        accounts
    }

    fn metas(&self) -> Vec<AccountMeta> {
        let mut metas = vec![
            AccountMeta::new(self.signer, true),
            AccountMeta::new_readonly(noreplay_program_id(), false),
            AccountMeta::new_readonly(noreplay_authority_pda(&program_id()), false),
            AccountMeta::new_readonly(system_program_id(), false),
        ];
        metas.extend(
            self.buckets()
                .into_iter()
                .map(|bucket| AccountMeta::new(bucket, false)),
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

struct Case {
    label: &'static str,
    data: Vec<u8>,
    accounts: Vec<(Pubkey, Account)>,
    metas: Vec<AccountMeta>,
    expected: u64,
}

/// The `.so` under test is built with the harness test key. A mismatch means the artifact
/// carries an operator key and the suites below assert nothing.
#[test]
fn backfill_authority_const_matches_test_keypair() {
    assert_eq!(
        BACKFILL_AUTHORITY,
        test_authority_pubkey().to_bytes(),
        "BACKFILL_AUTHORITY drift"
    );
}

#[test]
fn marks_noreplay_buckets() {
    let mollusk = mollusk();
    let emitter = [0x22u8; 32];
    // `entry_count` is one wire byte; 255 is its maximum.
    let full_bucket: Vec<Entry> = (0u64..255)
        .map(|i| entry(ETHEREUM, emitter, i, [i as u8; 32]))
        .collect();
    // `MAX_BATCH_ENTRIES` does not bind this arm: the cost is one CPI per bucket plus a
    // nested create for a new bucket, and Solana caps an instruction trace at 64 entries, so
    // 30 groups is the practical ceiling.
    let many_groups: Vec<Entry> = (0u16..30)
        .map(|i| {
            let mut group_emitter = [0u8; 32];
            group_emitter[30..].copy_from_slice(&i.to_be_bytes());
            entry(ETHEREUM, group_emitter, 1, [0xEEu8; 32])
        })
        .collect();

    // (label, entries, unique buckets)
    let cases: [(&str, Vec<Entry>, usize); 6] = [
        (
            "single entry",
            vec![entry(ETHEREUM, [0x11u8; 32], 42, [0x77u8; 32])],
            1,
        ),
        (
            "three sequences in one bucket",
            vec![
                entry(ETHEREUM, emitter, 10, [0xaau8; 32]),
                entry(ETHEREUM, emitter, 200, [0xbbu8; 32]),
                entry(ETHEREUM, emitter, 800, [0xccu8; 32]),
            ],
            1,
        ),
        (
            "sequences spanning two buckets",
            vec![
                entry(ETHEREUM, emitter, 10, [0xaau8; 32]),
                entry(ETHEREUM, emitter, 500, [0xbbu8; 32]),
                entry(ETHEREUM, emitter, 1500, [0xccu8; 32]),
            ],
            2,
        ),
        (
            "bucket boundary 1023 and 1024",
            vec![
                entry(ETHEREUM, emitter, 1023, [0xaau8; 32]),
                entry(ETHEREUM, emitter, 1024, [0xbbu8; 32]),
            ],
            2,
        ),
        ("255 entries in one bucket", full_bucket, 1),
        ("30 emitters, one bucket each", many_groups, 30),
    ];

    for (label, entries, unique_buckets) in cases {
        let batch = Batch::new(&entries);
        assert_eq!(batch.buckets().len(), unique_buckets, "{label}: buckets");

        let result = batch.submit(&mollusk);
        assert_success(&result, label);
        for e in &entries {
            let bucket = find_account(
                &result.resulting_accounts,
                &bucket_pda(e.chain, &e.emitter, e.sequence),
            );
            assert_bucket_marked(bucket, e.sequence);
        }
    }
}

#[test]
fn rejects() {
    let mollusk = mollusk();
    let emitter = [0x11u8; 32];
    let one = Batch::new(&[entry(ETHEREUM, emitter, 42, [0x77u8; 32])]);
    let two_buckets = Batch::new(&[
        entry(ETHEREUM, [0x66u8; 32], 10, [0xaau8; 32]),
        entry(ETHEREUM, [0x66u8; 32], 1500, [0xbbu8; 32]),
    ]);
    let three_buckets = Batch::new(&[
        entry(ETHEREUM, [0x44u8; 32], 0, [0xaau8; 32]),
        entry(ETHEREUM, [0x44u8; 32], 1500, [0xbbu8; 32]),
        entry(ETHEREUM, [0x44u8; 32], 3000, [0xccu8; 32]),
    ]);

    let wrong_signer = Batch::signed_by(Pubkey::new_from_array([0xDEu8; 32]), &one.entries);
    let unsigned_metas = {
        let mut metas = one.metas();
        metas[0] = AccountMeta::new(one.signer, false);
        metas
    };
    let (spoofed_accounts, spoofed_metas) = {
        let (mut accounts, mut metas) = (one.accounts(), one.metas());
        let spoofed = Pubkey::new_from_array([0x55u8; 32]);
        accounts[2] = (spoofed, uninitialised_pda_account());
        metas[2] = AccountMeta::new_readonly(spoofed, false);
        (accounts, metas)
    };
    // Descending group keys: a well-formed builder cannot emit these.
    let descending = wire::encode_noreplay_batch_raw(
        Instruction::BackfillNoReplay as u8,
        &[
            (5u16, [0x11u8; 32], &[(1u64, [0xaau8; 32])]),
            (2u16, [0x22u8; 32], &[(1u64, [0xbbu8; 32])]),
        ],
    );
    let (mid_flush_accounts, mid_flush_metas) = {
        let (mut accounts, mut metas) = (three_buckets.accounts(), three_buckets.metas());
        accounts.truncate(4 + 1);
        metas.truncate(4 + 1);
        (accounts, metas)
    };
    let (trailing_accounts, trailing_metas) = {
        let (mut accounts, mut metas) = (one.accounts(), one.metas());
        let dead_bucket = Pubkey::new_from_array([0x99u8; 32]);
        accounts.push((dead_bucket, noreplay_bucket_unmarked()));
        metas.push(AccountMeta::new(dead_bucket, false));
        (accounts, metas)
    };
    let (swapped_accounts, swapped_metas) = {
        let (mut accounts, mut metas) = (two_buckets.accounts(), two_buckets.metas());
        accounts.swap(4, 5);
        metas.swap(4, 5);
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
            label: "payer and NoReplay program alone",
            data: one.data(),
            accounts: one.accounts()[..2].to_vec(),
            metas: one.metas()[..2].to_vec(),
            expected: AnchorError::AccountNotEnoughKeys as u64,
        },
        Case {
            label: "spoofed NoReplay authority PDA",
            data: one.data(),
            accounts: spoofed_accounts,
            metas: spoofed_metas,
            expected: GlobalAccountantError::InvalidPda as u64,
        },
        Case {
            label: "data ends before the group count",
            data: vec![Instruction::BackfillNoReplay as u8],
            accounts: one.accounts(),
            metas: one.metas(),
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "descending group keys",
            data: descending,
            accounts: one.accounts()[..4].to_vec(),
            metas: one.metas()[..4].to_vec(),
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "bucket accounts run out mid-flush",
            data: three_buckets.data(),
            accounts: mid_flush_accounts,
            metas: mid_flush_metas,
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "no bucket account for the final flush",
            data: one.data(),
            accounts: one.accounts()[..4].to_vec(),
            metas: one.metas()[..4].to_vec(),
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "bucket account beyond the walk",
            data: one.data(),
            accounts: trailing_accounts,
            metas: trailing_metas,
            expected: GlobalAccountantError::InvalidInstructionData as u64,
        },
        Case {
            label: "bucket accounts swapped",
            data: two_buckets.data(),
            accounts: swapped_accounts,
            metas: swapped_metas,
            expected: GlobalAccountantError::InvalidPda as u64,
        },
    ];

    for case in cases {
        let result = submit(&mollusk, &case.data, &case.accounts, case.metas);
        assert_error(&result, case.expected, case.label);
    }
}

/// `mark_used_bulk` builds its CPI against the compiled-in `NOREPLAY_PROGRAM_ID`, so
/// replacing the `noreplay_program` slot removes the real program from every slot and
/// mollusk panics naming the compiled-in id.
#[test]
fn cpi_target_is_compiled_in_not_caller_supplied() {
    let mollusk = mollusk();
    let batch = Batch::new(&[entry(ETHEREUM, [0x11u8; 32], 42, [0x77u8; 32])]);
    let mut accounts = batch.accounts();
    accounts[1] = keyed_account_for_system_program();

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        submit(&mollusk, &batch.data(), &accounts, batch.metas())
    }));
    std::panic::set_hook(previous_hook);

    let payload = outcome.expect_err(
        "mollusk must panic once the real NoReplay program is absent from every slot; \
         completing would mean the CPI target came from the caller",
    );
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(
        message.contains(&noreplay_program_id().to_string()),
        "panic must name the compiled-in NoReplay program id, got: {message}"
    );
}
