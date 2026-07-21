//! `ga-backfill` CLI shim.
//!
//! ## `run` orchestrator
//!
//! Parses the catalogue and materializes the full chunk plan up front
//! (bounded by available memory; acceptable for a run-once tool). On
//! resume, every chunk the cursor claims is confirmed is re-verified
//! against on-chain state (`preflight::chunk_confirmed_on_chain`) before
//! being trusted, and resubmitted one at a time if verification fails.
//! The remaining chunks are then driven through
//! `submitter::submit_chunks_with_progress` at the configured concurrency;
//! since chunks can confirm out of order, the cursor advances by the
//! longest contiguous confirmed prefix rather than by arrival order.
//! Reconciliation runs by default at the end (`--no-reconcile` to skip)
//! and a non-clean result is a hard failure.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};

use ga_backfill::catalogue::CatalogueReader;
use ga_backfill::chunker::{ChunkPlan, Chunker};
use ga_backfill::cursor::Cursor;
use ga_backfill::preflight::chunk_confirmed_on_chain;
use ga_backfill::reconcile;
use ga_backfill::submitter::{
    submit_chunks, submit_chunks_with_progress, ChunkOutcome, SubmitterConfig,
};
use ga_backfill::stats::TX_FEE_LAMPORTS;
use ga_backfill::tx_builder::BackfillCtx;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_keypair::read_keypair_file;
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
    /// Walk the catalogue and report record counts, chunking projection, and
    /// cost estimate. Sends no transactions — pure read.
    IndexStats {
        /// Path to `catalogue.jsonl` produced by the wormchain-snapshot tool.
        #[arg(long)]
        catalogue: PathBuf,
        /// SOL/USD price for the cost display (informational only).
        #[arg(long, default_value_t = 230.0)]
        sol_usd: f64,
    },
    /// Drive an actual backfill migration run against a deployed program:
    /// load the catalogue, chunk it, verify-before-skip whatever a resumed
    /// cursor claims is already done, submit the rest, and (by default)
    /// reconcile at the end. See this module's doc for details.
    Run {
        /// Path to `catalogue.jsonl`. A single run targets ONE deployed
        /// program (WTT or NTT) — point this at the matching catalogue.
        #[arg(long)]
        catalogue: PathBuf,
        /// Base58 pubkey of the deployed backfill program.
        #[arg(long)]
        program_id: String,
        /// Path to the payer/authority keypair JSON file. Must match the
        /// program's compile-time `BACKFILL_AUTHORITY` —
        /// `BackfillCtx::new` panics otherwise.
        #[arg(long)]
        payer: PathBuf,
        /// JSON-RPC endpoint.
        #[arg(long, default_value = "http://127.0.0.1:8899")]
        rpc_url: String,
        /// Cursor file path. Defaults to `cursor.json` next to the catalogue.
        #[arg(long)]
        cursor: Option<PathBuf>,
        /// Concurrent in-flight transactions during bulk submission.
        #[arg(long, default_value_t = 16)]
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
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::IndexStats { catalogue, sol_usd } => ga_backfill::stats::run(&catalogue, sol_usd),
        Commands::Run {
            catalogue,
            program_id,
            payer,
            rpc_url,
            cursor,
            concurrency,
            max_retries,
            initial_retry_delay_ms,
            max_retry_delay_ms,
            no_reconcile,
        } => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .context("build tokio runtime")?;
            rt.block_on(cmd_run(
                catalogue,
                program_id,
                payer,
                rpc_url,
                cursor,
                concurrency,
                max_retries,
                initial_retry_delay_ms,
                max_retry_delay_ms,
                no_reconcile,
            ))
        }
    }
}

/// SHA-256 (hex, `0x`-prefixed) of the catalogue file content, for the
/// cursor's resume identity check. Streamed rather than reading the whole
/// (potentially multi-GB) file into memory at once.
fn catalogue_content_hash(path: &std::path::Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file =
        std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).context("read catalogue for hashing")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("0x{}", hex::encode(hasher.finalize())))
}

#[allow(clippy::too_many_arguments)]
async fn cmd_run(
    catalogue: PathBuf,
    program_id: String,
    payer_path: PathBuf,
    rpc_url: String,
    cursor_path: Option<PathBuf>,
    concurrency: usize,
    max_retries: u32,
    initial_retry_delay_ms: u64,
    max_retry_delay_ms: u64,
    no_reconcile: bool,
) -> Result<()> {
    let program_id = Pubkey::from_str(&program_id).context("parse --program-id")?;
    let payer = read_keypair_file(&payer_path)
        .map_err(|e| anyhow!("read payer keypair {}: {e}", payer_path.display()))?;
    // BackfillCtx::new panics if `payer` doesn't match BACKFILL_AUTHORITY,
    // to fail fast rather than burn fees on doomed transactions.
    let ctx = BackfillCtx::new(program_id, payer.pubkey());

    let cursor_path = cursor_path.unwrap_or_else(|| catalogue.with_file_name("cursor.json"));
    let catalogue_hash = catalogue_content_hash(&catalogue)?;
    // Stride 1: flush every confirmed chunk immediately, rather than the
    // module's coarser default — cheap next to the tx round-trip it
    // follows, and it narrows the window a crash can leave unflushed.
    let mut cursor = Cursor::load_or_init_with_stride(
        cursor_path.clone(),
        &catalogue,
        &catalogue_hash,
        &program_id.to_string(),
        1,
    )
    .with_context(|| format!("load/init cursor at {}", cursor_path.display()))?;

    println!("[run] catalogue={}", catalogue.display());
    println!("[run] program_id={program_id}");
    println!("[run] cursor={}", cursor_path.display());
    println!(
        "[run] resume: cursor claims {} chunk(s) already confirmed",
        cursor.skip_count()
    );

    // Plan the full chunk list up front (bounded by memory; fine for a
    // run-once tool).
    let records: Vec<_> = CatalogueReader::open(&catalogue)
        .with_context(|| format!("open catalogue {}", catalogue.display()))?
        .collect::<Result<Vec<_>, _>>()
        .context("parse catalogue — a real migration must not silently skip malformed rows")?;
    let chunks: Vec<ChunkPlan> = Chunker::new(records.into_iter()).collect();
    println!("[run] planned {} chunk(s) total", chunks.len());

    let rpc = RpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());

    // Verify-before-skip: re-check every chunk the cursor claims is
    // confirmed against on-chain state before trusting it (see preflight.rs).
    let claimed_done = (cursor.skip_count() as usize).min(chunks.len());
    let mut anomalous: Vec<usize> = Vec::new();
    for (i, chunk) in chunks.iter().enumerate().take(claimed_done) {
        if !chunk_confirmed_on_chain(&rpc, &ctx, chunk).await? {
            eprintln!(
                "[run] WARNING: cursor claims chunk {i} already confirmed, but on-chain \
                 state does not match — resubmitting it instead of trusting the cursor"
            );
            anomalous.push(i);
        }
    }

    if !anomalous.is_empty() {
        println!(
            "[run] resubmitting {} anomalous chunk(s) found by verify-before-skip \
             (one at a time; this is expected to be rare)",
            anomalous.len()
        );
    }
    for &i in &anomalous {
        let config = SubmitterConfig {
            rpc_url: rpc_url.clone(),
            concurrency: 1,
            max_retries,
            initial_retry_delay_ms,
            max_retry_delay_ms,
        };
        // This pass repairs on-chain state only; cursor position `i` is
        // already correct from the prior run.
        submit_chunks(
            ctx.clone(),
            payer.insecure_clone(),
            config,
            vec![chunks[i].clone()],
        )
        .await
        .with_context(|| format!("resubmitting anomalous chunk {i}"))?;
    }

    // ----- Bulk submission of everything not yet confirmed. -----
    let remaining: Vec<ChunkPlan> = chunks[claimed_done..].to_vec();
    println!(
        "[run] {} chunk(s) verified already done; {} to submit this run",
        claimed_done, remaining.len()
    );

    if !remaining.is_empty() {
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
        let config = SubmitterConfig {
            rpc_url: rpc_url.clone(),
            concurrency,
            max_retries,
            initial_retry_delay_ms,
            max_retry_delay_ms,
        };
        let submit_ctx = ctx.clone();
        let submit_payer = payer.insecure_clone();
        let submit_handle = tokio::spawn(async move {
            submit_chunks_with_progress(submit_ctx, submit_payer, config, remaining, Some(progress_tx))
                .await
        });

        // Chunks can confirm out of order under concurrency; buffer by
        // original index and advance the cursor only by the longest
        // contiguous confirmed prefix.
        let mut pending: BTreeMap<usize, ChunkOutcome> = BTreeMap::new();
        let mut next_index = 0usize;
        while let Some(progress) = progress_rx.recv().await {
            pending.insert(progress.index, progress.outcome);
            while let Some(outcome) = pending.remove(&next_index) {
                match outcome {
                    ChunkOutcome::Confirmed(sig) => {
                        cursor.observe_confirmed(Some(&sig), TX_FEE_LAMPORTS)?
                    }
                    ChunkOutcome::AlreadyAccounted => cursor.observe_already_accounted()?,
                    ChunkOutcome::DeferredSkipped => cursor.observe_deferred_skipped()?,
                }
                next_index += 1;
            }
        }

        let stats = submit_handle
            .await
            .context("submit task join")?
            .context("submission halted on an unrecoverable error")?;
        println!("[run] submission stats: {stats:?}");
    }

    cursor.flush().context("final cursor flush")?;
    println!(
        "[run] cursor now at {} / {} chunk(s)",
        cursor.skip_count(),
        chunks.len()
    );

    if !no_reconcile {
        run_reconcile(&rpc, &program_id, &catalogue, &chunks).await?;
    } else {
        println!("[run] --no-reconcile set; skipping post-run reconciliation");
    }

    Ok(())
}

/// Reconciles whichever account classes appeared in the plan (WTT Balance
/// and/or the three NTT maps) against the catalogue. A non-clean result is
/// a hard failure.
async fn run_reconcile(
    rpc: &RpcClient,
    program_id: &Pubkey,
    catalogue: &std::path::Path,
    chunks: &[ChunkPlan],
) -> Result<()> {
    let has_balance = chunks.iter().any(|c| matches!(c, ChunkPlan::BackfillBalance(_)));
    let has_relayer = chunks
        .iter()
        .any(|c| matches!(c, ChunkPlan::BackfillRelayerRegistration(_)));
    let has_hub = chunks
        .iter()
        .any(|c| matches!(c, ChunkPlan::BackfillTransceiverHub(_)));
    let has_peer = chunks
        .iter()
        .any(|c| matches!(c, ChunkPlan::BackfillTransceiverPeer(_)));

    let mut clean = true;

    if has_balance {
        let report = reconcile::reconcile_balances(rpc, program_id, catalogue)
            .await
            .context("reconcile balances")?;
        println!(
            "[reconcile:balance] matched={} mismatched={} missing_from_chain={} unexpected_on_chain={}",
            report.matched,
            report.mismatched.len(),
            report.missing_from_chain.len(),
            report.unexpected_on_chain.len()
        );
        if !report.is_clean() {
            clean = false;
            for m in &report.mismatched {
                eprintln!("[reconcile:balance] MISMATCH {m:?}");
            }
            for k in &report.missing_from_chain {
                eprintln!("[reconcile:balance] MISSING_FROM_CHAIN {k:?}");
            }
            for k in &report.unexpected_on_chain {
                eprintln!("[reconcile:balance] UNEXPECTED_ON_CHAIN {k:?}");
            }
        }
    }

    if has_relayer {
        let expected = reconcile::load_expected_relayer_registrations(catalogue)
            .context("load expected relayer registrations")?;
        let actual = reconcile::fetch_on_chain_relayer_registrations(rpc, program_id)
            .await
            .context("fetch on-chain relayer registrations")?;
        let verdict = reconcile::compare_maps(&expected, &actual);
        println!("[reconcile:relayer_registration] {verdict:?}");
        clean &= verdict.is_clean();
    }

    if has_hub {
        let expected = reconcile::load_expected_transceiver_hubs(catalogue)
            .context("load expected transceiver hubs")?;
        let actual = reconcile::fetch_on_chain_transceiver_hubs(rpc, program_id)
            .await
            .context("fetch on-chain transceiver hubs")?;
        let verdict = reconcile::compare_maps(&expected, &actual);
        println!("[reconcile:transceiver_hub] {verdict:?}");
        clean &= verdict.is_clean();
    }

    if has_peer {
        let expected = reconcile::load_expected_transceiver_peers(catalogue)
            .context("load expected transceiver peers")?;
        let actual = reconcile::fetch_on_chain_transceiver_peers(rpc, program_id)
            .await
            .context("fetch on-chain transceiver peers")?;
        let verdict = reconcile::compare_maps(&expected, &actual);
        println!("[reconcile:transceiver_peer] {verdict:?}");
        clean &= verdict.is_clean();
    }

    if !clean {
        bail!("reconciliation found discrepancies — see output above; do not consider this migration complete");
    }
    println!("[reconcile] OK — all applicable account classes reconcile cleanly");
    Ok(())
}
