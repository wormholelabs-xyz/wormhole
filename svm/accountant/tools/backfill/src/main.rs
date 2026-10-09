//! `ga-backfill` CLI shim.
//!
//! ## `run` orchestrator
//!
//! Parses the snapshot, rejects any row kind the accountant does not hold,
//! materializes the full batch plan, and builds and parses every
//! instruction once before the first send. On resume, every batch the progress file
//! claims is confirmed is re-verified against on-chain state
//! (`onchain_check::batch_is_written`) before being trusted, and
//! resubmitted one at a time if verification fails. The remaining batches are
//! then driven through `submitter::submit_batches_with_progress` at the
//! configured concurrency; since batches can confirm out of order, the progress file
//! advances by the longest contiguous confirmed prefix rather than by arrival
//! order. Reconciliation runs by default at the end (`--no-reconcile` to skip)
//! and a non-clean result is a hard failure.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, ensure, Context, Result};
use clap::{Args, Parser, Subcommand};

use ga_backfill::accountant::Accountant;
use ga_backfill::batcher::{check_batch_order, Batcher, TxBatch};
use ga_backfill::expected_accounts::{
    chain_registration_account, modify_balance_account, register_chain_account,
    transceiver_hub_account, transceiver_peer_account,
};
use ga_backfill::onchain_check::batch_is_written;
use ga_backfill::progress::{ProgressFile, RunIdentity};
use ga_backfill::reconcile::{self, MapVerdict};
use ga_backfill::snapshot::{file_sha256, SnapshotReader, SnapshotRow};
use ga_backfill::stats::TX_FEE_LAMPORTS;
use ga_backfill::submitter::{
    submit_batches, submit_batches_with_progress, BatchOutcome, SubmitterConfig,
};
use ga_backfill::tx_builder::{batches_hash, BackfillTarget};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_keypair::{read_keypair_file, Keypair};
use solana_pubkey::Pubkey;
use solana_signer::Signer;

#[derive(Parser, Debug)]
#[command(name = "ga-backfill", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Walk the snapshot and report row counts, batching projection, and
    /// cost estimate. Sends no transactions — pure read.
    Stats {
        /// Snapshot file (`catalogue.jsonl`) produced by the wormchain-snapshot tool.
        #[arg(long)]
        snapshot: PathBuf,
        /// SOL/USD price for the cost display (informational only).
        #[arg(long, default_value_t = 230.0)]
        sol_usd: f64,
    },
    /// Drive an actual backfill migration run against a deployed program:
    /// load the snapshot, batch it, verify-before-skip whatever a resumed
    /// progress file claims is already done, submit the rest, and (by default)
    /// reconcile at the end. See this module's doc for details.
    Run(RunArgs),
}

#[derive(Args, Debug)]
struct RunArgs {
    /// Snapshot file (`catalogue.jsonl`). A single run targets ONE deployed
    /// program (WTT or NTT) — point this at the matching snapshot.
    #[arg(long)]
    snapshot: PathBuf,
    /// Accountant the snapshot belongs to. Rows of the other kind are fatal.
    #[arg(long, value_enum)]
    accountant: Accountant,
    /// Base58 pubkey of the deployed backfill program. Must equal the
    /// accountant's compiled program id.
    #[arg(long)]
    program_id: Pubkey,
    /// Path to the payer/authority keypair JSON file. Must match the
    /// accountant's compile-time backfill authority.
    #[arg(long)]
    payer: PathBuf,
    /// JSON-RPC endpoint. Use a direct endpoint: the internal RPC proxy
    /// rejects requests without an `Origin` header.
    #[arg(long, default_value = "http://127.0.0.1:8899")]
    rpc_url: String,
    /// Progress file path. Defaults to `progress.json` next to the snapshot.
    #[arg(long)]
    progress_file: Option<PathBuf>,
    /// Concurrent in-flight transactions during bulk submission.
    #[arg(long, default_value_t = 16, value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..=256))]
    concurrency: usize,
    #[arg(long, default_value_t = 5)]
    max_retries: u32,
    #[arg(long, default_value_t = 500)]
    initial_retry_delay_ms: u64,
    #[arg(long, default_value_t = 30_000)]
    max_retry_delay_ms: u64,
    /// Skip the post-run reconciliation pass.
    #[arg(long)]
    no_reconcile: bool,
}

impl RunArgs {
    fn submitter_config(&self) -> SubmitterConfig {
        SubmitterConfig {
            rpc_url: self.rpc_url.clone(),
            concurrency: self.concurrency,
            max_retries: self.max_retries,
            initial_retry_delay_ms: self.initial_retry_delay_ms,
            max_retry_delay_ms: self.max_retry_delay_ms,
        }
    }
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Commands::Stats { snapshot, sol_usd } => ga_backfill::stats::run(&snapshot, sol_usd),
        Commands::Run(args) => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("build tokio runtime")?
            .block_on(cmd_run(args)),
    }
}

async fn cmd_run(args: RunArgs) -> Result<()> {
    let payer = read_keypair_file(&args.payer)
        .map_err(|e| anyhow!("read payer keypair {}: {e}", args.payer.display()))?;
    // Fails on a program id or payer the `.so` rejects, before any fee is paid.
    let target = BackfillTarget::try_new(args.accountant, args.program_id, payer.pubkey())?;
    let progress_path = args
        .progress_file
        .clone()
        .unwrap_or_else(|| args.snapshot.with_file_name("progress.json"));

    println!("[run] snapshot={}", args.snapshot.display());
    println!("[run] accountant={}", args.accountant);
    println!("[run] program_id={}", args.program_id);
    println!("[run] progress_file={}", progress_path.display());

    let snapshot_sha256 =
        file_sha256(&args.snapshot).with_context(|| format!("hash {}", args.snapshot.display()))?;
    let mut batches = plan(&args.snapshot, args.accountant)?;
    ensure!(
        file_sha256(&args.snapshot)? == snapshot_sha256,
        "snapshot {} changed while it was read",
        args.snapshot.display()
    );
    println!("[run] planned {} batch(s) total", batches.len());

    // SECURITY: key order across batches, then every instruction built and
    // parsed back once, so a malformed plan aborts before the first send.
    check_batch_order(&batches).context("batch plan")?;
    let batches_hash = format!("0x{}", hex::encode(batches_hash(&target, &batches)?));
    println!(
        "[run] all {} instruction(s) parse with the program's parsers; plan {batches_hash}",
        batches.len()
    );

    // Stride 1: flush every confirmed batch immediately.
    let identity = RunIdentity {
        snapshot_sha256: format!("0x{}", hex::encode(snapshot_sha256)),
        batches_hash,
        program_id: args.program_id.to_string(),
    };
    let mut progress_file = ProgressFile::open(progress_path.clone(), &args.snapshot, &identity, 1)
        .with_context(|| format!("load/init progress_file at {}", progress_path.display()))?;
    let claimed_done = progress_file.batches_done() as usize;
    println!("[run] resume: progress_file claims {claimed_done} batch(s) already confirmed");
    if claimed_done > batches.len() {
        bail!(
            "progress_file claims {claimed_done} batch(s) but the plan has {}",
            batches.len()
        );
    }

    let rpc = RpcClient::new_with_commitment(args.rpc_url.clone(), CommitmentConfig::confirmed());
    repair_claimed(&rpc, &target, &payer, &args, &batches[..claimed_done]).await?;

    let total = batches.len();
    let remaining = batches.split_off(claimed_done);
    println!(
        "[run] {claimed_done} batch(s) verified already done; {} to submit this run",
        remaining.len()
    );
    if !remaining.is_empty() {
        submit_remaining(&target, &payer, &args, remaining, &mut progress_file).await?;
    }

    progress_file.flush().context("final progress_file flush")?;
    println!(
        "[run] progress_file now at {} / {total} batch(s)",
        progress_file.batches_done()
    );

    if args.no_reconcile {
        println!("[run] --no-reconcile set; skipping post-run reconciliation");
        return Ok(());
    }
    run_reconcile(&rpc, &target, &args.snapshot).await
}

/// Snapshot rows a run holds in memory. Mainnet: about 5.53M.
const MAX_SNAPSHOT_ROWS: usize = 16_000_000;

/// Parse the whole snapshot, require every row to be `accountant` state, and batch it.
fn plan(snapshot: &Path, accountant: Accountant) -> Result<Vec<TxBatch>> {
    let mut rows: Vec<SnapshotRow> = Vec::new();
    for row in SnapshotReader::open(snapshot)
        .with_context(|| format!("open snapshot {}", snapshot.display()))?
    {
        ensure!(
            rows.len() < MAX_SNAPSHOT_ROWS,
            "snapshot holds more than MAX_SNAPSHOT_ROWS ({MAX_SNAPSHOT_ROWS}) rows"
        );
        rows.push(
            row.context("parse snapshot — a real migration must not silently skip malformed rows")?,
        );
    }
    if let Some((line, row)) = rows
        .iter()
        .enumerate()
        .find(|(_, r)| !accountant.accepts(r))
    {
        bail!(
            "snapshot row {} is not {accountant} state: {row:?}",
            line + 1
        );
    }
    Ok(Batcher::new(rows.into_iter()).collect())
}

/// Verify every batch the progress file claims against chain state, and resubmit
/// any that is not fully written, one at a time. The progress file position stays.
async fn repair_claimed(
    rpc: &RpcClient,
    target: &BackfillTarget,
    payer: &Keypair,
    args: &RunArgs,
    claimed: &[TxBatch],
) -> Result<()> {
    let mut anomalous = Vec::new();
    for (i, batch) in claimed.iter().enumerate() {
        if !batch_is_written(rpc, target, batch).await? {
            eprintln!(
                "[run] WARNING: progress_file claims batch {i} already confirmed, but on-chain \
                 state does not match — resubmitting it instead of trusting the progress file"
            );
            anomalous.push(i);
        }
    }
    if anomalous.is_empty() {
        return Ok(());
    }
    println!(
        "[run] resubmitting {} anomalous batch(s) found by verify-before-skip \
         (one at a time; this is expected to be rare)",
        anomalous.len()
    );
    let config = SubmitterConfig {
        concurrency: 1,
        ..args.submitter_config()
    };
    for i in anomalous {
        submit_batches(
            target.clone(),
            payer.insecure_clone(),
            config.clone(),
            vec![claimed[i].clone()],
        )
        .await
        .with_context(|| format!("resubmitting anomalous batch {i}"))?;
    }
    Ok(())
}

/// Submit `remaining` concurrently. Batches can confirm out of order, so the
/// progress file advances only by the longest contiguous confirmed prefix.
async fn submit_remaining(
    target: &BackfillTarget,
    payer: &Keypair,
    args: &RunArgs,
    remaining: Vec<TxBatch>,
    progress_file: &mut ProgressFile,
) -> Result<()> {
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
    let submit = tokio::spawn(submit_batches_with_progress(
        target.clone(),
        payer.insecure_clone(),
        args.submitter_config(),
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

/// Reconciles every account class the accountant writes, and the NoReplay
/// marks, against the snapshot. A non-clean result is a hard failure.
async fn run_reconcile(rpc: &RpcClient, target: &BackfillTarget, snapshot: &Path) -> Result<()> {
    let program_id = &target.program_id;
    let mut clean = true;
    let mut row = |class: &str, verdict: &MapVerdict| {
        println!("[reconcile:{class}] {verdict:?}");
        clean &= verdict.is_clean();
    };

    let report = reconcile::reconcile_balances(rpc, program_id, snapshot)
        .await
        .context("reconcile balances")?;
    println!(
        "[reconcile:balance] matched={} mismatched={} missing_from_chain={} unexpected_on_chain={}",
        report.matched,
        report.mismatched.len(),
        report.missing_from_chain.len(),
        report.unexpected_on_chain.len()
    );
    for m in &report.mismatched {
        eprintln!("[reconcile:balance] MISMATCH {m:?}");
    }
    for k in &report.missing_from_chain {
        eprintln!("[reconcile:balance] MISSING_FROM_CHAIN {k:?}");
    }
    for k in &report.unexpected_on_chain {
        eprintln!("[reconcile:balance] UNEXPECTED_ON_CHAIN {k:?}");
    }
    let balances_clean = report.is_clean();

    // `Accountant::accepts` already admitted only this accountant's registration kind.
    let verdict = reconcile::reconcile_layouts(rpc, program_id, snapshot, |r| match r {
        SnapshotRow::ModifyBalance(m) => vec![modify_balance_account(m)],
        _ => vec![],
    })
    .await
    .context("reconcile modify_balance records")?;
    row("modify_balance", &verdict);
    let verdict = reconcile::reconcile_layouts(rpc, program_id, snapshot, |r| {
        r.chain_registration()
            .map(chain_registration_account)
            .into_iter()
            .collect()
    })
    .await
    .context("reconcile chain registrations")?;
    row("chain_registration", &verdict);
    let verdict = reconcile::reconcile_layouts(rpc, program_id, snapshot, |r| {
        r.chain_registration()
            .map(register_chain_account)
            .into_iter()
            .collect()
    })
    .await
    .context("reconcile register_chain records")?;
    row("register_chain", &verdict);

    if target.accountant == Accountant::Ntt {
        let verdict = reconcile::reconcile_layouts(rpc, program_id, snapshot, |r| match r {
            SnapshotRow::TransceiverHub(h) => vec![transceiver_hub_account(h)],
            _ => vec![],
        })
        .await
        .context("reconcile transceiver hubs")?;
        row("transceiver_hub", &verdict);
        let verdict = reconcile::reconcile_layouts(rpc, program_id, snapshot, |r| match r {
            SnapshotRow::TransceiverPeer(p) => vec![transceiver_peer_account(p)],
            _ => vec![],
        })
        .await
        .context("reconcile transceiver peers")?;
        row("transceiver_peer", &verdict);
    }

    let noreplay = reconcile::reconcile_noreplay(rpc, target, snapshot)
        .await
        .context("reconcile noreplay marks")?;
    println!("[reconcile:noreplay] {noreplay:?}");

    if !(clean && balances_clean && noreplay.is_clean()) {
        bail!("reconciliation found discrepancies — see output above; do not consider this migration complete");
    }
    println!("[reconcile] OK — all account classes and NoReplay marks reconcile cleanly");
    Ok(())
}
