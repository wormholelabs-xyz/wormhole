//! Resumable progress file for the backfill orchestrator.
//!
//! Persists `(catalogue_hash, program_id, chunks_confirmed, running_totals)`
//! to a small JSON file under `target/backfill/` (or wherever the operator
//! points). A `--resume` run reloads the file, verifies the identity
//! triple matches, and skips the first `chunks_confirmed` chunks before
//! resuming submission.
//!
//! ## Atomic write
//!
//! Persistence uses the canonical write-tempfile-then-rename pattern:
//! `cursor.json.tmp` is written + `fsync`'d, then `rename(2)`'d over
//! `cursor.json`. On POSIX filesystems the rename is atomic, so the
//! cursor file is never observable in a half-written state.
//!
//! ## Granularity
//!
//! Persisting every confirmed chunk is wasteful (~300k disk writes on a
//! full run). Instead the cursor flushes every `persist_stride` events
//! (default 100). On crash, up to `persist_stride - 1` chunks may be
//! re-submitted on resume — each one is a wasted ~5,000 L tx fee, bounded
//! at ~$0.20 of waste per crash. NoReplay's `AlreadyAccounted` rejection
//! handles the re-submission safely; the cursor is a performance
//! optimisation, not a correctness primitive.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Default flush stride — persist the cursor every N observed events.
pub const DEFAULT_PERSIST_STRIDE: u64 = 100;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CursorState {
    /// Source catalogue file path (informational, not used for verification).
    pub catalogue_path: String,
    /// SHA-256 (hex with 0x prefix) of the catalogue file content. Mismatch
    /// on resume aborts.
    pub catalogue_content_hash: String,
    /// On-chain program id (base58). Mismatch on resume aborts.
    pub program_id: String,
    /// Number of catalogue chunks confirmed so far. The orchestrator skips
    /// this many chunks before resuming submission.
    pub last_confirmed_chunk_index: u64,
    /// Successful chunk submissions (Confirmed outcome).
    pub submitted: u64,
    /// Skipped chunks because NoReplay reported `AlreadyAccounted` (treated
    /// as success).
    pub already_accounted: u64,
    /// Cumulative tx fees paid.
    pub fees_lamports: u64,
    /// Most recent successful tx signature, for human inspection.
    pub last_tx_sig: Option<String>,
    /// Wall-clock when the first run started (seconds since unix epoch).
    pub started_at_epoch_secs: u64,
    /// Wall-clock of the most recent observed event.
    pub last_updated_at_epoch_secs: u64,
}

#[derive(Debug)]
pub struct Cursor {
    state: CursorState,
    path: PathBuf,
    persist_stride: u64,
    events_since_persist: u64,
}

impl Cursor {
    /// Load existing cursor or initialise fresh. Uses the default flush stride.
    pub fn load_or_init(
        path: PathBuf,
        catalogue_path: &Path,
        catalogue_content_hash: &str,
        program_id: &str,
    ) -> Result<Self> {
        Self::load_or_init_with_stride(
            path,
            catalogue_path,
            catalogue_content_hash,
            program_id,
            DEFAULT_PERSIST_STRIDE,
        )
    }

    /// As [`load_or_init`] but with a caller-chosen persist stride. Lower
    /// strides write more often (less crash loss, more disk thrash).
    pub fn load_or_init_with_stride(
        path: PathBuf,
        catalogue_path: &Path,
        catalogue_content_hash: &str,
        program_id: &str,
        persist_stride: u64,
    ) -> Result<Self> {
        if path.exists() {
            let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            let state: CursorState = serde_json::from_slice(&bytes).context("parse cursor JSON")?;
            if state.catalogue_content_hash != catalogue_content_hash {
                bail!(
                    "cursor catalogue_content_hash mismatch: stored {} != current {}; refusing to resume against a different catalogue",
                    state.catalogue_content_hash,
                    catalogue_content_hash
                );
            }
            if state.program_id != program_id {
                bail!(
                    "cursor program_id mismatch: stored {} != current {}; refusing to resume against a different deployment",
                    state.program_id,
                    program_id
                );
            }
            return Ok(Self {
                state,
                path,
                persist_stride: persist_stride.max(1),
                events_since_persist: 0,
            });
        }

        let now = now_epoch_secs();
        let state = CursorState {
            catalogue_path: catalogue_path.to_string_lossy().into_owned(),
            catalogue_content_hash: catalogue_content_hash.to_owned(),
            program_id: program_id.to_owned(),
            last_confirmed_chunk_index: 0,
            submitted: 0,
            already_accounted: 0,
            fees_lamports: 0,
            last_tx_sig: None,
            started_at_epoch_secs: now,
            last_updated_at_epoch_secs: now,
        };
        let mut me = Self {
            state,
            path,
            persist_stride: persist_stride.max(1),
            events_since_persist: 0,
        };
        // Persist the initial state so a subsequent run can verify identity
        // without an empty-file race.
        me.write_to_disk()?;
        Ok(me)
    }

    /// Number of catalogue chunks the orchestrator should skip when
    /// resuming a run.
    pub fn skip_count(&self) -> u64 {
        self.state.last_confirmed_chunk_index
    }

    pub fn state(&self) -> &CursorState {
        &self.state
    }

    /// Record one successfully-confirmed chunk. Triggers a flush if the
    /// stride threshold is reached.
    pub fn observe_confirmed(&mut self, sig: Option<&str>, fee_lamports: u64) -> Result<()> {
        self.state.last_confirmed_chunk_index += 1;
        self.state.submitted += 1;
        self.state.fees_lamports += fee_lamports;
        if let Some(s) = sig {
            self.state.last_tx_sig = Some(s.to_owned());
        }
        self.tick()
    }

    /// Record one chunk that the program rejected as `AlreadyAccounted` —
    /// no tx fee was paid (preflight rejected before submission) so fees
    /// aren't incremented, but the chunk counter advances.
    pub fn observe_already_accounted(&mut self) -> Result<()> {
        self.state.last_confirmed_chunk_index += 1;
        self.state.already_accounted += 1;
        self.tick()
    }

    /// Force-flush to disk regardless of stride. Call at end of run.
    pub fn flush(&mut self) -> Result<()> {
        self.write_to_disk()?;
        self.events_since_persist = 0;
        Ok(())
    }

    fn tick(&mut self) -> Result<()> {
        self.events_since_persist += 1;
        if self.events_since_persist >= self.persist_stride {
            self.flush()?;
        }
        Ok(())
    }

    fn write_to_disk(&mut self) -> Result<()> {
        self.state.last_updated_at_epoch_secs = now_epoch_secs();
        let bytes = serde_json::to_vec_pretty(&self.state).context("serialise cursor")?;

        // Temp file lives next to the target so the rename is intra-directory
        // (intra-filesystem) and POSIX-atomic.
        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow!("cursor path has no parent: {}", self.path.display()))?;
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| format!("mkdir -p {}", parent.display()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        {
            let mut f = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
            f.write_all(&bytes).context("write cursor body")?;
            f.sync_all().context("fsync cursor body")?;
        }
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("rename {} → {}", tmp.display(), self.path.display()))?;
        Ok(())
    }
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
