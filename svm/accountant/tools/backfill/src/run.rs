//! `svm-accountant-backfill run`: one backfill run against a deployed program.
//!
//! 1. Parse the snapshot once, reject any row kind the accountant does not
//!    hold, build the expected parity state, and batch the rows.
//! 2. Build and parse back every instruction once before the first send.
//! 3. On resume, verify every batch the progress file claims on chain
//!    ([`onchain_check::unwritten_batches`]) and resubmit the unwritten ones.
//! 4. Submit the remaining batches concurrently. Batches can confirm out of
//!    order, so the progress file advances by the longest contiguous confirmed prefix.
//! 5. Run the parity check (`svm_accountant_tools_common::parity`) against the
//!    expected state, at `finalized`. Any finding fails the run.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use svm_accountant_tools_common::accountant::Accountant;
use svm_accountant_tools_common::hash::{file_sha256, hex32};
use svm_accountant_tools_common::parity::compare::Finding;
use svm_accountant_tools_common::parity::expected_state::from_snapshot_non_empty;
use svm_accountant_tools_common::parity::findings::{sample, CheckTarget, Mode};
use svm_accountant_tools_common::parity::state::ChainState;
use svm_accountant_tools_common::rpc;
use svm_accountant_tools_common::snapshot::{read_rows, SnapshotRow};

use crate::batcher::{check_batch_order, Batcher, TxBatch};
use crate::onchain_check::unwritten_batches;
use crate::progress::{ProgressFile, RunIdentity};
use crate::submitter::{
    submit_batches, submit_batches_with_progress, BatchOutcome, SubmitterConfig,
};
use crate::target::BackfillTarget;
use crate::tx_builder::plan_hash;
use crate::wire::TX_FEE_LAMPORTS;

/// Everything one run needs.
pub struct RunConfig {
    pub snapshot: PathBuf,
    pub accountant: Accountant,
    pub program_id: Pubkey,
    pub payer: Keypair,
    pub rpc_url: String,
    /// `Origin` header for every RPC request.
    pub origin: Option<String>,
    pub progress_file: PathBuf,
    pub submitter: SubmitterConfig,
    pub parity_check: bool,
}

/// Require every row to be `accountant` state, and batch the rows.
pub fn plan_batches(rows: Vec<SnapshotRow>, accountant: Accountant) -> Result<Vec<TxBatch>> {
    if let Some((index, row)) = rows
        .iter()
        .enumerate()
        .find(|(_, r)| !accountant.accepts(r))
    {
        bail!(
            "snapshot row {} is not {accountant} state: {row:?}",
            index + 1
        );
    }
    let row_count = rows.len();
    let batches: Vec<TxBatch> = Batcher::new(rows.into_iter()).collect();
    ensure!(
        row_count == 0 || !batches.is_empty(),
        "{row_count} rows produced no batch"
    );
    Ok(batches)
}

pub async fn run(config: RunConfig) -> Result<()> {
    // Fails on a program id or payer the `.so` rejects, before any fee is paid.
    let target =
        BackfillTarget::try_new(config.accountant, config.program_id, config.payer.pubkey())?;
    let progress_path = config.progress_file.clone();

    println!("[run] snapshot={}", config.snapshot.display());
    println!("[run] accountant={}", config.accountant);
    println!("[run] program_id={}", config.program_id);
    println!("[run] progress file={}", progress_path.display());

    let snapshot_sha256 = file_sha256(&config.snapshot)
        .with_context(|| format!("hash {}", config.snapshot.display()))?;
    let rows = read_rows(&config.snapshot)?;
    ensure!(
        file_sha256(&config.snapshot)? == snapshot_sha256,
        "snapshot {} changed while it was read",
        config.snapshot.display()
    );
    // Built before the rows move into the batcher; the expected state is compact.
    let expected = if config.parity_check {
        Some(from_snapshot_non_empty(&rows, config.accountant)?.0)
    } else {
        None
    };
    let mut batches = plan_batches(rows, config.accountant)?;
    println!("[run] planned {} batch(s) total", batches.len());

    // SECURITY: key order across batches, then every instruction built and
    // parsed back once, so a malformed plan aborts before the first send.
    check_batch_order(&batches).context("batch plan")?;
    let plan_hash = hex32(&plan_hash(&target, &batches)?);
    println!(
        "[run] all {} instruction(s) parse with the program's parsers; plan {plan_hash}",
        batches.len()
    );

    // Stride 1: flush every confirmed batch immediately.
    let identity = RunIdentity {
        snapshot_sha256: hex32(&snapshot_sha256),
        batches_hash: plan_hash,
        program_id: config.program_id.to_string(),
    };
    let mut progress_file =
        ProgressFile::open(progress_path.clone(), &config.snapshot, &identity, 1)
            .with_context(|| format!("open progress file {}", progress_path.display()))?;
    let claimed = progress_file.batches_done() as usize;
    println!("[run] resume: progress file claims {claimed} batch(s) already confirmed");
    if claimed > batches.len() {
        bail!(
            "progress file claims {claimed} batch(s) but the plan has {}",
            batches.len()
        );
    }

    let rpc = Arc::new(rpc::client(
        &config.rpc_url,
        config.origin.as_deref(),
        CommitmentConfig::confirmed(),
    )?);
    reverify_claimed_batches(&rpc, &target, &config, &batches[..claimed]).await?;

    let total = batches.len();
    let remaining = batches.split_off(claimed);
    println!(
        "[run] {claimed} batch(s) verified already done; {} to submit this run",
        remaining.len()
    );
    if !remaining.is_empty() {
        submit_remaining_batches(&rpc, &target, &config, remaining, &mut progress_file).await?;
    }

    progress_file.flush().context("final progress file flush")?;
    println!(
        "[run] progress file now at {} / {total} batch(s)",
        progress_file.batches_done()
    );

    let Some(expected) = expected else {
        println!("[run] --no-parity-check set; skipping the post-run parity check");
        return Ok(());
    };
    let findings = parity_findings(
        &rpc,
        &config.rpc_url,
        config.origin.as_deref(),
        &target,
        &expected,
    )
    .await?;
    if findings.is_empty() {
        println!("[parity] clean: every compared state and NoReplay mark matches the snapshot");
        return Ok(());
    }
    for finding in &findings {
        eprintln!(
            "[parity] [{}] {} expected={} actual={}",
            finding.kind,
            finding.key,
            finding.expected.as_deref().unwrap_or("absent"),
            finding.actual.as_deref().unwrap_or("absent"),
        );
    }
    // Same rule as the cutover gate, `svm-accountant-parity check --strict`.
    bail!(
        "parity check found {} finding(s); do not consider this migration complete",
        findings.len()
    )
}

/// Verify every batch the progress file claims against chain state, and resubmit
/// any that is not fully written, one at a time. The progress file position stays.
pub async fn reverify_claimed_batches(
    rpc: &Arc<RpcClient>,
    target: &BackfillTarget,
    config: &RunConfig,
    claimed: &[TxBatch],
) -> Result<()> {
    let unwritten = unwritten_batches(rpc, target, claimed, config.submitter.concurrency).await?;
    if unwritten.is_empty() {
        return Ok(());
    }
    for index in &unwritten {
        eprintln!(
            "[run] WARNING: progress file claims batch {index} already confirmed, but on-chain \
             state does not match — resubmitting it instead of trusting the progress file"
        );
    }
    println!(
        "[run] resubmitting {} unwritten batch(s) found by verify-before-skip \
         (one at a time; this is expected to be rare)",
        unwritten.len()
    );
    let one_at_a_time = SubmitterConfig {
        concurrency: 1,
        ..config.submitter.clone()
    };
    for index in unwritten {
        submit_batches(
            Arc::clone(rpc),
            target.clone(),
            config.payer.insecure_clone(),
            one_at_a_time.clone(),
            vec![claimed[index].clone()],
        )
        .await
        .with_context(|| format!("resubmitting unwritten batch {index}"))?;
    }
    Ok(())
}

/// Submit `remaining` concurrently. Batches can confirm out of order, so the
/// progress file advances only by the longest contiguous confirmed prefix.
pub async fn submit_remaining_batches(
    rpc: &Arc<RpcClient>,
    target: &BackfillTarget,
    config: &RunConfig,
    remaining: Vec<TxBatch>,
    progress_file: &mut ProgressFile,
) -> Result<()> {
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
    let submit = tokio::spawn(submit_batches_with_progress(
        Arc::clone(rpc),
        target.clone(),
        config.payer.insecure_clone(),
        config.submitter.clone(),
        remaining,
        Some(progress_tx),
    ));

    let mut pending: BTreeMap<usize, BatchOutcome> = BTreeMap::new();
    let mut next_index = 0usize;
    while let Some(progress) = progress_rx.recv().await {
        pending.insert(progress.index, progress.outcome);
        while let Some(outcome) = pending.remove(&next_index) {
            match outcome {
                BatchOutcome::Confirmed(sig) => {
                    progress_file.observe_confirmed(Some(&sig), TX_FEE_LAMPORTS)?
                }
                BatchOutcome::AlreadyWritten => progress_file.observe_already_written()?,
            }
            next_index += 1;
        }
    }

    let stats = submit
        .await
        .context("submit task join")?
        .context("submission halted on an unrecoverable error")?;
    println!("[run] submission stats: {stats:?}");
    Ok(())
}

/// Longest wait for the finalized slot to reach the last confirmed write.
pub const MAX_FINALIZATION_WAIT: Duration = Duration::from_secs(120);

/// The parity check of `svm-accountant-parity check` against `expected`, at `finalized`,
/// after the finalized slot reaches the slot `confirmed` reports now.
pub async fn parity_findings(
    confirmed: &RpcClient,
    rpc_url: &str,
    origin: Option<&str>,
    target: &BackfillTarget,
    expected: &ChainState,
) -> Result<Vec<Finding>> {
    let finalized = rpc::client(rpc_url, origin, CommitmentConfig::finalized())?;
    wait_for_finalization(confirmed, &finalized).await?;
    let check_target = CheckTarget {
        accountant: target.accountant(),
        program_id: target.program_id(),
        noreplay_program: target.noreplay_program(),
    };
    let mode = post_run_parity_mode();
    let result = sample(&finalized, &check_target, expected, mode).await?;
    println!(
        "[parity] mode={mode} pending observations={}",
        result.solana_only.pending_observations
    );
    result.solana_only.log_foreign_accounts();
    Ok(result.findings)
}

/// Poll until the finalized slot reaches the confirmed slot at call time.
async fn wait_for_finalization(confirmed: &RpcClient, finalized: &RpcClient) -> Result<()> {
    let target_slot = confirmed.get_slot().await.context("confirmed slot")?;
    let deadline = Instant::now() + MAX_FINALIZATION_WAIT;
    // Bounded by MAX_FINALIZATION_WAIT.
    loop {
        let slot = finalized.get_slot().await.context("finalized slot")?;
        if slot >= target_slot {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "finalized slot {slot} below confirmed slot {target_slot} after {MAX_FINALIZATION_WAIT:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Parity mode of the post-run check.
fn post_run_parity_mode() -> Mode {
    Mode::Strict
}
