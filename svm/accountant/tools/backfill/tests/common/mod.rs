//! Shared test infrastructure: the mock RPC server and fixed targets.

#![allow(dead_code)] // Different test files use different subsets.

pub mod mock_rpc;

use solana_pubkey::Pubkey;
use svm_accountant_backfill::target::BackfillTarget;
use svm_accountant_tools_common::accountant::Accountant;

/// [`BackfillTarget::try_new`] for fixed test values. Panics on a mismatch.
pub fn target(accountant: Accountant, program_id: Pubkey, payer: Pubkey) -> BackfillTarget {
    BackfillTarget::try_new(accountant, program_id, payer).unwrap_or_else(|e| panic!("{e:#}"))
}

/// Strict parity findings of `snapshot` against the deployed program, as `run` computes them.
pub async fn parity_findings(
    rpc_url: &str,
    target: &BackfillTarget,
    snapshot: &std::path::Path,
) -> Vec<svm_accountant_tools_common::parity::compare::Finding> {
    let rows = svm_accountant_tools_common::snapshot::read_rows(snapshot).expect("read snapshot");
    let (expected, _) = svm_accountant_tools_common::parity::expected_state::from_snapshot(
        &rows,
        target.accountant(),
    )
    .expect("expected state");
    let confirmed = solana_client::nonblocking::rpc_client::RpcClient::new_with_commitment(
        rpc_url.to_string(),
        solana_commitment_config::CommitmentConfig::confirmed(),
    );
    svm_accountant_backfill::run::parity_findings(&confirmed, rpc_url, None, target, &expected)
        .await
        .expect("parity check")
}

/// A `confirmed` async client for a surfpool instance.
pub fn async_rpc_for(
    guard: &svm_accountant_tools_common::test_support::surfpool::SurfpoolGuard,
) -> solana_client::nonblocking::rpc_client::RpcClient {
    solana_client::nonblocking::rpc_client::RpcClient::new_with_commitment(
        guard.rpc_url(),
        solana_commitment_config::CommitmentConfig::confirmed(),
    )
}

/// A [`svm_accountant_backfill::progress::RunIdentity`] with fixed plan hash, for tests that drive the submitter directly.
pub fn test_identity(
    snapshot_sha256: &str,
    program_id: &str,
) -> svm_accountant_backfill::progress::RunIdentity {
    svm_accountant_backfill::progress::RunIdentity {
        snapshot_sha256: snapshot_sha256.to_owned(),
        batches_hash: "0xtest-plan".to_owned(),
        program_id: program_id.to_owned(),
    }
}
