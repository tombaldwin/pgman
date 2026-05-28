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
    let buffer = "SELECT 1\nFROM t";
    let edited =
        external_edit_via(buffer, &stub("fake_editor")).expect("fake_editor should succeed");
    assert_eq!(edited, "-- edited by fake_editor\nSELECT 1\nFROM t");
}

#[test]
fn external_edit_nonzero_exit_surfaces_buffer_unchanged() {
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
    let cmd = format!("{} --noop-flag", stub("fake_editor"));
    let edited = external_edit_via("hello", &cmd).expect("split-arg invocation should still run");
    assert!(edited.contains("hello"));
}
