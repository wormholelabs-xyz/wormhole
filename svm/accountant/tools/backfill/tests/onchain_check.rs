//! `unwritten_batches`: the resume check reads many batches per
//! `getMultipleAccounts` call and reports exactly the unwritten ones, in order.

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use global_accountant_definitions::{NoReplayBitmapAccount, Uint256};
use solana_pubkey::Pubkey;
use svm_accountant_backfill::batcher::TxBatch;
use svm_accountant_backfill::onchain_check::{expected_writes, unwritten_batches, ExpectedWrite};
use svm_accountant_backfill::target::{AccountantProgram, BackfillTarget};
use svm_accountant_tools_common::accountant::Accountant;
use svm_accountant_tools_common::ids::{Address, ChainId, Digest, Sequence};
use svm_accountant_tools_common::snapshot::{BalanceRow, TransferRow};

use common::mock_rpc::MockRpc;

fn wtt_target() -> BackfillTarget {
    common::target(
        Accountant::Wtt,
        Accountant::Wtt.program_id(),
        Accountant::Wtt.authority(),
    )
}

fn balance_batch(index: u16) -> TxBatch {
    let mut token_address = [0u8; 32];
    token_address[..2].copy_from_slice(&index.to_be_bytes());
    TxBatch::BackfillBalance(vec![BalanceRow {
        chain: ChainId(1),
        token_chain: ChainId(2),
        token_address: Address(token_address),
        balance: Uint256::from_u128(u128::from(index)),
    }])
}

fn noreplay_batch(sequences: &[u64]) -> TxBatch {
    TxBatch::BackfillNoReplay(
        sequences
            .iter()
            .map(|&sequence| TransferRow {
                chain: ChainId(1),
                emitter: Address([0xEC; 32]),
                sequence: Sequence(sequence),
                digest: Digest([0xAA; 32]),
                amount: Uint256::ZERO,
                token_chain: ChainId(1),
                token_address: Address([0; 32]),
                recipient_chain: ChainId(2),
            })
            .collect(),
    )
}

/// Chain state holding every write of `batches` except those of `unwritten`.
/// `marked_only` limits a bucket to those sequences.
fn chain_state(
    target: &BackfillTarget,
    batches: &[TxBatch],
    unwritten: &[usize],
    marked_only: &HashMap<usize, Vec<u64>>,
) -> HashMap<Pubkey, (Pubkey, Vec<u8>)> {
    let mut accounts = HashMap::new();
    for (index, batch) in batches.iter().enumerate() {
        if unwritten.contains(&index) && !marked_only.contains_key(&index) {
            continue;
        }
        for write in expected_writes(target, batch) {
            match write {
                ExpectedWrite::Account { pda, bytes } => {
                    accounts.insert(pda, (target.program_id(), bytes));
                }
                ExpectedWrite::Bucket { pda, sequences } => {
                    let mut bitmap: NoReplayBitmapAccount = bytemuck::Zeroable::zeroed();
                    let marked = marked_only
                        .get(&index)
                        .cloned()
                        .unwrap_or_else(|| sequences.iter().map(|s| s.0).collect());
                    for sequence in marked {
                        let bit = NoReplayBitmapAccount::bit_index(sequence);
                        bitmap.bitmap[bit / 8] |= 1 << (bit % 8);
                    }
                    let data = bytemuck::bytes_of(&bitmap).to_vec();
                    accounts.insert(pda, (target.noreplay_program(), data));
                }
            }
        }
    }
    accounts
}

#[tokio::test]
async fn reports_exactly_the_unwritten_batches_with_batched_reads() {
    let target = wtt_target();
    // Index 0: fully marked bucket. Index 1: bucket with one of two bits marked.
    // Indices 2..252: one balance each.
    let mut batches = vec![noreplay_batch(&[5, 6]), noreplay_batch(&[2048, 2049])];
    batches.extend((0..250u16).map(balance_batch));
    let unwritten = vec![1, 9, 122, 203];
    let marked_only = HashMap::from([(1usize, vec![2048u64])]);
    let key_count: usize = batches
        .iter()
        .map(|b| expected_writes(&target, b).len())
        .sum();
    assert_eq!(key_count, 252, "one key per batch");

    // `(concurrency, getMultipleAccounts calls)`. A window holds
    // `concurrency * 100` keys and closes on a batch boundary.
    let cases = [(1usize, 3usize), (4, 3), (256, 3)];
    for (concurrency, calls) in cases {
        let mock =
            MockRpc::start_with_accounts(chain_state(&target, &batches, &unwritten, &marked_only));
        let rpc = Arc::new(mock.client());
        let found = unwritten_batches(&rpc, &target, &batches, concurrency)
            .await
            .expect("check");
        assert_eq!(
            found, unwritten,
            "concurrency {concurrency}: unwritten batches"
        );
        assert_eq!(
            mock.get_multiple_accounts_calls(),
            calls,
            "concurrency {concurrency}: calls"
        );
    }
}

#[tokio::test]
async fn reports_every_batch_on_an_empty_chain_and_none_on_an_empty_plan() {
    let target = wtt_target();
    let batches: Vec<TxBatch> = (0..3u16).map(balance_batch).collect();
    let mock = MockRpc::start_with_accounts(HashMap::new());
    let rpc = Arc::new(mock.client());
    assert_eq!(
        unwritten_batches(&rpc, &target, &batches, 2).await.unwrap(),
        vec![0, 1, 2]
    );
    assert!(unwritten_batches(&rpc, &target, &[], 2)
        .await
        .unwrap()
        .is_empty());
}
