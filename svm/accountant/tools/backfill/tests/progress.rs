//! Integration tests for the resumable progress file.

use svm_accountant_backfill::progress::{ProgressFile, RunIdentity, DEFAULT_FLUSH_STRIDE};
use tempfile::TempDir;

const HASH: &str = "0x7ea3b17ceb23a426a4b46d469fc24a9c4fc0e5239b2d196058d623720bb1d363";
const PROGRAM: &str = "TKyKMUPncinqKyoqVyrfCfa8jNaWATxpVBvkQ67jXzS";

const PLAN: &str = "0x00000000000000000000000000000000000000000000000000000000000000aa";

fn setup() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
    let progress_path = tmp.path().join("progress.json");
    let snapshot_path = tmp.path().join("snapshot.jsonl");
    std::fs::write(&snapshot_path, "stub").expect("write snapshot");
    (tmp, progress_path, snapshot_path)
}

fn identity(snapshot_sha256: &str, program_id: &str) -> RunIdentity {
    RunIdentity {
        snapshot_sha256: snapshot_sha256.to_owned(),
        batches_hash: PLAN.to_owned(),
        program_id: program_id.to_owned(),
    }
}

fn open_with_stride(
    path: std::path::PathBuf,
    snapshot: &std::path::Path,
    snapshot_sha256: &str,
    program_id: &str,
    stride: u64,
) -> anyhow::Result<ProgressFile> {
    ProgressFile::open(
        path,
        snapshot,
        &identity(snapshot_sha256, program_id),
        stride,
    )
}

fn open(
    path: std::path::PathBuf,
    snapshot: &std::path::Path,
    snapshot_sha256: &str,
    program_id: &str,
) -> anyhow::Result<ProgressFile> {
    open_with_stride(
        path,
        snapshot,
        snapshot_sha256,
        program_id,
        DEFAULT_FLUSH_STRIDE,
    )
}

#[test]
fn fresh_progress_starts_at_zero() {
    let (_tmp, progress_path, snapshot) = setup();
    let progress_file = open(progress_path, &snapshot, HASH, PROGRAM).expect("load");
    assert_eq!(progress_file.batches_done(), 0);
    assert_eq!(progress_file.state().confirmed, 0);
    assert_eq!(progress_file.state().already_written, 0);
    assert_eq!(progress_file.state().fees_lamports, 0);
}

#[test]
fn observed_events_advance_skip_count() {
    let (_tmp, progress_path, snapshot) = setup();
    let mut progress_file = open(progress_path, &snapshot, HASH, PROGRAM).expect("load");
    progress_file
        .observe_confirmed(Some("sig1"), 5_000)
        .expect("observe");
    progress_file
        .observe_confirmed(Some("sig2"), 5_000)
        .expect("observe");
    progress_file.observe_already_written().expect("observe");
    assert_eq!(progress_file.batches_done(), 3);
    assert_eq!(progress_file.state().confirmed, 2);
    assert_eq!(progress_file.state().already_written, 1);
    assert_eq!(progress_file.state().fees_lamports, 10_000);
    assert_eq!(progress_file.state().last_tx_sig.as_deref(), Some("sig2"));
}

#[test]
fn progress_persists_and_resumes() {
    let (_tmp, progress_path, snapshot) = setup();
    {
        let mut progress_file =
            open(progress_path.clone(), &snapshot, HASH, PROGRAM).expect("load");
        progress_file
            .observe_confirmed(Some("sig1"), 5_000)
            .expect("observe");
        progress_file.flush().expect("flush");
    }
    let progress_file = open(progress_path, &snapshot, HASH, PROGRAM).expect("resume");
    assert_eq!(progress_file.batches_done(), 1);
    assert_eq!(progress_file.state().confirmed, 1);
    assert_eq!(progress_file.state().fees_lamports, 5_000);
}

#[test]
fn progress_rejects_wrong_snapshot_hash() {
    let (_tmp, progress_path, snapshot) = setup();
    {
        let mut progress_file =
            open(progress_path.clone(), &snapshot, HASH, PROGRAM).expect("load");
        progress_file.flush().expect("flush");
    }
    let result = open(progress_path, &snapshot, "0xDIFFERENT", PROGRAM);
    assert!(result.is_err(), "must reject snapshot-hash drift");
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("snapshot_sha256") || msg.contains("snapshot"),
        "error should mention snapshot mismatch, got: {msg}"
    );
}

#[test]
fn progress_rejects_wrong_program_id() {
    let (_tmp, progress_path, snapshot) = setup();
    {
        let mut progress_file =
            open(progress_path.clone(), &snapshot, HASH, PROGRAM).expect("load");
        progress_file.flush().expect("flush");
    }
    let result = open(progress_path, &snapshot, HASH, "DIFFERENT_PROGRAM");
    assert!(result.is_err(), "must reject program-id drift");
}

#[test]
fn progress_persists_on_stride_threshold() {
    let (_tmp, progress_path, snapshot) = setup();
    let mut progress_file =
        open_with_stride(progress_path.clone(), &snapshot, HASH, PROGRAM, 3).expect("load");

    progress_file
        .observe_confirmed(Some("a"), 5_000)
        .expect("observe");
    progress_file
        .observe_confirmed(Some("b"), 5_000)
        .expect("observe");
    // After 2 events, stride (3) not yet hit; on-disk still reflects initial state
    let on_disk = std::fs::read_to_string(&progress_path).expect("read");
    assert!(
        on_disk.contains("\"confirmed\": 0") || on_disk.contains("\"confirmed\":0"),
        "stride not yet hit; on-disk confirmed should be 0, was: {on_disk}"
    );

    progress_file
        .observe_confirmed(Some("c"), 5_000)
        .expect("observe");
    // Third event hits stride; on-disk now reflects the latest state
    let on_disk = std::fs::read_to_string(&progress_path).expect("read");
    assert!(
        on_disk.contains("\"confirmed\": 3") || on_disk.contains("\"confirmed\":3"),
        "stride hit; on-disk confirmed should be 3, was: {on_disk}"
    );
}

#[test]
fn final_flush_persists_pending_events() {
    let (_tmp, progress_path, snapshot) = setup();
    let mut progress_file =
        open_with_stride(progress_path.clone(), &snapshot, HASH, PROGRAM, 1_000).expect("load");
    progress_file
        .observe_confirmed(Some("a"), 5_000)
        .expect("observe");
    progress_file
        .observe_confirmed(Some("b"), 5_000)
        .expect("observe");
    // No flush triggered yet (stride 1000). Final flush must persist.
    progress_file.flush().expect("final flush");
    let reloaded = open(progress_path, &snapshot, HASH, PROGRAM).expect("reload");
    assert_eq!(reloaded.batches_done(), 2);
    assert_eq!(reloaded.state().confirmed, 2);
}

#[test]
fn no_tmp_file_left_after_successful_write() {
    let (_tmp, progress_path, snapshot) = setup();
    let mut progress_file = open(progress_path.clone(), &snapshot, HASH, PROGRAM).expect("load");
    progress_file
        .observe_confirmed(Some("a"), 5_000)
        .expect("observe");
    progress_file.flush().expect("flush");
    // Verify no stray .tmp file alongside progress.json
    let tmp_path = progress_path.with_extension("json.tmp");
    assert!(!tmp_path.exists(), "temp file should be renamed away");
    // The progress file itself exists
    assert!(progress_path.exists());
}

// Corrupted / partially-written progress.json (disk corruption, manual edit,
// interrupted copy, older non-atomic writer) must fail loudly rather than
// be silently treated as absent — a silent reinit would reset
// `batches_done` to 0 and resubmit the whole snapshot.

#[test]
fn truncated_mid_write_progress_json_fails_loudly_not_silently() {
    let (_tmp, progress_path, snapshot) = setup();
    // Build a real, valid progress file first...
    {
        let mut progress_file =
            open(progress_path.clone(), &snapshot, HASH, PROGRAM).expect("load");
        progress_file
            .observe_confirmed(Some("sig1"), 5_000)
            .expect("observe");
        progress_file.flush().expect("flush");
    }
    // Simulate a crash mid-write by truncating the file partway through.
    let full = std::fs::read(&progress_path).expect("read written progress file");
    assert!(
        full.len() > 10,
        "sanity: progress_file file has real content"
    );
    let truncated = &full[..full.len() / 2];
    std::fs::write(&progress_path, truncated).expect("write truncated progress file");

    let result = open(progress_path, &snapshot, HASH, PROGRAM);
    assert!(
        result.is_err(),
        "a truncated progress_file.json must fail loudly, not silently reinitialise \
         (a silent reinit would blindly resubmit the whole snapshot on resume)"
    );
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("parse") || msg.contains("JSON") || msg.contains("json"),
        "error should identify the parse failure, got: {msg}"
    );
}

#[test]
fn invalid_json_progress_file_fails_loudly() {
    let (_tmp, progress_path, snapshot) = setup();
    std::fs::write(&progress_path, b"{ this is not valid json at all")
        .expect("write garbage progress file");

    let result = open(progress_path, &snapshot, HASH, PROGRAM);
    assert!(result.is_err(), "invalid JSON must fail loudly");
}

#[test]
fn empty_progress_file_fails_loudly() {
    // A zero-byte file (e.g. from an external tool or manual `touch`) must
    // not be treated as fresh state.
    let (_tmp, progress_path, snapshot) = setup();
    std::fs::write(&progress_path, b"").expect("write empty progress file");

    let result = open(progress_path, &snapshot, HASH, PROGRAM);
    assert!(
        result.is_err(),
        "empty progress_file.json must fail loudly, not be treated as absent"
    );
}

#[test]
fn reads_a_progress_file_written_with_the_submitted_field() {
    let (_tmp, progress_path, snapshot) = setup();
    {
        let mut progress_file =
            open(progress_path.clone(), &snapshot, HASH, PROGRAM).expect("load");
        progress_file
            .observe_confirmed(Some("sig"), 5_000)
            .expect("observe");
        progress_file.flush().expect("flush");
    }
    let written = std::fs::read_to_string(&progress_path).expect("read");
    let legacy = written.replace("\"confirmed\"", "\"submitted\"");
    assert_ne!(written, legacy, "the file holds a confirmed count");
    std::fs::write(&progress_path, legacy).expect("write legacy");

    let progress_file = open(progress_path, &snapshot, HASH, PROGRAM).expect("reload legacy");
    assert_eq!(progress_file.state().confirmed, 1);
    assert_eq!(progress_file.batches_done(), 1);
}

#[test]
fn rejects_a_zero_flush_stride_and_an_empty_plan_hash() {
    let (_tmp, progress_path, snapshot) = setup();
    assert!(open_with_stride(progress_path.clone(), &snapshot, HASH, PROGRAM, 0).is_err());
    let no_plan = RunIdentity {
        batches_hash: String::new(),
        ..identity(HASH, PROGRAM)
    };
    assert!(ProgressFile::open(progress_path, &snapshot, &no_plan, 1).is_err());
}
