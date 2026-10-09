//! JSON-RPC client. Every read uses `finalized`.

use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::header::{HeaderValue, ORIGIN};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::http_sender::HttpSender;
use solana_rpc_client::rpc_client::RpcClientConfig;

/// Per-request timeout. `getProgramAccounts` on a large program is the slow call.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// `origin` is sent as the `Origin` header; the internal RPC proxy requires an
/// allow-listed one (`https://portalbridge.com`).
pub fn client(url: &str, origin: Option<&str>) -> Result<RpcClient> {
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
        RpcClientConfig::with_commitment(CommitmentConfig::finalized()),
    ))
}
