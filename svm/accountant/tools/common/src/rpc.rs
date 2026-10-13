//! JSON-RPC client and batched account reads.

use std::time::Duration;

use anyhow::{ensure, Context, Result};
use reqwest::header::{HeaderValue, ORIGIN};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client::http_sender::HttpSender;
use solana_rpc_client::rpc_client::RpcClientConfig;

/// Per-request timeout. `getProgramAccounts` on a large program is the slow call.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// `getMultipleAccounts` accepts at most 100 keys per call.
pub const MAX_KEYS_PER_GET_MULTIPLE_ACCOUNTS: usize = 100;

/// `origin` is sent as the `Origin` header; the internal RPC proxy requires an
/// allow-listed one (`https://portalbridge.com`).
pub fn client(url: &str, origin: Option<&str>, commitment: CommitmentConfig) -> Result<RpcClient> {
    let mut headers = HttpSender::default_headers();
    if let Some(origin) = origin {
        headers.insert(
            ORIGIN,
            HeaderValue::from_str(origin).context("parse --origin")?,
        );
    }
    let http = reqwest::Client::builder()
        .default_headers(headers)
        .timeout(REQUEST_TIMEOUT)
        .pool_idle_timeout(REQUEST_TIMEOUT)
        .build()
        .context("build HTTP client")?;
    Ok(RpcClient::new_sender(
        HttpSender::new_with_client(url, http),
        RpcClientConfig::with_commitment(commitment),
    ))
}

/// `(owner, data)` of one account; `None` where the account does not exist.
pub type FetchedAccount = Option<(Pubkey, Vec<u8>)>;

/// One [`FetchedAccount`] per key, in key order.
pub async fn fetch_accounts(rpc: &RpcClient, keys: &[Pubkey]) -> Result<Vec<FetchedAccount>> {
    let mut out = Vec::with_capacity(keys.len());
    for batch in keys.chunks(MAX_KEYS_PER_GET_MULTIPLE_ACCOUNTS) {
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
    ensure!(
        out.len() == keys.len(),
        "fetched {} of {} keys",
        out.len(),
        keys.len()
    );
    Ok(out)
}
