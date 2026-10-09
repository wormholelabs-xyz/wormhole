//! NoReplay buckets: grouping, PDA derivation and a batched read, shared by
//! the builder, the resume check, reconcile and ga-parity.

use std::collections::BTreeMap;

use anyhow::Result;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_pubkey::Pubkey;

use global_accountant_definitions::{
    NoReplayBitmapAccount, NoReplayNamespace, NOREPLAY_AUTHORITY_SEED_PREFIX,
};

use crate::accounts::fetch_accounts;
use crate::snapshot::TransferRow;

/// One bitmap PDA: `(emitter_chain, emitter_address, bucket_index)`.
pub type BucketId = (u16, [u8; 32], u64);

pub fn bucket_of(t: &TransferRow) -> BucketId {
    (
        t.chain,
        t.emitter,
        NoReplayBitmapAccount::bucket_index(t.sequence),
    )
}

/// Sequences per bucket, ascending by bucket.
pub fn group_by_bucket<'a>(
    transfers: impl IntoIterator<Item = &'a TransferRow>,
) -> BTreeMap<BucketId, Vec<u64>> {
    let mut out: BTreeMap<BucketId, Vec<u64>> = BTreeMap::new();
    for t in transfers {
        out.entry(bucket_of(t)).or_default().push(t.sequence);
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
    let (chain, emitter, index) = *bucket;
    let namespace = NoReplayNamespace::new(chain, emitter);
    let (head, tail) = namespace.seed_chunks();
    Pubkey::find_program_address(
        &[authority.as_ref(), head, tail, &index.to_le_bytes()],
        noreplay_program,
    )
    .0
}

/// Bitmap PDA holding `sequence` for `(chain, emitter)`.
pub fn derive_noreplay_bucket(
    noreplay_program: &Pubkey,
    authority: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> Pubkey {
    let bucket = (
        chain,
        *emitter,
        NoReplayBitmapAccount::bucket_index(sequence),
    );
    derive_bucket_pda(noreplay_program, authority, &bucket)
}

/// State of one bucket PDA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BucketAccount {
    Missing,
    /// Owned by a program other than NoReplay.
    Foreign(Pubkey),
    /// NoReplay-owned with a length other than `NoReplayBitmapAccount::LEN`.
    Malformed(usize),
    Bitmap(NoReplayBitmapAccount),
}

/// `(pda, state)` per bucket, in input order.
pub async fn fetch_buckets(
    rpc: &RpcClient,
    noreplay_program: &Pubkey,
    authority: &Pubkey,
    buckets: &[BucketId],
) -> Result<Vec<(Pubkey, BucketAccount)>> {
    let pdas: Vec<Pubkey> = buckets
        .iter()
        .map(|b| derive_bucket_pda(noreplay_program, authority, b))
        .collect();
    let accounts = fetch_accounts(rpc, &pdas).await?;
    Ok(pdas
        .into_iter()
        .zip(accounts)
        .map(|(pda, account)| {
            let state = match account {
                None => BucketAccount::Missing,
                Some((owner, _)) if owner != *noreplay_program => BucketAccount::Foreign(owner),
                Some((_, data)) => match NoReplayBitmapAccount::from_bytes(&data) {
                    Some(bitmap) => BucketAccount::Bitmap(*bitmap),
                    None => BucketAccount::Malformed(data.len()),
                },
            };
            (pda, state)
        })
        .collect())
}
