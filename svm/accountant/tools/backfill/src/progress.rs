//! Resumable progress file for the backfill orchestrator.
//!
//! Persists `(identity, program_id, batches_done, running_totals)` to a
//! small JSON file, by default `progress.json` next to the snapshot. A later
//! `run` reloads it, requires the same identity and program id, and verifies
//! the claimed batches on chain before skipping them.
//!
//! ## Atomic write
//!
//! Persistence uses the canonical write-tempfile-then-rename pattern:
//! `progress.json.tmp` is written + `fsync`'d, then `rename(2)`'d over
//! `progress.json`. On POSIX filesystems the rename is atomic, so a reader
//! sees either the old file or the new one.
//!
//! ## Granularity
//!
//! The progress file flushes every `flush_stride` events. On a crash, batches after
//! the last flush are resubmitted on resume; the program rejects each as
//! already written, and the submitter counts it done only after verifying the
//! whole batch on chain.
//!
//! ## Identity
//!
//! [`RunIdentity`]: the snapshot content hash, the batch-plan fingerprint
//! and the program id. A changed snapshot or batcher yields a different
//! identity, and the progress file refuses to resume.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, ensure, Context, Result};
use serde::{Deserialize, Serialize};

/// Default flush stride — write the progress file every N observed events.
pub const DEFAULT_FLUSH_STRIDE: u64 = 100;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProgressState {
    /// Source snapshot file path (informational, not used for verification).
    pub snapshot_path: String,
    /// Snapshot SHA-256. Mismatch on resume aborts.
    pub snapshot_sha256: String,
    /// Batch-plan fingerprint. Mismatch on resume aborts.
    #[serde(default)]
    pub batches_hash: String,
    /// On-chain program id (base58). Mismatch on resume aborts.
    pub program_id: String,
    /// Number of snapshot batches confirmed so far. The orchestrator skips
    /// this many batches before resuming submission.
    pub batches_done: u64,
    /// Batches whose transaction confirmed.
    #[serde(alias = "submitted")]
    pub confirmed: u64,
    /// Batches the program rejected as already written and that verified
    /// fully written on chain.
    pub already_written: u64,
    /// Cumulative tx fees paid.
    pub fees_lamports: u64,
    /// Most recent successful tx signature, for human inspection.
    pub last_tx_sig: Option<String>,
    /// Wall-clock when the first run started (seconds since unix epoch).
    pub started_at_epoch_secs: u64,
    /// Wall-clock of the most recent observed event.
    pub last_updated_at_epoch_secs: u64,
}

/// What a progress file must match to resume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunIdentity {
    pub snapshot_sha256: String,
    pub batches_hash: String,
    pub program_id: String,
}

impl ProgressState {
    fn require(&self, want: &RunIdentity) -> Result<()> {
        ensure!(
            self.snapshot_sha256 == want.snapshot_sha256,
            "progress file snapshot_sha256 mismatch: stored {} != current {}; refusing to resume against a different snapshot",
            self.snapshot_sha256,
            want.snapshot_sha256
        );
        ensure!(
            self.batches_hash == want.batches_hash,
            "progress file batches_hash mismatch: stored {} != current {}; refusing to resume under a different batch plan. A new progress file halts at the first batch that straddles written and unwritten entries",
            self.batches_hash,
            want.batches_hash
        );
        ensure!(
            self.program_id == want.program_id,
            "progress file program_id mismatch: stored {} != current {}; refusing to resume against a different accountant",
            self.program_id,
            want.program_id
        );
        Ok(())
    }
}

#[derive(Debug)]
pub struct ProgressFile {
    state: ProgressState,
    path: PathBuf,
    flush_stride: u64,
    events_since_flush: u64,
}

impl ProgressFile {
    /// Load the progress file at `path` and require `identity`, or start a fresh one.
    pub fn open(
        path: PathBuf,
        snapshot_path: &Path,
        identity: &RunIdentity,
        flush_stride: u64,
    ) -> Result<Self> {
        ensure!(flush_stride > 0, "flush stride must be positive");
        ensure!(
            !identity.batches_hash.is_empty(),
            "run identity has no batch-plan hash"
        );
        let (state, fresh) = match fs::read(&path) {
            Ok(bytes) => {
                let state: ProgressState =
                    serde_json::from_slice(&bytes).context("parse progress file JSON")?;
                state.require(identity)?;
                (state, false)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let now = now_epoch_secs();
                let state = ProgressState {
                    snapshot_path: snapshot_path.to_string_lossy().into_owned(),
                    snapshot_sha256: identity.snapshot_sha256.clone(),
                    batches_hash: identity.batches_hash.clone(),
                    program_id: identity.program_id.clone(),
                    batches_done: 0,
                    confirmed: 0,
                    already_written: 0,
                    fees_lamports: 0,
                    last_tx_sig: None,
                    started_at_epoch_secs: now,
                    last_updated_at_epoch_secs: now,
                };
                (state, true)
            }
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        let mut me = Self {
            state,
            path,
            flush_stride,
            events_since_flush: 0,
        };
        // Persist a fresh progress file so a subsequent run can verify identity
        // without an empty-file race.
        if fresh {
            me.write_to_disk()?;
        }
        Ok(me)
    }

    /// Number of snapshot batches the orchestrator should skip when
    /// resuming a run.
    pub fn batches_done(&self) -> u64 {
        self.state.batches_done
    }

    pub fn state(&self) -> &ProgressState {
        &self.state
    }

    /// Record one successfully-confirmed batch. Triggers a flush if the
    /// stride threshold is reached.
    pub fn observe_confirmed(&mut self, sig: Option<&str>, fee_lamports: u64) -> Result<()> {
        self.state.batches_done += 1;
        self.state.confirmed += 1;
        self.state.fees_lamports += fee_lamports;
        if let Some(s) = sig {
            self.state.last_tx_sig = Some(s.to_owned());
        }
        self.tick()
    }

    /// Record one batch rejected as already written and verified on chain.
    /// Fees are not counted: a simulation rejection pays none, an execution
    /// rejection pays one base fee.
    pub fn observe_already_written(&mut self) -> Result<()> {
        self.state.batches_done += 1;
        self.state.already_written += 1;
        self.tick()
    }

    /// Force-flush to disk regardless of stride. Call at end of run.
    pub fn flush(&mut self) -> Result<()> {
        self.write_to_disk()?;
        self.events_since_flush = 0;
        Ok(())
    }

    fn tick(&mut self) -> Result<()> {
        self.events_since_flush += 1;
        if self.events_since_flush >= self.flush_stride {
            self.flush()?;
        }
        Ok(())
    }

    fn write_to_disk(&mut self) -> Result<()> {
        self.state.last_updated_at_epoch_secs = now_epoch_secs();
        let bytes = serde_json::to_vec_pretty(&self.state).context("serialise progress file")?;

        // Temp file lives next to the target so the rename is intra-directory
        // (intra-filesystem) and POSIX-atomic.
        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow!("progress file path has no parent: {}", self.path.display()))?;
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| format!("mkdir -p {}", parent.display()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        {
            let mut f = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
            f.write_all(&bytes).context("write progress file body")?;
            f.sync_all().context("fsync progress file body")?;
        }
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("rename {} → {}", tmp.display(), self.path.display()))?;
        Ok(())
    }
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs()
}
