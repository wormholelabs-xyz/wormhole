//! CLI shim for [`ga_backfill::snapshot_balance_check`] — see that module for the
//! balance check and sign rule.
//!
//!   check_snapshot_balances --snapshot <path> [--label NTT|WTT]

use anyhow::{bail, Context, Result};
use clap::Parser;

use ga_backfill::snapshot::SnapshotReader;
use ga_backfill::snapshot_balance_check::check_snapshot_balances;

#[derive(Parser)]
#[command(about = "Reconcile committed accountant balances against the transfer history.")]
struct Cli {
    /// Path to the decoded snapshot (JSONL). For NTT this is the
    /// decoded `ntt-raw-dump.jsonl`; for WTT the wormchain-snapshot.
    #[arg(long)]
    snapshot: std::path::PathBuf,
    /// Label for output only (e.g. NTT, WTT).
    #[arg(long, default_value = "")]
    label: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let tag = if cli.label.is_empty() {
        String::new()
    } else {
        format!("[{}] ", cli.label)
    };

    let reader = SnapshotReader::open(&cli.snapshot)
        .with_context(|| format!("open snapshot {}", cli.snapshot.display()))?;
    let report = check_snapshot_balances(reader)?;

    println!(
        "{tag}replayed {} transfers + {} modifications against {} committed balances",
        report.transfers, report.modifications, report.accounts
    );
    for line in &report.mismatches {
        println!("{tag}{line}");
    }
    if report.is_clean() {
        println!(
            "{tag}OK — all {} committed balances reconcile from the transfer history",
            report.matched
        );
        Ok(())
    } else {
        bail!(
            "{tag}reconciliation FAILED: {} matched, {} discrepancies",
            report.matched,
            report.mismatches.len()
        )
    }
}
