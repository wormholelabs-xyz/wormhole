//! One parity sample: read Solana, compare against the expected state, and
//! collect every finding. `svm-accountant-parity check` and `svm-accountant-backfill run` both call [`sample`].

use std::fmt;
use std::time::Duration;

use anyhow::Result;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_pubkey::Pubkey;

use crate::accountant::Accountant;
use crate::parity::compare::{compare, Finding};
use crate::parity::onchain_state::{self, SolanaOnlyFindings};
use crate::parity::state::{ChainState, FindingKind, KeyedKind, StateKind};

/// Longest settle interval.
pub const MAX_SETTLE_SECONDS: u64 = 3_600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Single sample; any `PendingObservations` account is a finding. For quiesced state.
    Strict,
    /// Sample, wait this many seconds, and report the second sample.
    Settle(u64),
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mode::Strict => f.write_str("strict"),
            Mode::Settle(seconds) => write!(f, "settle:{seconds}s"),
        }
    }
}

/// The deployed program a check reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckTarget {
    pub accountant: Accountant,
    pub program_id: Pubkey,
    pub noreplay_program: Pubkey,
}

/// The reported sample of one check.
#[derive(Debug)]
pub struct Sample {
    pub actual: ChainState,
    pub solana_only: SolanaOnlyFindings,
    pub findings: Vec<Finding>,
}

/// States compared for `accountant`, in report order.
pub fn compared_states(accountant: Accountant) -> Vec<StateKind> {
    let mut out = vec![
        StateKind::BALANCES,
        StateKind::ReplayMarks,
        StateKind::Keyed(KeyedKind::ModifyBalanceRecords),
        StateKind::Keyed(KeyedKind::ChainRegistrations),
        StateKind::Keyed(KeyedKind::RegisterChainRecords),
    ];
    if accountant == Accountant::Ntt {
        out.extend([
            StateKind::Keyed(KeyedKind::TransceiverHubs),
            StateKind::Keyed(KeyedKind::TransceiverPeers),
        ]);
    }
    out
}

/// All findings of one sample: state differences, malformed accounts, and in
/// strict mode a non-zero pending count.
pub fn collect_findings(
    expected: &ChainState,
    actual: &ChainState,
    solana_only: &SolanaOnlyFindings,
    mode: Mode,
) -> Vec<Finding> {
    let mut out = compare(expected, actual);
    out.extend(solana_only.malformed.iter().cloned());
    if mode == Mode::Strict && solana_only.pending_observations != 0 {
        out.push(Finding {
            kind: FindingKind::PendingObservations,
            key: "count".to_string(),
            expected: Some("0".to_string()),
            actual: Some(solana_only.pending_observations.to_string()),
        });
    }
    out
}

/// Sample Solana per `mode`; in settle mode the second sample is reported.
/// A finding gone by the second sample was lag; one new in it is current.
pub async fn sample(
    rpc: &RpcClient,
    target: &CheckTarget,
    expected: &ChainState,
    mode: Mode,
) -> Result<Sample> {
    if let Mode::Settle(seconds) = mode {
        assert!(
            (1..=MAX_SETTLE_SECONDS).contains(&seconds),
            "settle {seconds}s out of range"
        );
    }
    let take = || async {
        let (actual, solana_only) = onchain_state::fetch(
            rpc,
            &target.program_id,
            &target.noreplay_program,
            target.accountant,
            &expected.replay_marks,
        )
        .await?;
        let findings = collect_findings(expected, &actual, &solana_only, mode);
        anyhow::Ok(Sample {
            actual,
            solana_only,
            findings,
        })
    };
    let first = take().await?;
    let Mode::Settle(seconds) = mode else {
        return Ok(first);
    };
    eprintln!(
        "[parity] settle: {} finding(s) in the first sample; resampling in {seconds}s",
        first.findings.len(),
    );
    tokio::time::sleep(Duration::from_secs(seconds)).await;
    take().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_observations_are_a_finding_in_strict_mode_only() {
        let state = ChainState::default();
        let solana_only = SolanaOnlyFindings {
            malformed: vec![],
            pending_observations: 2,
        };
        let cases = [(Mode::Strict, 1), (Mode::Settle(1), 0)];
        for (mode, want) in cases {
            let findings = collect_findings(&state, &state, &solana_only, mode);
            assert_eq!(findings.len(), want, "{mode}");
            if want == 1 {
                assert_eq!(findings[0].kind, FindingKind::PendingObservations);
                assert_eq!(findings[0].actual.as_deref(), Some("2"));
            }
        }
        let quiet = SolanaOnlyFindings::default();
        assert!(collect_findings(&state, &state, &quiet, Mode::Strict).is_empty());
    }

    #[test]
    fn ntt_compares_the_transceiver_maps() {
        assert_eq!(compared_states(Accountant::Wtt).len(), 5);
        let ntt = compared_states(Accountant::Ntt);
        assert_eq!(ntt.len(), 7);
        assert!(ntt.contains(&StateKind::Keyed(KeyedKind::TransceiverPeers)));
    }
}
