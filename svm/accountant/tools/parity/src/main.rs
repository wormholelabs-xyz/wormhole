//! `ga-parity` CLI. `check` runs one point-in-time parity check between a
//! snapshot and the deployed program; `report` replays a ledger.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use clap::{Args, Parser, Subcommand};
use ga_backfill::accountant::Accountant;
use ga_backfill::snapshot::file_sha256;
use solana_pubkey::Pubkey;

use ga_parity::compare::{compare, Finding};
use ga_parity::expected_state::SkippedRows;
use ga_parity::ledger::{
    append, latest_check, replay, report_at, CheckResult, StateTotals, MAX_FINDING_LINES,
    SCHEMA_VERSION,
};
use ga_parity::onchain_state::ScanFindings;
use ga_parity::state::{hex32, ChainState, ComparedState};
use ga_parity::time::{now_unix, parse_at, unix_to_rfc3339};
use ga_parity::{expected_state, onchain_state, rpc};

/// Longest `--settle` interval.
const MAX_SETTLE_SECONDS: u64 = 3_600;

#[derive(Parser, Debug)]
#[command(name = "ga-parity", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// One point-in-time parity check: snapshot vs deployed program.
    /// Exit 0 when clean, 1 on any finding.
    Check(CheckArgs),
    /// Replay a ledger and print the check state at an instant.
    Report {
        #[arg(long)]
        ledger: PathBuf,
        /// Unix seconds or `YYYY-MM-DDTHH:MM:SSZ`. Defaults to the latest check.
        #[arg(long)]
        at: Option<String>,
    },
}

#[derive(Args, Debug)]
struct CheckArgs {
    /// Snapshot file (`catalogue.jsonl`) produced by the wormchain-snapshot tool.
    #[arg(long)]
    snapshot: PathBuf,
    /// Accountant the snapshot belongs to. Selects the compared states.
    #[arg(long, value_enum)]
    accountant: Accountant,
    /// Base58 pubkey of the deployed accountant program.
    #[arg(long)]
    program_id: Pubkey,
    /// Base58 pubkey of the NoReplay program the accountant CPIs into.
    #[arg(long)]
    noreplay_program_id: Pubkey,
    /// JSON-RPC endpoint.
    #[arg(long, default_value = "http://127.0.0.1:8899")]
    rpc_url: String,
    /// `Origin` header for every RPC request (the internal proxy needs one).
    #[arg(long)]
    origin: Option<String>,
    #[command(flatten)]
    mode: ModeArgs,
    /// JSONL ledger to append this check (and its findings) to.
    #[arg(long)]
    ledger: Option<PathBuf>,
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

#[derive(Clone, Copy, Debug)]
enum Mode {
    Strict,
    Settle(u64),
}

impl From<&ModeArgs> for Mode {
    fn from(args: &ModeArgs) -> Self {
        args.settle.map_or(Mode::Strict, Mode::Settle)
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mode::Strict => f.write_str("strict"),
            Mode::Settle(seconds) => write!(f, "settle:{seconds}s"),
        }
    }
}

/// Exit 0: clean. Exit 1: findings. Exit 2: the check did not complete.
fn main() {
    match run() {
        Ok(clean) => std::process::exit(if clean { 0 } else { 1 }),
        Err(e) => {
            eprintln!("Error: {e:?}");
            std::process::exit(2);
        }
    }
}

/// `Ok(true)` when the reported check is clean.
fn run() -> Result<bool> {
    match Cli::parse().command {
        Commands::Check(args) => check(&args),
        Commands::Report { ledger, at } => report(&ledger, at.as_deref()),
    }
}

fn check(args: &CheckArgs) -> Result<bool> {
    let head = match &args.ledger {
        Some(path) if path.exists() => latest_check(&replay(path)?)?,
        _ => None,
    };
    if let Some(h) = &head {
        ensure!(
            h.accountant == args.accountant && h.program_id == args.program_id.to_string(),
            "ledger holds {} {}; use one ledger per accountant and program",
            h.accountant,
            h.program_id
        );
    }
    let (check, findings) = run_check(args, head.map(|h| h.check_id))?;

    print_summary(&check, &findings);
    if let Some(path) = &args.ledger {
        append(path, &check, &findings).context("append ledger")?;
    }
    if let Some(path) = &args.out {
        let shown = &findings[..findings.len().min(MAX_FINDING_LINES)];
        let report = serde_json::json!({ "check": check, "findings": shown });
        std::fs::write(path, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("write {}", path.display()))?;
    }
    Ok(check.clean)
}

fn report(ledger: &Path, at: Option<&str>) -> Result<bool> {
    // No `--at`: the latest check, whatever its id.
    let at_unix = at.map(parse_at).transpose()?.unwrap_or(u64::MAX);
    let events = replay(ledger)?;
    latest_check(&events)?;
    let Some((check, findings)) = report_at(&events, at_unix) else {
        bail!(
            "ledger {} holds no check at or before {}",
            ledger.display(),
            unix_to_rfc3339(at_unix),
        );
    };
    require_equal_states(&check)?;
    ensure!(
        check.clean == (check.finding_count == 0)
            && findings.len() == check.finding_count.min(MAX_FINDING_LINES),
        "ledger check {} is inconsistent: clean={} finding_count={} diff lines={}",
        check.check_id,
        check.clean,
        check.finding_count,
        findings.len()
    );
    print_summary(&check, &findings);
    Ok(check.clean)
}

/// States compared for `accountant`, in report order.
fn compared_states(accountant: Accountant) -> Vec<ComparedState> {
    let mut out = vec![
        ComparedState::Balances,
        ComparedState::ReplayMarks,
        ComparedState::ModifyBalanceRecords,
        ComparedState::ChainRegistrations,
        ComparedState::RegisterChainRecords,
    ];
    if accountant == Accountant::Ntt {
        out.extend([
            ComparedState::TransceiverHubs,
            ComparedState::TransceiverPeers,
        ]);
    }
    out
}

/// All findings of one sample: state differences, malformed accounts, and in
/// `--strict` a non-zero pending count.
fn collect_findings(
    expected: &ChainState,
    actual: &ChainState,
    scan: &ScanFindings,
    strict: bool,
) -> Vec<Finding> {
    let mut out = compare(expected, actual);
    out.extend(scan.malformed.iter().cloned());
    if strict && scan.pending != 0 {
        out.push(Finding {
            kind: ComparedState::PendingObservations,
            key: "count".to_string(),
            wormchain: Some("0".to_string()),
            solana: Some(scan.pending.to_string()),
        });
    }
    out
}

/// Expected state from the snapshot. Empty Balances or Replay is fatal: a
/// truncated or wrong snapshot against an empty program would compare clean.
fn load_expected(args: &CheckArgs) -> Result<(ChainState, SkippedRows)> {
    let (expected, skipped) =
        expected_state::expected_state_from_snapshot(&args.snapshot, args.accountant)?;
    for kind in [ComparedState::Balances, ComparedState::ReplayMarks] {
        ensure!(
            expected.state_len(kind) != 0,
            "snapshot {} holds no {kind} rows",
            args.snapshot.display()
        );
    }
    eprintln!(
        "[parity] snapshot: {} balances, {} replay marks, {} governance rows (not backfilled)",
        expected.state_len(ComparedState::Balances),
        expected.state_len(ComparedState::ReplayMarks),
        skipped.governance_digests,
    );
    Ok((expected, skipped))
}

/// Sample Solana per `mode`; in settle mode the second sample is reported.
/// A finding gone by the second sample was lag; one new in it is current.
fn sample(
    args: &CheckArgs,
    mode: Mode,
    expected: &ChainState,
) -> Result<(ChainState, ScanFindings, Vec<Finding>)> {
    let runtime = tokio::runtime::Runtime::new().context("build tokio runtime")?;
    let client = rpc::client(&args.rpc_url, args.origin.as_deref())?;
    let strict = matches!(mode, Mode::Strict);
    let take = || -> Result<_> {
        let (actual, scan) = runtime.block_on(onchain_state::fetch_onchain_state(
            &client,
            &args.program_id,
            &args.noreplay_program_id,
            args.accountant,
            &expected.replay,
        ))?;
        let findings = collect_findings(expected, &actual, &scan, strict);
        Ok((actual, scan, findings))
    };
    let first = take()?;
    let Mode::Settle(seconds) = mode else {
        return Ok(first);
    };
    eprintln!(
        "[parity] settle: {} finding(s) in the first sample; resampling in {seconds}s",
        first.2.len(),
    );
    std::thread::sleep(std::time::Duration::from_secs(seconds));
    take()
}

fn run_check(
    args: &CheckArgs,
    previous_check_id: Option<u64>,
) -> Result<(CheckResult, Vec<Finding>)> {
    let mode = Mode::from(&args.mode);
    let snapshot_hash =
        file_sha256(&args.snapshot).with_context(|| format!("hash {}", args.snapshot.display()))?;
    let (expected, skipped) = load_expected(args)?;
    let (actual, scan, findings) = sample(args, mode, &expected)?;
    if !matches!(mode, Mode::Strict) && scan.pending != 0 {
        eprintln!(
            "[parity] warning: {} PendingObservations account(s)",
            scan.pending
        );
    }
    ensure!(
        file_sha256(&args.snapshot)? == snapshot_hash,
        "snapshot {} changed during the check",
        args.snapshot.display()
    );

    let states: BTreeMap<ComparedState, StateTotals> = compared_states(args.accountant)
        .into_iter()
        .map(|kind| {
            let summary = StateTotals {
                wormchain_count: expected.state_len(kind),
                wormchain_hash: hex32(&expected.state_hash(kind)),
                solana_count: actual.state_len(kind),
                solana_hash: hex32(&actual.state_hash(kind)),
            };
            (kind, summary)
        })
        .collect();
    let now = now_unix();
    let check = CheckResult {
        schema_version: SCHEMA_VERSION,
        // Unique and increasing per ledger: `report` groups finding lines by check id.
        check_id: previous_check_id.map_or(now, |previous| now.max(previous + 1)),
        ts: unix_to_rfc3339(now),
        mode: mode.to_string(),
        accountant: args.accountant,
        snapshot: args.snapshot.display().to_string(),
        snapshot_sha256: hex32(&snapshot_hash),
        rpc_url: args.rpc_url.clone(),
        program_id: args.program_id.to_string(),
        noreplay_program_id: args.noreplay_program_id.to_string(),
        states,
        governance_digests: skipped.governance_digests,
        pending: scan.pending,
        finding_count: findings.len(),
        clean: findings.is_empty(),
    };
    require_equal_states(&check)?;
    Ok((check, findings))
}

/// SECURITY: a clean check must show equal count and hash on every compared state.
fn require_equal_states(check: &CheckResult) -> Result<()> {
    if !check.clean {
        return Ok(());
    }
    for (kind, s) in &check.states {
        ensure!(
            s.wormchain_hash == s.solana_hash && s.wormchain_count == s.solana_count,
            "check {} is clean but {kind} differs: {s:?}",
            check.check_id
        );
    }
    Ok(())
}

fn print_summary(check: &CheckResult, findings: &[Finding]) {
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
    for entry in findings.iter().take(MAX_FINDING_LINES) {
        println!(
            "    [{}] {} wormchain={} solana={}",
            entry.kind,
            entry.key,
            entry.wormchain.as_deref().unwrap_or("absent"),
            entry.solana.as_deref().unwrap_or("absent"),
        );
    }
}
