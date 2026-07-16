//! Integration tests for the resumable cursor.

use ga_backfill::cursor::Cursor;
use tempfile::TempDir;

const HASH: &str = "0x7ea3b17ceb23a426a4b46d469fc24a9c4fc0e5239b2d196058d623720bb1d363";
const PROGRAM: &str = "TKyKMUPncinqKyoqVyrfCfa8jNaWATxpVBvkQ67jXzS";

fn setup() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
    let cursor_path = tmp.path().join("cursor.json");
    let catalogue_path = tmp.path().join("catalogue.jsonl");
    std::fs::write(&catalogue_path, "stub").expect("write catalogue");
    (tmp, cursor_path, catalogue_path)
}

#[test]
fn fresh_cursor_starts_at_zero() {
    let (_tmp, cursor_path, catalogue) = setup();
    let cursor = Cursor::load_or_init(cursor_path, &catalogue, HASH, PROGRAM).expect("load");
    assert_eq!(cursor.skip_count(), 0);
    assert_eq!(cursor.state().submitted, 0);
    assert_eq!(cursor.state().already_accounted, 0);
    assert_eq!(cursor.state().fees_lamports, 0);
}

#[test]
fn observed_events_advance_skip_count() {
    let (_tmp, cursor_path, catalogue) = setup();
    let mut cursor = Cursor::load_or_init(cursor_path, &catalogue, HASH, PROGRAM).expect("load");
    cursor
        .observe_confirmed(Some("sig1"), 5_000)
        .expect("observe");
    cursor
        .observe_confirmed(Some("sig2"), 5_000)
        .expect("observe");
    cursor.observe_already_accounted().expect("observe");
    assert_eq!(cursor.skip_count(), 3);
    assert_eq!(cursor.state().submitted, 2);
    assert_eq!(cursor.state().already_accounted, 1);
    assert_eq!(cursor.state().fees_lamports, 10_000);
    assert_eq!(cursor.state().last_tx_sig.as_deref(), Some("sig2"));
}

#[test]
fn cursor_persists_and_resumes() {
    let (_tmp, cursor_path, catalogue) = setup();
    {
        let mut cursor =
            Cursor::load_or_init(cursor_path.clone(), &catalogue, HASH, PROGRAM).expect("load");
        cursor
            .observe_confirmed(Some("sig1"), 5_000)
            .expect("observe");
        cursor.flush().expect("flush");
    }
    let cursor = Cursor::load_or_init(cursor_path, &catalogue, HASH, PROGRAM).expect("resume");
    assert_eq!(cursor.skip_count(), 1);
    assert_eq!(cursor.state().submitted, 1);
    assert_eq!(cursor.state().fees_lamports, 5_000);
}

#[test]
fn cursor_rejects_wrong_catalogue_hash() {
    let (_tmp, cursor_path, catalogue) = setup();
    {
        let mut cursor =
            Cursor::load_or_init(cursor_path.clone(), &catalogue, HASH, PROGRAM).expect("load");
        cursor.flush().expect("flush");
    }
    let result = Cursor::load_or_init(cursor_path, &catalogue, "0xDIFFERENT", PROGRAM);
    assert!(result.is_err(), "must reject catalogue-hash drift");
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("catalogue_content_hash") || msg.contains("catalogue"),
        "error should mention catalogue mismatch, got: {msg}"
    );
}

#[test]
fn cursor_rejects_wrong_program_id() {
    let (_tmp, cursor_path, catalogue) = setup();
    {
        let mut cursor =
            Cursor::load_or_init(cursor_path.clone(), &catalogue, HASH, PROGRAM).expect("load");
        cursor.flush().expect("flush");
    }
    let result = Cursor::load_or_init(cursor_path, &catalogue, HASH, "DIFFERENT_PROGRAM");
    assert!(result.is_err(), "must reject program-id drift");
}

#[test]
fn cursor_persists_on_stride_threshold() {
    let (_tmp, cursor_path, catalogue) = setup();
    let mut cursor =
        Cursor::load_or_init_with_stride(cursor_path.clone(), &catalogue, HASH, PROGRAM, 3)
            .expect("load");

    cursor.observe_confirmed(Some("a"), 5_000).expect("observe");
    cursor.observe_confirmed(Some("b"), 5_000).expect("observe");
    // After 2 events, stride (3) not yet hit; on-disk still reflects initial state
    let on_disk = std::fs::read_to_string(&cursor_path).expect("read");
    assert!(
        on_disk.contains("\"submitted\": 0") || on_disk.contains("\"submitted\":0"),
        "stride not yet hit; on-disk submitted should be 0, was: {on_disk}"
    );

    cursor.observe_confirmed(Some("c"), 5_000).expect("observe");
    // Third event hits stride; on-disk now reflects the latest state
    let on_disk = std::fs::read_to_string(&cursor_path).expect("read");
    assert!(
        on_disk.contains("\"submitted\": 3") || on_disk.contains("\"submitted\":3"),
        "stride hit; on-disk submitted should be 3, was: {on_disk}"
    );
}

#[test]
fn final_flush_persists_pending_events() {
    let (_tmp, cursor_path, catalogue) = setup();
    let mut cursor =
        Cursor::load_or_init_with_stride(cursor_path.clone(), &catalogue, HASH, PROGRAM, 1_000)
            .expect("load");
    cursor.observe_confirmed(Some("a"), 5_000).expect("observe");
    cursor.observe_confirmed(Some("b"), 5_000).expect("observe");
    // No flush triggered yet (stride 1000). Final flush must persist.
    cursor.flush().expect("final flush");
    let reloaded = Cursor::load_or_init(cursor_path, &catalogue, HASH, PROGRAM).expect("reload");
    assert_eq!(reloaded.skip_count(), 2);
    assert_eq!(reloaded.state().submitted, 2);
}

#[test]
fn no_tmp_file_left_after_successful_write() {
    let (_tmp, cursor_path, catalogue) = setup();
    let mut cursor =
        Cursor::load_or_init(cursor_path.clone(), &catalogue, HASH, PROGRAM).expect("load");
    cursor.observe_confirmed(Some("a"), 5_000).expect("observe");
    cursor.flush().expect("flush");
    // Verify no stray .tmp file alongside cursor.json
    let tmp_path = cursor_path.with_extension("json.tmp");
    assert!(!tmp_path.exists(), "temp file should be renamed away");
    // The cursor file itself exists
    assert!(cursor_path.exists());
}
