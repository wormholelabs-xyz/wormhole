//! CLI shim for [`ga_backfill::balance_reconcile`] — see that module for the
//! reconciliation logic and sign rule.
//!
//!   reconcile_balances --catalogue <path> [--label NTT|WTT]

use anyhow::{bail, Context, Result};
use clap::Parser;

use ga_backfill::balance_reconcile::reconcile;
use ga_backfill::catalogue::CatalogueReader;

#[derive(Parser)]
#[command(about = "Reconcile committed accountant balances against the transfer history.")]
struct Cli {
    /// Path to the decoded snapshot catalogue (JSONL). For NTT this is the
    /// decoded `ntt-raw-dump.jsonl`; for WTT the workstream-A catalogue.
    #[arg(long)]
    catalogue: std::path::PathBuf,
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

    let reader = CatalogueReader::open(&cli.catalogue)
        .with_context(|| format!("open catalogue {}", cli.catalogue.display()))?;
    let report = reconcile(reader)?;

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
