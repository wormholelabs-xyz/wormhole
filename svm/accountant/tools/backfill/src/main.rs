//! `ga-backfill` CLI shim.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::IndexStats { catalogue, sol_usd } => ga_backfill::stats::run(&catalogue, sol_usd),
    }
}
