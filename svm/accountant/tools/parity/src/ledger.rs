//! Append-only JSONL ledger: one `check` line per run, one `finding` line per
//! finding. `report --at` replays the file and returns the check state at
//! that instant.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use ga_backfill::accountant::Accountant;

use crate::compare::Finding;
use crate::state::ComparedState;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum LedgerEvent {
    Check(CheckResult),
    Finding(FindingRecord),
}

/// Ledger length bound: one check line plus its finding lines per run.
pub const MAX_LEDGER_LINES: usize = 10_000_000;

/// Diff lines written per check; `finding_count` keeps the full total.
pub const MAX_FINDING_LINES: usize = 10_000;

/// `check` line schema. Lines with any other version are rejected.
pub const SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckResult {
    pub schema_version: u32,
    /// Id the `finding` lines point at: unix seconds, raised to stay unique and
    /// increasing within one ledger.
    pub check_id: u64,
    /// Wall-clock time of the check.
    pub ts: String,
    /// `strict` or `settle:<seconds>s`.
    pub mode: String,
    pub accountant: Accountant,
    pub snapshot: String,
    /// SHA-256 of the snapshot file. Guardians compare it out-of-band.
    pub snapshot_sha256: String,
    pub rpc_url: String,
    pub program_id: String,
    pub noreplay_program_id: String,
    /// Count and content hash per compared state.
    pub states: BTreeMap<ComparedState, StateTotals>,
    /// Snapshot `governance` rows, excluded from the backfill.
    pub governance_digests: usize,
    /// `PendingObservations` accounts in the last sample.
    pub pending: usize,
    pub finding_count: usize,
    pub clean: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateTotals {
    pub wormchain_count: usize,
    pub wormchain_hash: String,
    pub solana_count: usize,
    pub solana_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindingRecord {
    pub check_id: u64,
    #[serde(flatten)]
    pub entry: Finding,
}

pub fn append(path: &Path, check: &CheckResult, findings: &[Finding]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open ledger {}", path.display()))?;
    let mut write_line = |event: &LedgerEvent| -> Result<()> {
        let line = serde_json::to_string(event).context("serialize ledger event")?;
        writeln!(file, "{line}").context("append ledger line")
    };
    write_line(&LedgerEvent::Check(check.clone()))?;
    for entry in findings.iter().take(MAX_FINDING_LINES) {
        write_line(&LedgerEvent::Finding(FindingRecord {
            check_id: check.check_id,
            entry: entry.clone(),
        }))?;
    }
    file.flush().context("flush ledger")
}

pub fn replay(path: &Path) -> Result<Vec<LedgerEvent>> {
    let file =
        std::fs::File::open(path).with_context(|| format!("open ledger {}", path.display()))?;
    let mut out = Vec::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        if index == MAX_LEDGER_LINES {
            bail!(
                "ledger {} exceeds MAX_LEDGER_LINES ({MAX_LEDGER_LINES})",
                path.display()
            );
        }
        let line = line.with_context(|| format!("read ledger line {}", index + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(&line)
            .with_context(|| format!("parse ledger line {}", index + 1))?;
        if value.get("event").and_then(|e| e.as_str()) == Some("check") {
            let version = value
                .get("schema_version")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if version != u64::from(SCHEMA_VERSION) {
                bail!(
                    "ledger line {} is check schema v{version}; this binary reads v{SCHEMA_VERSION}",
                    index + 1
                );
            }
        }
        let event: LedgerEvent = serde_json::from_value(value)
            .with_context(|| format!("parse ledger line {}", index + 1))?;
        out.push(event);
    }
    Ok(out)
}

/// Latest check id and the one `(accountant, program_id)` a ledger holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestCheck {
    pub check_id: u64,
    pub accountant: Accountant,
    pub program_id: String,
}

/// `None` for a ledger with no check. Fails on a ledger that mixes targets.
pub fn latest_check(events: &[LedgerEvent]) -> Result<Option<LatestCheck>> {
    let mut head: Option<LatestCheck> = None;
    for event in events {
        let LedgerEvent::Check(check) = event else {
            continue;
        };
        if let Some(h) = &head {
            if (h.accountant, h.program_id.as_str())
                != (check.accountant, check.program_id.as_str())
            {
                bail!(
                    "ledger mixes {} {} with {} {}",
                    h.accountant,
                    h.program_id,
                    check.accountant,
                    check.program_id
                );
            }
        }
        let check_id = head
            .as_ref()
            .map_or(check.check_id, |h| h.check_id.max(check.check_id));
        head = Some(LatestCheck {
            check_id,
            accountant: check.accountant,
            program_id: check.program_id.clone(),
        });
    }
    Ok(head)
}

/// The latest check at or before `at_unix`, with its findings.
pub fn report_at(events: &[LedgerEvent], at_unix: u64) -> Option<(CheckResult, Vec<Finding>)> {
    let check = events
        .iter()
        .filter_map(|event| match event {
            LedgerEvent::Check(check) if check.check_id <= at_unix => Some(check),
            _ => None,
        })
        .max_by_key(|check| check.check_id)?
        .clone();
    let findings = events
        .iter()
        .filter_map(|event| match event {
            LedgerEvent::Finding(diff) if diff.check_id == check.check_id => {
                Some(diff.entry.clone())
            }
            _ => None,
        })
        .collect();
    Some((check, findings))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(check_id: u64, clean: bool) -> CheckResult {
        let summary = StateTotals {
            wormchain_count: 1,
            wormchain_hash: "0xaa".into(),
            solana_count: 1,
            solana_hash: "0xaa".into(),
        };
        CheckResult {
            schema_version: SCHEMA_VERSION,
            check_id,
            ts: crate::time::unix_to_rfc3339(check_id),
            mode: "strict".into(),
            accountant: Accountant::Wtt,
            snapshot: "catalogue.jsonl".into(),
            snapshot_sha256: "0x00".into(),
            rpc_url: "http://localhost".into(),
            program_id: "prog".into(),
            noreplay_program_id: "noreplay".into(),
            states: BTreeMap::from([(ComparedState::Balances, summary)]),
            governance_digests: 0,
            pending: 0,
            finding_count: usize::from(!clean),
            clean,
        }
    }

    fn entry() -> Finding {
        Finding {
            kind: ComparedState::ReplayMarks,
            key: "chain=2 emitter=0xee sequence=5".into(),
            wormchain: Some("marked".into()),
            solana: None,
        }
    }

    #[test]
    fn append_replay_and_report_at() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");

        append(&path, &check(100, false), &[entry()]).unwrap();
        append(&path, &check(200, true), &[]).unwrap();

        let events = replay(&path).unwrap();
        assert_eq!(events.len(), 3, "two checks + one diff");

        assert!(report_at(&events, 99).is_none(), "before the first check");

        let (at_first, findings) = report_at(&events, 150).unwrap();
        assert_eq!(at_first.check_id, 100);
        assert!(!at_first.clean);
        assert_eq!(findings, vec![entry()]);

        let (at_second, findings) = report_at(&events, 200).unwrap();
        assert_eq!(at_second.check_id, 200);
        assert!(at_second.clean);
        assert!(findings.is_empty());
    }

    #[test]
    fn replay_rejects_garbage_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        std::fs::write(&path, "{\"event\":\"nonsense\"}\n").unwrap();
        assert!(replay(&path).is_err());
    }
}
