//! Concurrent RPC submission with retry; already-written rejections are verified on chain.
//!
//! ## Policy
//!
//! - **AlreadyAccounted (custom 7) / InvalidPda (custom 2)** → re-verify.
//!   A replayed batch fails at its first already-written entry: NoReplay with
//!   7, every PDA-creating kind with 2. Either code proves one entry only, so
//!   the batch counts as done only if `onchain_check::batch_is_written`
//!   finds every entry written; otherwise halt.
//! - **UnauthorizedCaller (custom 33)** → halt. The configured signer does
//!   not match the program's compile-time backfill authority — operator
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
//! On first unrecoverable error, dispatch of new batches stops, but every
//! batch already in flight is a real, already-broadcast transaction, so it
//! is awaited to completion and counted rather than abandoned. A shared
//! `Arc<AtomicBool>` halt flag is checked before acquiring a concurrency
//! permit and again right after (the flag may flip while blocked waiting
//! on a permit), so no new work is spawned once it's set. Finished tasks are
//! reaped from a `JoinSet` during dispatch, and the join phase awaits every
//! remaining task; the first error is reported.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
use tokio::sync::{mpsc::UnboundedSender, Semaphore};
use tokio::task::{JoinError, JoinSet};
use tracing::{debug, warn};

use global_accountant_definitions::GlobalAccountantError;

use crate::batcher::TxBatch;
use crate::onchain_check::batch_is_written;
use crate::tx_builder::{build_ix, BackfillTarget};

/// `GlobalAccountantError::AlreadyAccounted`: a replayed `BackfillNoReplay` entry.
pub const ALREADY_ACCOUNTED_CUSTOM: u32 = GlobalAccountantError::AlreadyAccounted as u32;
/// `GlobalAccountantError::InvalidPda`: among others, a create on an account that holds data.
pub const INVALID_PDA_CUSTOM: u32 = GlobalAccountantError::InvalidPda as u32;
/// `GlobalAccountantError::UnauthorizedCaller` — signer mismatch vs the
/// program's compile-time backfill authority; halt for operator inspection.
pub const UNAUTHORIZED_CALLER_CUSTOM: u32 = GlobalAccountantError::UnauthorizedCaller as u32;

#[derive(Debug, Clone)]
pub struct SubmitterConfig {
    pub rpc_url: String,
    pub concurrency: usize,
    pub max_retries: u32,
    pub initial_retry_delay_ms: u64,
    pub max_retry_delay_ms: u64,
}

#[derive(Debug, Default, Clone)]
pub struct SubmissionStats {
    pub submitted: u64,
    pub already_written: u64,
    pub retries: u64,
}

/// Per-batch terminal outcome, reported via the progress channel passed to
/// [`submit_batches_with_progress`] in true completion order (not
/// necessarily dispatch order under concurrency).
#[derive(Debug, Clone)]
pub enum BatchOutcome {
    Confirmed(String),
    AlreadyWritten,
}

/// One batch's terminal outcome, tagged with its position in the ORIGINAL
/// input sequence passed to [`submit_batches_with_progress`]. Batches can
/// complete out of order under concurrency, so `index` — not arrival order
/// on the channel — is the only reliable way to know which batch this is.
#[derive(Debug, Clone)]
pub struct BatchProgress {
    pub index: usize,
    pub outcome: BatchOutcome,
}

/// How to handle an RPC error. Public because integration tests in the same
/// crate could exercise the classifier directly.
#[derive(Debug, PartialEq, Eq)]
pub enum ErrorDecision {
    Retry,
    /// The program rejected an entry as already written. Done only after on-chain verification.
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
            ALREADY_ACCOUNTED_CUSTOM | INVALID_PDA_CUSTOM => ErrorDecision::AlreadyAccounted,
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

/// Live counters behind [`SubmissionStats`].
#[derive(Default)]
struct Counters {
    submitted: AtomicU64,
    already_written: AtomicU64,
    retries: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> SubmissionStats {
        SubmissionStats {
            submitted: self.submitted.load(Ordering::SeqCst),
            already_written: self.already_written.load(Ordering::SeqCst),
            retries: self.retries.load(Ordering::SeqCst),
        }
    }
}

/// Exponential backoff from the submitter config: doubles per wait, capped.
struct Backoff {
    delay_ms: u64,
    max_ms: u64,
}

impl Backoff {
    fn new(config: &SubmitterConfig) -> Self {
        Self {
            delay_ms: config.initial_retry_delay_ms,
            max_ms: config.max_retry_delay_ms,
        }
    }

    async fn wait(&mut self) {
        tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
        self.delay_ms = self.delay_ms.saturating_mul(2).min(self.max_ms);
    }
}

struct Submitter {
    target: BackfillTarget,
    payer: Keypair,
    rpc: RpcClient,
    config: SubmitterConfig,
    /// Updated as events happen, regardless of a batch's eventual outcome.
    counters: Counters,
}

impl Submitter {
    async fn submit_batch(&self, batch: TxBatch) -> Result<BatchOutcome> {
        let ix = build_ix(&self.target, &batch)?;
        let outcome = self.submit_ix_with_retries(&batch, ix).await?;
        match &outcome {
            BatchOutcome::Confirmed(sig) => {
                self.counters.submitted.fetch_add(1, Ordering::SeqCst);
                debug!(sig = %sig, "tx confirmed");
            }
            BatchOutcome::AlreadyWritten => {
                self.counters.already_written.fetch_add(1, Ordering::SeqCst);
            }
        }
        Ok(outcome)
    }

    /// `batch_is_written`, retried on `false` and on RPC errors: a
    /// lagging RPC node can miss a just-landed write. At most `max_retries`
    /// attempts, at least one.
    async fn verify_with_retries(&self, batch: &TxBatch) -> Result<bool> {
        let mut backoff = Backoff::new(&self.config);
        for _ in 1..self.config.max_retries {
            if let Ok(true) = batch_is_written(&self.rpc, &self.target, batch).await {
                return Ok(true);
            }
            backoff.wait().await;
        }
        batch_is_written(&self.rpc, &self.target, batch).await
    }

    async fn submit_ix_with_retries(
        &self,
        batch: &TxBatch,
        ix: Instruction,
    ) -> Result<BatchOutcome> {
        let mut backoff = Backoff::new(&self.config);
        let mut attempt = 0u32;
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
                &[&self.payer],
                blockhash,
            );
            let e = match self.rpc.send_and_confirm_transaction(&tx).await {
                Ok(sig) => return Ok(BatchOutcome::Confirmed(sig.to_string())),
                Err(e) => e,
            };
            match classify(&e) {
                ErrorDecision::AlreadyAccounted => {
                    // SECURITY: the error names one entry; every entry must be on chain.
                    if self.verify_with_retries(batch).await? {
                        return Ok(BatchOutcome::AlreadyWritten);
                    }
                    return Err(anyhow!(
                        "halt: {} rejected as already written, but on-chain state does not hold the whole batch: {e}",
                        batch.kind()
                    ));
                }
                ErrorDecision::Halt(what) => return Err(anyhow!("halt: {what}: {e}")),
                ErrorDecision::Retry if attempt >= self.config.max_retries => {
                    return Err(anyhow!("tx failed after {attempt} attempts: {e}"));
                }
                ErrorDecision::Retry => {
                    self.counters.retries.fetch_add(1, Ordering::SeqCst);
                    warn!(attempt, delay_ms = backoff.delay_ms, error = %e, "tx error, retrying");
                    backoff.wait().await;
                }
            }
        }
    }
}

/// Drive a stream of batches through bounded-concurrency submission. See the
/// module doc's "Halt semantics" section for the precise halt behavior.
/// Equivalent to [`submit_batches_with_progress`] with no progress channel —
/// use that variant when per-batch completion needs to be observed, e.g.
/// to persist progress file progress as batches land.
pub async fn submit_batches<I>(
    target: BackfillTarget,
    payer: Keypair,
    config: SubmitterConfig,
    batches: I,
) -> Result<SubmissionStats>
where
    I: IntoIterator<Item = TxBatch>,
{
    submit_batches_with_progress(target, payer, config, batches, None).await
}

/// As [`submit_batches`], but additionally reports each batch's terminal
/// outcome — tagged with its position in `batches` — on `progress` the
/// moment it completes. Batches can complete out of order under
/// concurrency; callers that need an ordered "confirmed up to index N"
/// notion (e.g. a resumable progress file) must track a completion set keyed by
/// `BatchProgress::index` and compute the longest confirmed prefix
/// themselves — see `main.rs`'s `run` orchestrator.
pub async fn submit_batches_with_progress<I>(
    target: BackfillTarget,
    payer: Keypair,
    config: SubmitterConfig,
    batches: I,
    progress: Option<UnboundedSender<BatchProgress>>,
) -> Result<SubmissionStats>
where
    I: IntoIterator<Item = TxBatch>,
{
    let rpc = RpcClient::new_with_commitment(config.rpc_url.clone(), CommitmentConfig::confirmed());
    let concurrency = config.concurrency;
    let shared = Arc::new(Submitter {
        target,
        payer,
        rpc,
        config,
        counters: Counters::default(),
    });
    let sem = Arc::new(Semaphore::new(concurrency));
    // Set as soon as any dispatched batch hits an unrecoverable error; see
    // the module doc's "Halt semantics" section.
    let halt = Arc::new(AtomicBool::new(false));

    let mut tasks: JoinSet<Result<()>> = JoinSet::new();
    let mut first_err: Option<anyhow::Error> = None;
    let mut note = |joined: Result<Result<()>, JoinError>| {
        let err = match joined {
            Ok(Ok(())) => return,
            Ok(Err(e)) => e,
            Err(join_err) => anyhow!("task join error: {join_err}"),
        };
        first_err.get_or_insert(err);
    };
    for (index, batch) in batches.into_iter().enumerate() {
        while let Some(joined) = tasks.try_join_next() {
            note(joined);
        }
        // Early-out before even trying to acquire a permit.
        if halt.load(Ordering::SeqCst) {
            break;
        }
        let permit = sem.clone().acquire_owned().await.expect("semaphore");
        // Re-check: the flag may have flipped while this iteration waited
        // on the semaphore.
        if halt.load(Ordering::SeqCst) {
            break;
        }

        let shared = shared.clone();
        let halt = halt.clone();
        let progress = progress.clone();
        tasks.spawn(async move {
            let _permit = permit;
            match shared.submit_batch(batch).await {
                Ok(outcome) => {
                    if let Some(tx) = &progress {
                        let _ = tx.send(BatchProgress { index, outcome });
                    }
                    Ok(())
                }
                Err(e) => {
                    // Unrecoverable: stop dispatching further work; this
                    // batch has no terminal state to report on `progress`.
                    halt.store(true, Ordering::SeqCst);
                    Err(e)
                }
            }
        });
    }
    // Every in-flight task is a broadcast transaction: await all of them.
    while let Some(joined) = tasks.join_next().await {
        note(joined);
    }

    let final_stats = shared.counters.snapshot();
    match first_err {
        Some(e) => Err(e.context(format!(
            "halted after unrecoverable error; partial stats: {final_stats:?}"
        ))),
        None => Ok(final_stats),
    }
}
