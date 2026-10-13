//! `svm-accountant-backfill` CLI: argument parsing and dispatch. The run logic
//! lives in [`svm_accountant_backfill::run`]. Settings come from the config file
//! (`--config`, default `svm-accountant.toml`); program addresses come from the build.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use solana_keypair::read_keypair_file;
use tracing_subscriber::EnvFilter;

use svm_accountant_backfill::run::{run, RunConfig};
use svm_accountant_backfill::snapshot_balance_check::check_snapshot_balances;
use svm_accountant_backfill::submitter::SubmitterConfig;
use svm_accountant_tools_common::accountant::Accountant;
use svm_accountant_tools_common::config::{LoadedConfig, DEFAULT_CONFIG_PATH};
use svm_accountant_tools_common::snapshot::SnapshotReader;

#[derive(Parser, Debug)]
#[command(name = "svm-accountant-backfill", version, about)]
struct Cli {
    /// Config file with the RPC endpoint, submission settings and each accountant's files.
    #[arg(long, global = true, default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Walk the accountant's snapshot and report row counts, batching
    /// projection, and cost estimate. Reads the snapshot file only.
    Stats {
        #[arg(value_enum)]
        accountant: Accountant,
        /// SOL/USD price for the cost display (informational only).
        #[arg(long, default_value_t = 230.0)]
        sol_usd: f64,
    },
    /// Backfill the accountant's snapshot into its deployed program: batch
    /// it, verify-before-skip whatever a resumed progress file claims is
    /// already done, submit the rest, and (by default) run the parity check.
    Run {
        #[arg(value_enum)]
        accountant: Accountant,
        /// Skip the post-run parity check.
        #[arg(long)]
        no_parity_check: bool,
    },
    /// Check that the accountant's snapshot transfer history nets to its committed balances.
    CheckSnapshotBalances {
        #[arg(value_enum)]
        accountant: Accountant,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let config = LoadedConfig::load(&cli.config)?;
    match cli.command {
        Command::Stats {
            accountant,
            sol_usd,
        } => svm_accountant_backfill::stats::run(&config.accountant(accountant)?.snapshot, sol_usd),
        Command::Run {
            accountant,
            no_parity_check,
        } => {
            let run_config = run_config(&config, accountant, !no_parity_check)?;
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .context("build tokio runtime")?
                .block_on(run(run_config))
        }
        Command::CheckSnapshotBalances { accountant } => {
            cmd_check_snapshot_balances(&config.accountant(accountant)?.snapshot, accountant)
        }
    }
}

fn run_config(
    config: &LoadedConfig,
    accountant: Accountant,
    parity_check: bool,
) -> Result<RunConfig> {
    let section = config.accountant(accountant)?;
    let Some(payer_path) = &section.payer else {
        bail!(
            "config {} has no payer for [{accountant}]",
            config.path.display()
        );
    };
    let payer = read_keypair_file(payer_path)
        .map_err(|e| anyhow!("read payer keypair {}: {e}", payer_path.display()))?;
    let backfill = &config.config.backfill;
    Ok(RunConfig {
        snapshot: section.snapshot.clone(),
        accountant,
        program_id: accountant.program_id(),
        payer,
        rpc_url: config.config.rpc_url.clone(),
        origin: config.config.origin.clone(),
        progress_file: section.progress_file_path(),
        submitter: SubmitterConfig {
            concurrency: backfill.concurrency,
            max_retries: backfill.max_retries,
            initial_retry_delay_ms: backfill.initial_retry_delay_ms,
            max_retry_delay_ms: backfill.max_retry_delay_ms,
        },
        parity_check,
    })
}

fn cmd_check_snapshot_balances(snapshot: &Path, accountant: Accountant) -> Result<()> {
    let tag = format!("[{accountant}] ");
    let reader = SnapshotReader::open(snapshot)
        .with_context(|| format!("open snapshot {}", snapshot.display()))?;
    let report = check_snapshot_balances(reader)?;

    println!(
        "{tag}netted {} transfers + {} modifications against {} committed balances",
        report.transfers, report.modifications, report.balances
    );
    for line in &report.discrepancies {
        println!("{tag}{line}");
    }
    if !report.is_clean() {
        bail!(
            "{tag}snapshot balance check FAILED: {} matched, {} discrepancies",
            report.matched,
            report.discrepancies.len()
        );
    }
    println!(
        "{tag}OK — all {} committed balances equal their net transfer history",
        report.matched
    );
    Ok(())
}
