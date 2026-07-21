//! Concurrent RPC submission with retry and AlreadyAccounted-as-success.
//!
//! ## Policy
//!
//! - **AlreadyAccounted (custom 7)** → success, if the program ever
//!   actually returns it. In practice no instruction this program exposes
//!   today can produce code 7 (`BackfillNoReplay`'s CPI target OR-merges
//!   its bitmap unconditionally, with no "already set" rejection) — this
//!   branch is kept as defense-in-depth for a future variant that does
//!   define it. Resume-time dedup relies on `preflight.rs`'s on-chain
//!   verification instead, not on this code path.
//! - **UnauthorizedCaller (custom 3)** → halt. The configured signer does
//!   not match the program's compile-time `BACKFILL_AUTHORITY` — operator
//!   misconfig, no point retrying.
//! - **Other custom program errors** → halt. Logic errors are unrecoverable
//!   in the orchestrator's frame — the operator needs to inspect.
//! - **RPC / I/O / blockhash-expired / transient errors** → retry with
//!   exponential backoff, capped attempts, incrementing
//!   `SubmissionStats::retries` for every retry actually taken.
//!
//! Each tx fetches its own blockhash before signing. Concurrent submission
//! through a `tokio::sync::Semaphore`; the empirical 16-way concurrency from
//! the at-scale probe is the default.
//!
//! ## Halt semantics
//!
//! On first unrecoverable error, dispatch of new chunks stops, but every
//! chunk already in flight is a real, already-broadcast transaction, so it
//! is awaited to completion and counted rather than abandoned. A shared
//! `Arc<AtomicBool>` halt flag is checked before acquiring a concurrency
//! permit and again right after (the flag may flip while blocked waiting
//! on a permit), so no new work is spawned once it's set. The join phase
//! then awaits every handle unconditionally rather than short-circuiting
//! on the first error.

use std::sync::atomic::{AtomicBool, Ordering};
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
use tokio::sync::{mpsc::UnboundedSender, Mutex, Semaphore};
use tracing::{debug, warn};

use crate::chunker::ChunkPlan;
use crate::tx_builder::{
    build_backfill_balance_ix, build_backfill_noreplay_ix, build_backfill_relayer_registration_ix,
    build_backfill_transceiver_hub_ix, build_backfill_transceiver_peer_ix, BackfillCtx,
};

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

#[derive(Debug, Clone)]
pub enum SubmitOutcome {
    Confirmed(String),
    AlreadyAccounted,
}

/// Per-chunk terminal outcome, reported via the progress channel passed to
/// [`submit_chunks_with_progress`] in true completion order (not
/// necessarily dispatch order under concurrency). Mirrors [`SubmitOutcome`]
/// plus the deferred-skip case, so cursor bookkeeping can track every
/// chunk's fate, including the ones this submitter never actually sends.
#[derive(Debug, Clone)]
pub enum ChunkOutcome {
    Confirmed(String),
    AlreadyAccounted,
    DeferredSkipped,
}

/// One chunk's terminal outcome, tagged with its position in the ORIGINAL
/// input sequence passed to [`submit_chunks_with_progress`]. Chunks can
/// complete out of order under concurrency, so `index` — not arrival order
/// on the channel — is the only reliable way to know which chunk this is.
#[derive(Debug, Clone)]
pub struct ChunkProgress {
    pub index: usize,
    pub outcome: ChunkOutcome,
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
    let end = hex
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(hex.len());
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
    /// Shared so retries can be counted as they happen, regardless of the
    /// chunk's eventual outcome.
    stats: Arc<Mutex<SubmissionStats>>,
}

impl SharedState {
    async fn submit_chunk(&self, chunk: ChunkPlan) -> Result<SubmitOutcome> {
        let ix = match chunk {
            ChunkPlan::BackfillNoReplay(transfers) => {
                build_backfill_noreplay_ix(&self.ctx, &transfers)
            }
            ChunkPlan::BackfillBalance(accounts) => build_backfill_balance_ix(&self.ctx, &accounts),
            ChunkPlan::BackfillRelayerRegistration(entries) => {
                build_backfill_relayer_registration_ix(&self.ctx, &entries)
            }
            ChunkPlan::BackfillTransceiverHub(entries) => {
                build_backfill_transceiver_hub_ix(&self.ctx, &entries)
            }
            ChunkPlan::BackfillTransceiverPeer(entries) => {
                build_backfill_transceiver_peer_ix(&self.ctx, &entries)
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
                        // Count the retry immediately, not deferred to the
                        // chunk's eventual outcome.
                        self.stats.lock().await.retries += 1;
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

/// Drive a stream of chunks through bounded-concurrency submission. See the
/// module doc's "Halt semantics" section for the precise halt behavior.
/// Equivalent to [`submit_chunks_with_progress`] with no progress channel —
/// use that variant when per-chunk completion needs to be observed, e.g.
/// to persist cursor progress as chunks land.
pub async fn submit_chunks<I>(
    ctx: BackfillCtx,
    payer: Keypair,
    config: SubmitterConfig,
    chunks: I,
) -> Result<SubmissionStats>
where
    I: IntoIterator<Item = ChunkPlan>,
{
    submit_chunks_with_progress(ctx, payer, config, chunks, None).await
}

/// As [`submit_chunks`], but additionally reports each chunk's terminal
/// outcome — tagged with its position in `chunks` — on `progress` the
/// moment it completes. Chunks can complete out of order under
/// concurrency; callers that need an ordered "confirmed up to index N"
/// notion (e.g. a resumable cursor) must track a completion set keyed by
/// `ChunkProgress::index` and compute the longest confirmed prefix
/// themselves — see `main.rs`'s `run` orchestrator.
pub async fn submit_chunks_with_progress<I>(
    ctx: BackfillCtx,
    payer: Keypair,
    config: SubmitterConfig,
    chunks: I,
    progress: Option<UnboundedSender<ChunkProgress>>,
) -> Result<SubmissionStats>
where
    I: IntoIterator<Item = ChunkPlan>,
{
    let rpc = RpcClient::new_with_commitment(config.rpc_url.clone(), CommitmentConfig::confirmed());
    let stats = Arc::new(Mutex::new(SubmissionStats::default()));
    let shared = SharedState {
        ctx,
        payer: Arc::new(payer),
        rpc: Arc::new(rpc),
        config: config.clone(),
        stats: stats.clone(),
    };
    let sem = Arc::new(Semaphore::new(config.concurrency));
    // Set as soon as any dispatched chunk hits an unrecoverable error; see
    // the module doc's "Halt semantics" section.
    let halt = Arc::new(AtomicBool::new(false));

    let mut handles = Vec::new();
    for (index, chunk) in chunks.into_iter().enumerate() {
        if matches!(
            chunk,
            ChunkPlan::DeferredModification(_) | ChunkPlan::DeferredRegistration(_)
        ) {
            stats.lock().await.deferred_skipped += 1;
            if let Some(tx) = &progress {
                let _ = tx.send(ChunkProgress {
                    index,
                    outcome: ChunkOutcome::DeferredSkipped,
                });
            }
            continue;
        }

        // Early-out before even trying to acquire a permit.
        if halt.load(Ordering::SeqCst) {
            break;
        }
        let permit = sem.clone().acquire_owned().await.expect("semaphore");
        // Re-check: the flag may have flipped while this iteration waited
        // on the semaphore.
        if halt.load(Ordering::SeqCst) {
            drop(permit);
            break;
        }

        let shared = shared.clone();
        let stats = stats.clone();
        let halt = halt.clone();
        let progress = progress.clone();

        let h = tokio::spawn(async move {
            let _permit = permit;
            match shared.submit_chunk(chunk).await {
                Ok(outcome) => {
                    let mut s = stats.lock().await;
                    let reported = match &outcome {
                        SubmitOutcome::Confirmed(sig) => {
                            s.submitted += 1;
                            debug!(sig = %sig, "tx confirmed");
                            ChunkOutcome::Confirmed(sig.clone())
                        }
                        SubmitOutcome::AlreadyAccounted => {
                            s.already_accounted += 1;
                            ChunkOutcome::AlreadyAccounted
                        }
                    };
                    drop(s);
                    if let Some(tx) = &progress {
                        let _ = tx.send(ChunkProgress {
                            index,
                            outcome: reported,
                        });
                    }
                    anyhow::Ok(())
                }
                Err(e) => {
                    // Unrecoverable: stop dispatching further work; this
                    // chunk has no terminal state to report on `progress`.
                    halt.store(true, Ordering::SeqCst);
                    Err(e)
                }
            }
        });
        handles.push(h);
    }

    // Await every handle unconditionally rather than short-circuiting on
    // the first error, so in-flight work is always awaited to completion.
    let mut first_err: Option<anyhow::Error> = None;
    for h in handles {
        match h.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
            Err(join_err) => {
                if first_err.is_none() {
                    first_err = Some(anyhow!("task join error: {join_err}"));
                }
            }
        }
    }

    let final_stats = stats.lock().await.clone();
    if let Some(e) = first_err {
        return Err(e.context(format!(
            "halted after unrecoverable error; partial stats: {final_stats:?}"
        )));
    }
    Ok(final_stats)
}
