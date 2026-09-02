//! End-to-end coverage for the two subprocess paths — `$EDITOR`
//! (`\e`) and `pg_format` (`\f`). The unit tests in `app.rs` cover
//! the wrapper logic (status messages, error surfacing) by mocking
//! at the boundary; these tests exercise the actual `Command::spawn`
//! path against shell stubs in `tests/bin/`.
//!
//! Stubs are committed to the repo as `tests/bin/fake_*` shell
//! scripts. They live under `tests/` rather than a build script so
//! they're inspectable and easy to update without touching Cargo.

use pgman::app::{external_edit_via, pg_format_via};

/// Absolute path to one of the `tests/bin/*` stub binaries.
fn stub(name: &str) -> String {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    format!("{manifest_dir}/tests/bin/{name}")
}

/// Serializes every test in this file that calls `external_edit_via`.
/// `external_edit_via`'s `$EDITOR` scratch directory is named
/// `pgman-edit-<pid>-<nanos>-<seq>` — unique per call — but the
/// leak-detection tests below count directories under that shared
/// `pgman-edit-<pid>-` prefix, which would race another thread's
/// transient directory if these tests ran in parallel with each
/// other (cargo's default test runner). Every `external_edit_via`
/// call in this file takes the lock first.
static EDIT_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_edit_tests() -> std::sync::MutexGuard<'static, ()> {
    EDIT_MUTEX.lock().unwrap_or_else(|e| e.into_inner())
}

/// Count `$TMPDIR` entries whose name starts with this process's
/// `$EDITOR` scratch-directory prefix — i.e. how many are currently
/// leaked. Comparing this before/after an `external_edit_via` call
/// (under `EDIT_MUTEX`) proves the scratch directory it created was
/// actually removed.
fn edit_scratch_dir_count() -> usize {
    let prefix = format!("pgman-edit-{}-", std::process::id());
    std::fs::read_dir(std::env::temp_dir())
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .count()
}

#[test]
fn pg_format_subprocess_writes_marker_and_passes_stdin_through() {
    let input = "select 1";
    let out = pg_format_via(input, &stub("fake_pg_format")).expect("fake_pg_format should succeed");
    // The stub prepends a marker line and echoes stdin. Trailing
    // newline is stripped by `pg_format_via` so the result reads
    // cleanly.
    assert_eq!(out, "-- FORMATTED BY FAKE PG_FORMAT\nselect 1");
}

#[test]
fn pg_format_missing_binary_surfaces_actionable_error() {
    let err = pg_format_via("SELECT 1", "definitely_not_a_real_binary_xyz")
        .expect_err("missing binary should error");
    // Lifted message points the operator at the install command.
    assert!(err.contains("not on PATH"), "got: {err}");
}

#[test]
fn external_edit_subprocess_prepends_marker_and_buffer_round_trips() {
    let _guard = lock_edit_tests();
    let buffer = "SELECT 1\nFROM t";
    let edited =
        external_edit_via(buffer, &stub("fake_editor")).expect("fake_editor should succeed");
    assert_eq!(edited, "-- edited by fake_editor\nSELECT 1\nFROM t");
}

#[test]
fn external_edit_nonzero_exit_surfaces_buffer_unchanged() {
    let _guard = lock_edit_tests();
    let err = external_edit_via("anything", &stub("fake_editor_failure"))
        .expect_err("exit 17 stub should error");
    assert!(err.contains("buffer unchanged"), "got: {err}");
    // Exit status format varies per platform; just spot-check the
    // code.
    assert!(err.contains("17"), "got: {err}");
}

#[test]
fn external_edit_with_args_splits_command_line() {
    // The stub doesn't care about extra args; the test just proves
    // that a command like `fake_editor --some-flag` parses and runs.
    let _guard = lock_edit_tests();
    let cmd = format!("{} --noop-flag", stub("fake_editor"));
    let edited = external_edit_via("hello", &cmd).expect("split-arg invocation should still run");
    assert!(edited.contains("hello"));
}

#[cfg(unix)]
#[test]
fn external_edit_scratch_file_and_dir_are_owner_only_while_open() {
    // The stub `stat`s the file it's editing and its parent dir
    // *while the editor has them open* and reports the octal modes
    // back through the buffer content — checking from out here after
    // the call returns would race the scratch-dir cleanup that runs
    // right after the subprocess exits.
    let _guard = lock_edit_tests();
    let edited = external_edit_via("SELECT 1", &stub("fake_editor_stat"))
        .expect("fake_editor_stat should succeed");
    assert!(
        edited.contains("file_mode=600"),
        "buffer file was not 0600 while open: {edited}"
    );
    assert!(
        edited.contains("dir_mode=700"),
        "scratch dir was not 0700 while the editor had it open: {edited}"
    );
}

#[test]
fn external_edit_scratch_dir_removed_on_success() {
    let _guard = lock_edit_tests();
    let before = edit_scratch_dir_count();
    external_edit_via("SELECT 1", &stub("fake_editor")).expect("fake_editor should succeed");
    assert_eq!(
        edit_scratch_dir_count(),
        before,
        "scratch dir leaked after a successful edit"
    );
}

#[test]
fn external_edit_scratch_dir_removed_on_nonzero_exit() {
    // Regression test: the old implementation `return`ed from the
    // read-back-failure branch before its `remove_file` call, and a
    // nonzero editor exit is the closest end-to-end path to that —
    // proving cleanup survives every early return, not just the
    // happy path.
    let _guard = lock_edit_tests();
    let before = edit_scratch_dir_count();
    let _ = external_edit_via("anything", &stub("fake_editor_failure"));
    assert_eq!(
        edit_scratch_dir_count(),
        before,
        "scratch dir leaked after editor failure"
    );
}

#[test]
fn external_edit_scratch_dir_removed_on_read_failure() {
    // The exact bug this hardening fixes: the old implementation
    // returned from the read-back-failure branch *before* its
    // `remove_file` call. The editor here exits 0 (success) but has
    // deleted the file out from under us, so `external_edit_via`
    // must hit the read-failure `Err` path while still cleaning up.
    let _guard = lock_edit_tests();
    let before = edit_scratch_dir_count();
    let err = external_edit_via("anything", &stub("fake_editor_delete_file"))
        .expect_err("editor deleting the file should surface a read failure");
    assert!(err.contains("could not read"), "got: {err}");
    assert_eq!(
        edit_scratch_dir_count(),
        before,
        "scratch dir leaked after a read-back failure"
    );
}

#[test]
fn external_edit_scratch_dir_removed_on_spawn_failure() {
    let _guard = lock_edit_tests();
    let before = edit_scratch_dir_count();
    let _ = external_edit_via("anything", "definitely_not_a_real_binary_xyz");
    assert_eq!(
        edit_scratch_dir_count(),
        before,
        "scratch dir leaked after a spawn failure"
    );
}

// --- the `pgman` binary itself, spawned end-to-end ------------------
//
// Unlike the stubs above, these spawn the real compiled binary
// (`CARGO_BIN_EXE_pgman`) to exercise the CLI surface rather than a
// library function.

use std::process::{Command, Stdio};

#[test]
fn non_tty_launch_prints_the_batch_hint_and_exits_2() {
    // No --batch / --upgrade / --init-config, so this reaches the
    // terminal probe. stdin is /dev/null and `.output()` always pipes
    // stdout — neither is a terminal — so the launch must refuse
    // before it ever touches the alternate screen (the old behaviour
    // was a raw `Error: Device not configured (os error 6)` from
    // inside crossterm).
    let out = Command::new(env!("CARGO_BIN_EXE_pgman"))
        .stdin(Stdio::null())
        .output()
        .expect("spawn pgman");
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("pgman needs a terminal") && stderr.contains("--batch"),
        "got: {stderr}"
    );
}
