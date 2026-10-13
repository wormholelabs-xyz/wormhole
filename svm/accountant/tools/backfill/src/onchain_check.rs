//! On-chain verification that a batch's state is fully written. `run` calls
//! [`unwritten_batches`] for every batch the progress file claims before skipping
//! it, and the submitter calls [`batch_is_written`] when the program rejects a
//! batch as already written (`AlreadyAccounted` for NoReplay, `InvalidPda` for the PDA kinds).
//!
//! A batch is written only when every account it writes exists, is owned by
//! the program, and holds exactly the bytes the handler writes
//! (`Layout::new(..)` from the definitions crate, padding included). NoReplay:
//! each bucket is owned by the NoReplay program and has every batch sequence
//! marked. Any other state means: resubmit.

use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_pubkey::Pubkey;
use tokio::task::JoinSet;

use svm_accountant_tools_common::ids::Sequence;
use svm_accountant_tools_common::layouts::{
    balance_layout, chain_registration_layout, modify_balance_layout, register_chain_layout,
    transceiver_hub_layout, transceiver_peer_layout, PdaAccount,
};
use svm_accountant_tools_common::noreplay::{derive_bucket_pda, group_by_bucket, BucketState};
use svm_accountant_tools_common::rpc::{
    fetch_accounts, FetchedAccount, MAX_KEYS_PER_GET_MULTIPLE_ACCOUNTS,
};

use crate::batcher::TxBatch;
use crate::target::BackfillTarget;

/// One account a batch writes, and what it must hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpectedWrite {
    /// Program-owned PDA with exactly these bytes.
    Account { pda: Pubkey, bytes: Vec<u8> },
    /// NoReplay bucket with every sequence marked.
    Bucket {
        pda: Pubkey,
        sequences: Vec<Sequence>,
    },
}

impl ExpectedWrite {
    pub fn pda(&self) -> Pubkey {
        match self {
            Self::Account { pda, .. } | Self::Bucket { pda, .. } => *pda,
        }
    }

    /// Whether the fetched `account` holds this write.
    fn holds(&self, target: &BackfillTarget, account: FetchedAccount) -> bool {
        match self {
            Self::Account { bytes, .. } => {
                matches!(account, Some((owner, data)) if owner == target.program_id() && data == *bytes)
            }
            Self::Bucket { sequences, .. } => {
                match BucketState::from_account(&target.noreplay_program(), account) {
                    BucketState::Bitmap(bitmap) => sequences.iter().all(|s| bitmap.is_marked(s.0)),
                    BucketState::Missing | BucketState::Foreign(_) | BucketState::Malformed(_) => {
                        false
                    }
                }
            }
        }
    }
}

/// Every account `batch` writes, in instruction order.
pub fn expected_writes(target: &BackfillTarget, batch: &TxBatch) -> Vec<ExpectedWrite> {
    assert!(!batch.is_empty(), "empty {} batch", batch.ix_name());
    let program_id = target.program_id();
    let account = |layout: &dyn PdaBytes| layout.write_at(&program_id);
    let writes: Vec<ExpectedWrite> = match batch {
        TxBatch::BackfillNoReplay(transfers) => group_by_bucket(transfers)
            .into_iter()
            .map(|(bucket, sequences)| ExpectedWrite::Bucket {
                pda: derive_bucket_pda(
                    &target.noreplay_program(),
                    &target.noreplay_authority(),
                    &bucket,
                ),
                sequences,
            })
            .collect(),
        TxBatch::BackfillBalance(e) => e.iter().map(|r| account(&balance_layout(r))).collect(),
        TxBatch::BackfillModifyBalance(e) => e
            .iter()
            .map(|r| account(&modify_balance_layout(r)))
            .collect(),
        TxBatch::BackfillChainRegistration(e) => e
            .iter()
            .flat_map(|r| {
                [
                    account(&chain_registration_layout(r)),
                    account(&register_chain_layout(r)),
                ]
            })
            .collect(),
        TxBatch::BackfillTransceiverHub(e) => e
            .iter()
            .map(|r| account(&transceiver_hub_layout(r)))
            .collect(),
        TxBatch::BackfillTransceiverPeer(e) => e
            .iter()
            .map(|r| account(&transceiver_peer_layout(r)))
            .collect(),
    };
    assert!(
        !writes.is_empty(),
        "{} batch writes nothing",
        batch.ix_name()
    );
    writes
}

/// Object-safe view of a [`PdaAccount`] layout: one closure serves every layout type.
trait PdaBytes {
    fn write_at(&self, program_id: &Pubkey) -> ExpectedWrite;
}

impl<L: PdaAccount> PdaBytes for L {
    fn write_at(&self, program_id: &Pubkey) -> ExpectedWrite {
        ExpectedWrite::Account {
            pda: self.address(program_id),
            bytes: bytemuck::bytes_of(self).to_vec(),
        }
    }
}

/// Whether it is safe to skip resubmitting `batch`. Any doubt resolves to
/// `Ok(false)` (resubmit).
pub async fn batch_is_written(
    rpc: &RpcClient,
    target: &BackfillTarget,
    batch: &TxBatch,
) -> Result<bool> {
    let writes = expected_writes(target, batch);
    let keys: Vec<Pubkey> = writes.iter().map(ExpectedWrite::pda).collect();
    let accounts = fetch_accounts(rpc, &keys).await?;
    ensure!(accounts.len() == writes.len(), "account fetch length");
    Ok(writes
        .iter()
        .zip(accounts)
        .all(|(write, account)| write.holds(target, account)))
}

/// Keys read per window of [`unwritten_batches`], per unit of concurrency.
const WINDOW_KEYS_PER_CALL: usize = MAX_KEYS_PER_GET_MULTIPLE_ACCOUNTS;

/// Indices of `batches` that are not fully written, ascending. Reads
/// [`MAX_KEYS_PER_GET_MULTIPLE_ACCOUNTS`] keys per call across batch
/// boundaries, at most `concurrency` calls at once, one window of batches at a time.
pub async fn unwritten_batches(
    rpc: &Arc<RpcClient>,
    target: &BackfillTarget,
    batches: &[TxBatch],
    concurrency: usize,
) -> Result<Vec<usize>> {
    assert!(concurrency > 0, "concurrency must be positive");
    let window_keys = concurrency * WINDOW_KEYS_PER_CALL;
    let mut unwritten = Vec::new();
    let mut checked = 0usize;
    let mut start = 0usize;
    // Bounded: each pass consumes at least one batch.
    while start < batches.len() {
        let mut writes: Vec<(usize, ExpectedWrite)> = Vec::new();
        let mut end = start;
        while end < batches.len() && (end == start || writes.len() < window_keys) {
            writes.extend(
                expected_writes(target, &batches[end])
                    .into_iter()
                    .map(|write| (end, write)),
            );
            end += 1;
        }
        let keys: Vec<Pubkey> = writes.iter().map(|(_, write)| write.pda()).collect();
        let accounts = fetch_concurrently(rpc, keys, concurrency).await?;
        ensure!(accounts.len() == writes.len(), "window fetch length");

        let mut written = vec![true; end - start];
        for ((index, write), account) in writes.iter().zip(accounts) {
            if !write.holds(target, account) {
                written[index - start] = false;
            }
        }
        for (offset, is_written) in written.into_iter().enumerate() {
            checked += 1;
            if !is_written {
                unwritten.push(start + offset);
            }
        }
        start = end;
    }
    assert_eq!(checked, batches.len(), "every batch checked exactly once");
    Ok(unwritten)
}

/// [`fetch_accounts`] over `keys`, one call per chunk, at most `concurrency`
/// calls in flight, results in key order.
async fn fetch_concurrently(
    rpc: &Arc<RpcClient>,
    keys: Vec<Pubkey>,
    concurrency: usize,
) -> Result<Vec<FetchedAccount>> {
    let chunks: Vec<Vec<Pubkey>> = keys
        .chunks(MAX_KEYS_PER_GET_MULTIPLE_ACCOUNTS)
        .map(<[Pubkey]>::to_vec)
        .collect();
    let mut results: Vec<Option<Vec<FetchedAccount>>> = vec![None; chunks.len()];
    let mut tasks = JoinSet::new();
    for (index, chunk) in chunks.into_iter().enumerate() {
        if tasks.len() == concurrency {
            let (done, accounts) = tasks.join_next().await.expect("non-empty")?;
            results[done] = Some(accounts?);
        }
        let rpc = Arc::clone(rpc);
        tasks.spawn(async move { (index, fetch_accounts(&rpc, &chunk).await) });
    }
    while let Some(joined) = tasks.join_next().await {
        let (done, accounts) = joined.context("account fetch task")?;
        results[done] = Some(accounts?);
    }
    let out: Vec<_> = results
        .into_iter()
        .flat_map(|chunk| chunk.expect("every chunk fetched"))
        .collect();
    ensure!(
        out.len() == keys.len(),
        "fetched {} of {} keys",
        out.len(),
        keys.len()
    );
    Ok(out)
}
