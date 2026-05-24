//! Crash-recovery tests for the editor draft auto-save.
//!
//! The contract: a panic mid-edit must not lose the operator's
//! buffer, provided the `run()` loop has already fired its 500ms
//! periodic save (which it does after every event when dirty).
//! `util::write_atomic` is what makes this work — a rename either
//! lands intact or doesn't land, never a truncated file.
//!
//! These tests use `persist_draft_to` / `load_draft_from` so each
//! test owns a unique temp path and parallel tests don't race on
//! the production `draft.sql`.

use pgman::app::{load_draft_from, persist_draft_to};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn unique_draft_path() -> PathBuf {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "pgman-crash-test-{}-{}.sql",
        std::process::id(),
        seq
    ))
}

#[test]
fn draft_persists_after_writing_thread_panics() {
    // The high-level contract: if persist completes BEFORE a panic
    // tears down the process (or thread, in this test), the saved
    // content is still readable on the way back up.
    let path = unique_draft_path();
    let path_for_thread = path.clone();
    let handle = std::thread::spawn(move || {
        persist_draft_to(&path_for_thread, "in-flight migration script").unwrap();
        panic!("simulated mid-edit crash after save");
    });
    let join = handle.join();
    assert!(join.is_err(), "thread should have panicked");

    let restored = load_draft_from(&path).expect("draft must be readable after panic");
    assert_eq!(restored, "in-flight migration script");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn empty_buffer_save_clears_persisted_draft() {
    // Quit with an empty buffer → next session should NOT see a
    // phantom draft. `load_draft_from` returns None for empty
    // files, matching the production behaviour.
    let path = unique_draft_path();
    persist_draft_to(&path, "something").unwrap();
    assert!(load_draft_from(&path).is_some());
    persist_draft_to(&path, "").unwrap();
    assert!(load_draft_from(&path).is_none());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn draft_load_returns_none_for_missing_file() {
    let path = unique_draft_path();
    let _ = std::fs::remove_file(&path); // ensure absent
    assert!(load_draft_from(&path).is_none());
}

// `util::write_atomic` is single-writer-atomic: it shares a single
// `<file>.tmp` sibling per target path, so concurrent writers can
// tear (write A's bytes to tmp, B opens-and-writes-the-same-tmp,
// rename races mid-write). pgman's production has exactly one
// writer (the App's main loop), so this is fine. We don't have a
// concurrent-writes-don't-tear test because the function doesn't
// promise that.
