//! `svm-accountant-parity` CLI. `check` runs one point-in-time parity check between a
//! snapshot and the deployed program; `report` reads a ledger back.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{bail, ensure, Context, Result};
use clap::{Args, Parser, Subcommand};
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;

use global_accountant_definitions::NOREPLAY_PROGRAM_ID;

use svm_accountant_parity::ledger::{
    append_check, latest_check, read_events, report_at, CheckRecord, StateTotals,
    MAX_FINDING_LINES, SCHEMA_VERSION,
};
use svm_accountant_parity::time::{now_unix, parse_at, unix_to_rfc3339};
use svm_accountant_tools_common::accountant::Accountant;
use svm_accountant_tools_common::config::{LoadedConfig, DEFAULT_CONFIG_PATH};
use svm_accountant_tools_common::hash::{file_sha256, hex32};
use svm_accountant_tools_common::parity::compare::Finding;
use svm_accountant_tools_common::parity::expected_state::from_snapshot_non_empty;
use svm_accountant_tools_common::parity::findings::{
    compared_states, sample, CheckTarget, Mode, MAX_SETTLE_SECONDS,
};
use svm_accountant_tools_common::parity::state::StateKind;
use svm_accountant_tools_common::rpc;
use svm_accountant_tools_common::snapshot::read_rows;

/// Settings come from the config file (`--config`, default `svm-accountant.toml`).
/// Program addresses come from the build.
#[derive(Parser, Debug)]
#[command(name = "svm-accountant-parity", version, about)]
struct Cli {
    /// Config file with the RPC endpoint and each accountant's files.
    #[arg(long, global = true, default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// One point-in-time parity check: snapshot vs deployed program.
    /// Exit 0 when clean, 1 on any finding.
    Check(CheckArgs),
    /// Read the accountant's ledger and print the check state at an instant.
    Report {
        /// Accountant whose ledger to read.
        #[arg(value_enum)]
        accountant: Accountant,
        /// Unix seconds or `YYYY-MM-DDTHH:MM:SSZ`. Defaults to the latest check.
        #[arg(long)]
        at: Option<String>,
    },
}

#[derive(Args, Debug)]
struct CheckArgs {
    /// Accountant to check. Selects the config table, the program and the compared states.
    #[arg(value_enum)]
    accountant: Accountant,
    #[command(flatten)]
    mode: ModeArgs,
    /// Write the check and its findings as JSON.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args, Debug)]
#[group(required = true, multiple = false)]
struct ModeArgs {
    /// Single sample; any finding fails the check, and any `PendingObservations`
    /// account is a finding. For quiesced state only.
    #[arg(long)]
    strict: bool,
    /// Sample, wait this many seconds, and report the second sample. Lets
    /// in-flight Solana writes land before the comparison. Valid before
    /// the cutover only: after it, live traffic moves Solana past the
    /// fixed snapshot.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=MAX_SETTLE_SECONDS))]
    settle: Option<u64>,
}

impl ModeArgs {
    fn mode(&self) -> Mode {
        self.settle.map_or(Mode::Strict, Mode::Settle)
    }
}

/// Exit 0: clean. Exit 1: findings. Exit 2: the check did not complete.
#[tokio::main]
async fn main() {
    match run().await {
        Ok(clean) => std::process::exit(if clean { 0 } else { 1 }),
        Err(e) => {
            eprintln!("Error: {e:?}");
            std::process::exit(2);
        }
    }
}

/// `Ok(true)` when the reported check is clean.
async fn run() -> Result<bool> {
    let cli = Cli::parse();
    let config = LoadedConfig::load(&cli.config)?;
    match cli.command {
        Command::Check(args) => cmd_check(&config, &args).await,
        Command::Report { accountant, at } => cmd_report(&config, accountant, at.as_deref()),
    }
}

/// The deployed program a check reads: the build's addresses for `accountant`.
fn check_target(accountant: Accountant) -> CheckTarget {
    CheckTarget {
        accountant,
        program_id: accountant.program_id(),
        noreplay_program: Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
    }
}

async fn cmd_check(config: &LoadedConfig, args: &CheckArgs) -> Result<bool> {
    let ledger = config.accountant(args.accountant)?.ledger.as_deref();
    let target = check_target(args.accountant);
    let ledger_head = match ledger {
        Some(path) if path.exists() => latest_check(&read_events(path)?)?,
        _ => None,
    };
    if let Some(head) = &ledger_head {
        ensure!(
            head.accountant == args.accountant,
            "ledger holds accountant {}; use one ledger per accountant and program",
            head.accountant
        );
        ensure!(
            head.program_id == target.program_id.to_string(),
            "ledger holds program {}; use one ledger per accountant and program",
            head.program_id
        );
    }
    let (check, findings) =
        run_check(config, args, &target, ledger_head.map(|h| h.check_id)).await?;

    print_summary(&check, &findings);
    if let Some(path) = ledger {
        append_check(path, &check, &findings).context("append ledger")?;
    }
    if let Some(path) = &args.out {
        let shown = &findings[..findings.len().min(MAX_FINDING_LINES)];
        let report = serde_json::json!({ "check": check, "findings": shown });
        std::fs::write(path, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("write {}", path.display()))?;
    }
    Ok(check.clean)
}

fn cmd_report(config: &LoadedConfig, accountant: Accountant, at: Option<&str>) -> Result<bool> {
    let Some(ledger) = config.accountant(accountant)?.ledger.as_deref() else {
        bail!(
            "config {} has no ledger for [{accountant}]",
            config.path.display()
        );
    };
    // No `--at`: the latest check, whatever its id.
    let at_unix = at.map(parse_at).transpose()?.unwrap_or(u64::MAX);
    let events = read_events(ledger)?;
    if let Some(head) = latest_check(&events)? {
        ensure!(
            head.accountant == accountant,
            "ledger {} holds accountant {}, not {accountant}",
            ledger.display(),
            head.accountant
        );
    }
    let Some((check, findings)) = report_at(&events, at_unix) else {
        bail!(
            "ledger {} holds no check at or before {}",
            ledger.display(),
            unix_to_rfc3339(at_unix),
        );
    };
    require_equal_states(&check)?;
    ensure!(
        check.clean == (check.finding_count == 0),
        "ledger check {} is inconsistent: clean={} finding_count={}",
        check.check_id,
        check.clean,
        check.finding_count,
    );
    ensure!(
        findings.len() == check.finding_count.min(MAX_FINDING_LINES),
        "ledger check {} is inconsistent: finding_count={} finding lines={}",
        check.check_id,
        check.finding_count,
        findings.len()
    );
    print_summary(&check, &findings);
    Ok(check.clean)
}

async fn run_check(
    config: &LoadedConfig,
    args: &CheckArgs,
    target: &CheckTarget,
    previous_check_id: Option<u64>,
) -> Result<(CheckRecord, Vec<Finding>)> {
    let mode = args.mode.mode();
    let snapshot = &config.accountant(args.accountant)?.snapshot;
    let rpc_url = &config.config.rpc_url;
    let snapshot_hash =
        file_sha256(snapshot).with_context(|| format!("hash {}", snapshot.display()))?;
    let rows = read_rows(snapshot)?;
    let (expected, skipped) = from_snapshot_non_empty(&rows, args.accountant)
        .with_context(|| format!("expected state of {}", snapshot.display()))?;
    drop(rows);
    eprintln!(
        "[parity] snapshot: {} balances, {} replay marks, {} governance rows (not backfilled)",
        expected.len_of(StateKind::BALANCES),
        expected.len_of(StateKind::ReplayMarks),
        skipped.governance_rows,
    );

    let client = rpc::client(
        rpc_url,
        config.config.origin.as_deref(),
        CommitmentConfig::finalized(),
    )?;
    let result = sample(&client, target, &expected, mode).await?;
    let pending = result.solana_only.pending_observations;
    if mode != Mode::Strict && pending != 0 {
        eprintln!("[parity] warning: {pending} PendingObservations account(s)");
    }
    ensure!(
        file_sha256(snapshot)? == snapshot_hash,
        "snapshot {} changed during the check",
        snapshot.display()
    );

    let states: BTreeMap<StateKind, StateTotals> = compared_states(args.accountant)
        .into_iter()
        .map(|kind| {
            let summary = StateTotals {
                wormchain_count: expected.len_of(kind),
                wormchain_hash: hex32(&expected.hash_of(kind)),
                solana_count: result.actual.len_of(kind),
                solana_hash: hex32(&result.actual.hash_of(kind)),
            };
            (kind, summary)
        })
        .collect();
    let now = now_unix();
    let findings = result.findings;
    let check = CheckRecord {
        schema_version: SCHEMA_VERSION,
        // Unique and increasing per ledger: `report` groups finding lines by check id.
        check_id: previous_check_id.map_or(now, |previous| now.max(previous + 1)),
        ts: unix_to_rfc3339(now),
        mode: mode.to_string(),
        accountant: args.accountant,
        snapshot: snapshot.display().to_string(),
        snapshot_sha256: hex32(&snapshot_hash),
        config: config.path.display().to_string(),
        config_sha256: hex32(&config.sha256),
        rpc_url: rpc_url.clone(),
        program_id: target.program_id.to_string(),
        noreplay_program_id: target.noreplay_program.to_string(),
        states,
        governance_digests: skipped.governance_rows,
        pending,
        finding_count: findings.len(),
        clean: findings.is_empty(),
    };
    require_equal_states(&check)?;
    Ok((check, findings))
}

/// SECURITY: a clean check must show equal count and hash on every compared state.
fn require_equal_states(check: &CheckRecord) -> Result<()> {
    if !check.clean {
        return Ok(());
    }
    for (kind, s) in &check.states {
        ensure!(
            s.wormchain_hash == s.solana_hash,
            "check {} is clean but the {kind} hash differs: {s:?}",
            check.check_id
        );
        ensure!(
            s.wormchain_count == s.solana_count,
            "check {} is clean but the {kind} count differs: {s:?}",
            check.check_id
        );
    }
    Ok(())
}

fn print_summary(check: &CheckRecord, findings: &[Finding]) {
    println!(
        "check {} ({}) mode={} accountant={}",
        check.check_id, check.ts, check.mode, check.accountant
    );
    println!(
        "  program {} (noreplay {}) via {}",
        check.program_id, check.noreplay_program_id, check.rpc_url
    );
    println!(
        "  snapshot {} sha256 {}",
        check.snapshot, check.snapshot_sha256
    );
    println!("  config {} sha256 {}", check.config, check.config_sha256);
    for (kind, s) in &check.states {
        println!(
            "  {:<18} wormchain {:>8} ({}) vs solana {:>8} ({})",
            kind.name(),
            s.wormchain_count,
            s.wormchain_hash,
            s.solana_count,
            s.solana_hash,
        );
    }
    println!("  pending observations: {}", check.pending);
    if check.clean {
        println!("  PARITY: clean");
        return;
    }
    println!("  PARITY: {} finding(s)", check.finding_count);
    for finding in findings.iter().take(MAX_FINDING_LINES) {
        println!(
            "    [{}] {} wormchain={} solana={}",
            finding.kind,
            finding.key,
            finding.expected.as_deref().unwrap_or("absent"),
            finding.actual.as_deref().unwrap_or("absent"),
        );
    }
}
