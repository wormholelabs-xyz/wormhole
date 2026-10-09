//! Batched account reads.

use anyhow::{ensure, Context, Result};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_pubkey::Pubkey;

/// `getMultipleAccounts` accepts at most 100 keys per call.
pub const GET_MULTIPLE_ACCOUNTS_LIMIT: usize = 100;

/// `(owner, data)` per key, in key order; `None` where the account does not exist.
pub async fn fetch_accounts(
    rpc: &RpcClient,
    keys: &[Pubkey],
) -> Result<Vec<Option<(Pubkey, Vec<u8>)>>> {
    let mut out = Vec::with_capacity(keys.len());
    for batch in keys.chunks(GET_MULTIPLE_ACCOUNTS_LIMIT) {
        let accounts = rpc
            .get_multiple_accounts(batch)
            .await
            .context("getMultipleAccounts")?;
        ensure!(
            accounts.len() == batch.len(),
            "getMultipleAccounts returned {} entries for {} keys",
            accounts.len(),
            batch.len()
        );
        out.extend(accounts.into_iter().map(|a| a.map(|a| (a.owner, a.data))));
    }
    Ok(out)
}
