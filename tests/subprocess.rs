//! End-to-end coverage for the two subprocess paths — `$EDITOR`
//! (`\e`) and `pg_format` (`\f`). The unit tests in `app.rs` cover
//! the wrapper logic (status messages, error surfacing) by mocking
//! at the boundary; these tests exercise the actual `Command::spawn`
//! path against shell stubs in `tests/bin/`.
//!
//! Stubs are committed to the repo as `tests/bin/fake_*` shell
//! scripts. They live under `tests/` rather than a build script so
//! they're inspectable and easy to update without touching Cargo.

use pgman::app::{external_edit_via, find_on_path_in, pg_format_via, App};

/// Absolute path to one of the `tests/bin/*` stub binaries.
fn stub(name: &str) -> String {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    format!("{manifest_dir}/tests/bin/{name}")
}

/// The `tests/bin/` directory itself, as a one-entry `PATH`.
fn stub_dir() -> std::ffi::OsString {
    format!("{}/tests/bin", env!("CARGO_MANIFEST_DIR")).into()
}

fn editor_app(buffer: &str) -> App {
    let mut a = App::new(
        pgman::theme::Theme::default(),
        None,
        Vec::new(),
        pgman::safety::SafetyConfig::default(),
    );
    a.editor.buffer = buffer.to_string();
    a.editor.cursor = a.editor.buffer.len();
    a
}

/// Ctrl-F with a `pg_format` on the given PATH: the real subprocess
/// runs (the stub is a shell script that has to be spawned), and the
/// status names it. The stub is copied to a throwaway directory under
/// the name the lookup wants, `pg_format`.
#[test]
fn reformat_uses_pg_format_when_it_is_on_path() {
    let dir = std::env::temp_dir().join(format!("pgman-pgfmt-sub-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(stub("fake_pg_format"), dir.join("pg_format")).unwrap();
    let mut a = editor_app("select 1");
    a.reformat_buffer_with_path(Some(dir.as_os_str()));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(a.editor.buffer, "-- FORMATTED BY FAKE PG_FORMAT\nselect 1");
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(
        status.starts_with("formatted via pg_format"),
        "got: {status:?}"
    );
}

/// Same buffer, `pg_format` not on PATH: the built-in formatter runs
/// and the status says so. No error — the operator is not told to
/// install anything.
#[test]
fn reformat_falls_back_to_built_in_when_pg_format_is_absent() {
    let mut a = editor_app("select 1");
    let empty: std::ffi::OsString = std::env::temp_dir().into();
    a.reformat_buffer_with_path(Some(&empty));
    assert_eq!(a.editor.buffer, "SELECT\n  1");
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(
        status.starts_with("formatted (built-in)"),
        "got: {status:?}"
    );
    assert!(a.last_error.is_none(), "last_error = {:?}", a.last_error);
    // And with no PATH at all.
    let mut a = editor_app("select 1");
    a.reformat_buffer_with_path(None);
    assert_eq!(a.editor.buffer, "SELECT\n  1");
}

#[test]
fn find_on_path_in_resolves_the_stub_only_where_it_lives() {
    let found = find_on_path_in("fake_pg_format", &stub_dir()).expect("stub on PATH");
    assert_eq!(found.to_string_lossy(), stub("fake_pg_format"));
    let empty: std::ffi::OsString = std::env::temp_dir().into();
    assert!(find_on_path_in("fake_pg_format", &empty).is_none());
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

#[test]
fn help_output_fits_a_screen_and_names_no_rust_types() {
    let out = Command::new(env!("CARGO_BIN_EXE_pgman"))
        .arg("--help")
        .output()
        .expect("spawn pgman");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("::"),
        "--help must name no Rust types: {stdout}"
    );
    // Grouped headings from `help_heading`.
    assert!(stdout.contains("Batch mode"), "got: {stdout}");
    assert!(stdout.contains("JDBC tap"), "got: {stdout}");
}

#[test]
fn positional_dsn_and_dsn_flag_must_agree() {
    let out = Command::new(env!("CARGO_BIN_EXE_pgman"))
        .args([
            "--batch",
            "--dsn",
            "postgres://a@host/db",
            "postgres://b@host/db",
            "--sql",
            "SELECT 1",
        ])
        .output()
        .expect("spawn pgman");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("disagree"), "got: {stderr}");
}

#[test]
fn init_config_writes_a_default_and_refuses_to_overwrite() {
    let home = std::env::temp_dir().join(format!("pgman-init-config-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();

    let run = || {
        Command::new(env!("CARGO_BIN_EXE_pgman"))
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .arg("--init-config")
            .output()
            .expect("spawn pgman")
    };

    let first = run();
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let path = home.join(".config/pgman/safety.toml");
    assert!(path.exists(), "safety.toml should have been written");
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(contents.contains("[default]"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "safety.toml mode was {mode:o}, want 0600");
    }

    let second = run();
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        !second.status.success(),
        "a second --init-config must refuse to overwrite"
    );
    assert_eq!(second.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(stderr.contains("already exists"), "got: {stderr}");
}

#[test]
fn log_over_64mb_is_refused_before_being_read() {
    let dir = std::env::temp_dir().join(format!("pgman-log-cap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("huge.log");
    {
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(65 * 1024 * 1024).unwrap();
    }
    let out = Command::new(env!("CARGO_BIN_EXE_pgman"))
        .args(["--log", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("spawn pgman");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("is 65 MB"), "got: {stderr}");
    assert!(stderr.contains("64 MB"), "got: {stderr}");
}
