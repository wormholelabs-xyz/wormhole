//! Concurrent RPC submission with retry and AlreadyAccounted-as-success.
//!
//! ## Policy
//!
//! - **AlreadyAccounted (custom 7)** → success. NoReplay's on-chain
//!   idempotency lets us safely re-send a tx that's already landed; the
//!   second submission rejects with this code and the orchestrator counts
//!   it as "already done, advance cursor".
//! - **UnauthorizedCaller (custom 3)** → halt. The configured signer does
//!   not match the program's compile-time `BACKFILL_AUTHORITY` — operator
//!   misconfig, no point retrying.
//! - **Other custom program errors** → halt. Logic errors are unrecoverable
//!   in the orchestrator's frame — the operator needs to inspect.
//! - **RPC / I/O / blockhash-expired / transient errors** → retry with
//!   exponential backoff, capped attempts.
//!
//! Each tx fetches its own blockhash before signing. Concurrent submission
//! through a `tokio::sync::Semaphore`; the empirical 16-way concurrency from
//! the at-scale probe is the default.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use solana_client::client_error::{ClientError, ClientErrorKind};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction::Transaction;
use tokio::sync::{Mutex, Semaphore};
use tracing::{debug, warn};

use crate::chunker::ChunkPlan;
use crate::tx_builder::{build_backfill_balance_ix, build_backfill_noreplay_ix, BackfillCtx};

/// `NoReplayError::AlreadyAccounted = 7` (solana-noreplay) — re-submitted
/// entry, treat as success.
pub const ALREADY_ACCOUNTED_CUSTOM: u32 = 7;
/// `BackfillError::UnauthorizedCaller = 3` — signer mismatch vs the program's
/// compile-time `BACKFILL_AUTHORITY` const; halt for operator inspection.
pub const UNAUTHORIZED_CALLER_CUSTOM: u32 = 3;

#[derive(Debug, Clone)]
pub struct SubmitterConfig {
    pub rpc_url: String,
    pub concurrency: usize,
    pub max_retries: u32,
    pub initial_retry_delay_ms: u64,
    pub max_retry_delay_ms: u64,
}

impl Default for SubmitterConfig {
    fn default() -> Self {
        Self {
            rpc_url: "http://127.0.0.1:8899".into(),
            concurrency: 16,
            max_retries: 5,
            initial_retry_delay_ms: 500,
            max_retry_delay_ms: 30_000,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct SubmissionStats {
    pub submitted: u64,
    pub already_accounted: u64,
    pub deferred_skipped: u64,
    pub retries: u64,
}

#[derive(Debug)]
pub enum SubmitOutcome {
    Confirmed(String),
    AlreadyAccounted,
}

/// How to handle an RPC error. Public because integration tests in the same
/// crate could exercise the classifier directly.
#[derive(Debug, PartialEq, Eq)]
pub enum ErrorDecision {
    Retry,
    AlreadyAccounted,
    Halt(&'static str),
}

/// Parse the custom-program-error hex code out of an RPC error string, if any.
///
/// The Solana RPC error chain encodes program errors as
/// `"custom program error: 0xN"` deep inside the wrapped message. Walking
/// the string is awkward but lets the orchestrator stay agnostic to the
/// exact ClientError variant layout (which has shifted across solana-client
/// versions).
pub fn extract_custom_program_error(msg: &str) -> Option<u32> {
    let needle = "custom program error: 0x";
    let start = msg.find(needle)?;
    let hex = &msg[start + needle.len()..];
    let end = hex.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(hex.len());
    u32::from_str_radix(&hex[..end], 16).ok()
}

/// Decide how to react to a `ClientError` from `send_and_confirm_transaction`.
pub fn classify(err: &ClientError) -> ErrorDecision {
    let msg = err.to_string();
    if let Some(code) = extract_custom_program_error(&msg) {
        return match code {
            ALREADY_ACCOUNTED_CUSTOM => ErrorDecision::AlreadyAccounted,
            UNAUTHORIZED_CALLER_CUSTOM => ErrorDecision::Halt("UnauthorizedCaller"),
            _ => ErrorDecision::Halt("custom program error (logic)"),
        };
    }
    match err.kind() {
        // Network / RPC transient
        ClientErrorKind::Io(_)
        | ClientErrorKind::Reqwest(_)
        | ClientErrorKind::Middleware(_)
        | ClientErrorKind::RpcError(_) => ErrorDecision::Retry,
        // SigningError, SerializeError, TransactionError → halt; these are
        // bugs on our side.
        _ => ErrorDecision::Halt("unrecoverable client error"),
    }
}

#[derive(Clone)]
struct SharedState {
    ctx: BackfillCtx,
    payer: Arc<Keypair>,
    rpc: Arc<RpcClient>,
    config: SubmitterConfig,
}

impl SharedState {
    async fn submit_chunk(&self, chunk: ChunkPlan) -> Result<SubmitOutcome> {
        let ix = match chunk {
            ChunkPlan::BackfillNoReplay(transfers) => {
                build_backfill_noreplay_ix(&self.ctx, &transfers)
            }
            ChunkPlan::BackfillBalance(accounts) => {
                build_backfill_balance_ix(&self.ctx, &accounts)
            }
            ChunkPlan::DeferredModification(_) | ChunkPlan::DeferredRegistration(_) => {
                return Err(anyhow!(
                    "DeferredModification/Registration not handled by backfill submitter — Phase 7"
                ));
            }
        };
        self.submit_ix_with_retries(ix).await
    }

    async fn submit_ix_with_retries(&self, ix: Instruction) -> Result<SubmitOutcome> {
        let mut attempt = 0u32;
        let mut delay_ms = self.config.initial_retry_delay_ms;
        loop {
            attempt += 1;
            let blockhash = self
                .rpc
                .get_latest_blockhash()
                .await
                .context("get blockhash")?;
            let tx = Transaction::new_signed_with_payer(
                std::slice::from_ref(&ix),
                Some(&self.payer.pubkey()),
                &[&*self.payer],
                blockhash,
            );
            match self.rpc.send_and_confirm_transaction(&tx).await {
                Ok(sig) => return Ok(SubmitOutcome::Confirmed(sig.to_string())),
                Err(e) => match classify(&e) {
                    ErrorDecision::AlreadyAccounted => return Ok(SubmitOutcome::AlreadyAccounted),
                    ErrorDecision::Halt(what) => return Err(anyhow!("halt: {what}: {e}")),
                    ErrorDecision::Retry => {
                        if attempt >= self.config.max_retries {
                            return Err(anyhow!("tx failed after {attempt} attempts: {e}"));
                        }
                        warn!(
                            attempt,
                            delay_ms,
                            error = %e,
                            "tx error, retrying"
                        );
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        delay_ms = (delay_ms * 2).min(self.config.max_retry_delay_ms);
                    }
                },
            }
        }
    }
}

/// Drive a stream of chunks through bounded-concurrency submission.
/// Halts the run on first unrecoverable error; returns aggregated stats
/// (which include the partial progress) wrapped as `Err`'s context where
/// the early halt occurred.
pub async fn submit_chunks<I>(
    ctx: BackfillCtx,
    payer: Keypair,
    config: SubmitterConfig,
    chunks: I,
) -> Result<SubmissionStats>
where
    I: IntoIterator<Item = ChunkPlan>,
{
    let rpc = RpcClient::new_with_commitment(config.rpc_url.clone(), CommitmentConfig::confirmed());
    let shared = SharedState {
        ctx,
        payer: Arc::new(payer),
        rpc: Arc::new(rpc),
        config: config.clone(),
    };
    let sem = Arc::new(Semaphore::new(config.concurrency));
    let stats = Arc::new(Mutex::new(SubmissionStats::default()));

    let mut handles = Vec::new();
    for chunk in chunks {
        if matches!(
            chunk,
            ChunkPlan::DeferredModification(_) | ChunkPlan::DeferredRegistration(_)
        ) {
            stats.lock().await.deferred_skipped += 1;
            continue;
        }

        let permit = sem.clone().acquire_owned().await.expect("semaphore");
        let shared = shared.clone();
        let stats = stats.clone();

        let h = tokio::spawn(async move {
            let _permit = permit;
            let outcome = shared.submit_chunk(chunk).await?;
            let mut s = stats.lock().await;
            match outcome {
                SubmitOutcome::Confirmed(sig) => {
                    s.submitted += 1;
                    debug!(sig = %sig, "tx confirmed");
                }
                SubmitOutcome::AlreadyAccounted => s.already_accounted += 1,
            }
            anyhow::Ok(())
        });
        handles.push(h);
    }

    // Await all; any join-error or task-error halts the run.
    for h in handles {
        h.await
            .context("task join")?
            .context("submission error")?;
    }

    let final_stats = stats.lock().await.clone();
    Ok(final_stats)
}
