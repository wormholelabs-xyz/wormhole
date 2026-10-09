//! On-chain verification that a batch's state is fully written. `run` calls
//! it for every batch the progress file claims before skipping it, and the submitter
//! calls it when the program rejects a batch as already written
//! (`AlreadyAccounted` for NoReplay, `InvalidPda` for the PDA kinds).
//!
//! `batch_is_written` returns `Ok(true)` only when every account a
//! batch writes exists, is owned by the program, and holds exactly the bytes
//! the handler writes (`Layout::new(..)` from the definitions crate, padding
//! included). NoReplay: each bucket is owned by the NoReplay program and has
//! every batch sequence marked. Any other state returns `Ok(false)`: resubmit.

use anyhow::Result;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_pubkey::Pubkey;

use crate::accounts::fetch_accounts;
use crate::batcher::TxBatch;
use crate::expected_accounts::{
    balance_account, chain_registration_account, modify_balance_account, register_chain_account,
    transceiver_hub_account, transceiver_peer_account, PdaAccount,
};
use crate::noreplay::{fetch_buckets, group_by_bucket, BucketAccount};
use crate::snapshot::TransferRow;
use crate::tx_builder::BackfillTarget;

/// Whether it is safe to skip resubmitting `batch`. Any doubt resolves to
/// `Ok(false)` (resubmit).
pub async fn batch_is_written(
    rpc: &RpcClient,
    target: &BackfillTarget,
    batch: &TxBatch,
) -> Result<bool> {
    let expected: Vec<(Pubkey, Vec<u8>)> = match batch {
        TxBatch::BackfillNoReplay(transfers) => {
            return noreplay_confirmed(rpc, target, transfers).await
        }
        TxBatch::BackfillBalance(e) => written(target, e.iter().map(balance_account)),
        TxBatch::BackfillModifyBalance(e) => written(target, e.iter().map(modify_balance_account)),
        TxBatch::BackfillChainRegistration(e) => {
            let mut out = written(target, e.iter().map(chain_registration_account));
            out.extend(written(target, e.iter().map(register_chain_account)));
            out
        }
        TxBatch::BackfillTransceiverHub(e) => {
            written(target, e.iter().map(transceiver_hub_account))
        }
        TxBatch::BackfillTransceiverPeer(e) => {
            written(target, e.iter().map(transceiver_peer_account))
        }
    };
    let keys: Vec<Pubkey> = expected.iter().map(|(pda, _)| *pda).collect();
    let accounts = fetch_accounts(rpc, &keys).await?;
    Ok(expected.iter().zip(accounts).all(|((_, bytes), account)| {
        matches!(account, Some((owner, data)) if owner == target.program_id && data == *bytes)
    }))
}

/// `(pda, bytes the handler writes there)` per layout.
fn written<L: PdaAccount>(
    target: &BackfillTarget,
    layouts: impl Iterator<Item = L>,
) -> Vec<(Pubkey, Vec<u8>)> {
    layouts
        .map(|l| {
            (
                l.address(&target.program_id),
                bytemuck::bytes_of(&l).to_vec(),
            )
        })
        .collect()
}

async fn noreplay_confirmed(
    rpc: &RpcClient,
    target: &BackfillTarget,
    transfers: &[TransferRow],
) -> Result<bool> {
    let grouped = group_by_bucket(transfers);
    let buckets: Vec<_> = grouped.keys().copied().collect();
    let fetched = fetch_buckets(
        rpc,
        &target.noreplay_program,
        &target.noreplay_authority,
        &buckets,
    )
    .await?;
    Ok(grouped
        .values()
        .zip(fetched)
        .all(|(sequences, (_, state))| match state {
            BucketAccount::Bitmap(bitmap) => sequences.iter().all(|&s| bitmap.is_marked(s)),
            BucketAccount::Missing | BucketAccount::Foreign(_) | BucketAccount::Malformed(_) => {
                false
            }
        }))
}
