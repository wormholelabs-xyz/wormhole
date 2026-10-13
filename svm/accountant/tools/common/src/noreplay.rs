//! NoReplay buckets: grouping, PDA derivation and a batched read, shared by
//! the builder, the resume check and the parity check.

use std::collections::BTreeMap;

use anyhow::{ensure, Result};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_pubkey::Pubkey;

use global_accountant_definitions::{
    NoReplayBitmapAccount, NoReplayNamespace, NOREPLAY_AUTHORITY_SEED_PREFIX,
};

use crate::ids::{BucketId, EmitterKey, Sequence};
use crate::rpc::{fetch_accounts, FetchedAccount};
use crate::snapshot::TransferRow;

/// Bucket holding `sequence` for `emitter`.
pub fn bucket_id(emitter: EmitterKey, sequence: Sequence) -> BucketId {
    BucketId {
        emitter,
        index: NoReplayBitmapAccount::bucket_index(sequence.0),
    }
}

/// Sequences per bucket, ascending by bucket.
pub fn group_by_bucket<'a>(
    transfers: impl IntoIterator<Item = &'a TransferRow>,
) -> BTreeMap<BucketId, Vec<Sequence>> {
    let mut out: BTreeMap<BucketId, Vec<Sequence>> = BTreeMap::new();
    for t in transfers {
        out.entry(bucket_id(t.emitter_key(), t.sequence))
            .or_default()
            .push(t.sequence);
    }
    out
}

pub fn derive_noreplay_authority_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], program_id).0
}

/// Mirrors `solana_noreplay::pda::BitmapPdaSeeds`: `authority ‖
/// namespace[..32] ‖ namespace[32..] ‖ bucket_index_le`.
pub fn derive_bucket_pda(
    noreplay_program: &Pubkey,
    authority: &Pubkey,
    bucket: &BucketId,
) -> Pubkey {
    let namespace = NoReplayNamespace::new(bucket.emitter.chain.0, bucket.emitter.emitter.0);
    let (head, tail) = namespace.seed_chunks();
    Pubkey::find_program_address(
        &[authority.as_ref(), head, tail, &bucket.index.to_le_bytes()],
        noreplay_program,
    )
    .0
}

/// State of one bucket PDA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BucketState {
    Missing,
    /// Owned by a program other than NoReplay.
    Foreign(Pubkey),
    /// NoReplay-owned with a length other than `NoReplayBitmapAccount::LEN`.
    Malformed(usize),
    Bitmap(NoReplayBitmapAccount),
}

impl BucketState {
    /// Classify one fetched account.
    pub fn from_account(noreplay_program: &Pubkey, account: FetchedAccount) -> Self {
        match account {
            None => Self::Missing,
            Some((owner, _)) if owner != *noreplay_program => Self::Foreign(owner),
            Some((_, data)) => match NoReplayBitmapAccount::from_bytes(&data) {
                Some(bitmap) => Self::Bitmap(*bitmap),
                None => Self::Malformed(data.len()),
            },
        }
    }
}

/// `(pda, state)` per bucket, in input order.
pub async fn fetch_bucket_states(
    rpc: &RpcClient,
    noreplay_program: &Pubkey,
    authority: &Pubkey,
    buckets: &[BucketId],
) -> Result<Vec<(Pubkey, BucketState)>> {
    let pdas: Vec<Pubkey> = buckets
        .iter()
        .map(|b| derive_bucket_pda(noreplay_program, authority, b))
        .collect();
    let accounts = fetch_accounts(rpc, &pdas).await?;
    ensure!(accounts.len() == pdas.len(), "bucket fetch length");
    Ok(pdas
        .into_iter()
        .zip(accounts)
        .map(|(pda, account)| (pda, BucketState::from_account(noreplay_program, account)))
        .collect())
}
