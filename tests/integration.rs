//! Integration tests against a real Postgres. Gated behind
//! `--features integration` so plain `cargo test` doesn't try to
//! reach a server that isn't there. Bring the DB up with:
//!
//!   docker compose -f docker-compose.test.yml up -d
//!   cargo test --features integration
//!   docker compose -f docker-compose.test.yml down
//!
//! The DSN is hard-wired to the host:port in the compose file. The
//! tests cover the batch mode end-to-end (exit code + stdout shape)
//! since that's the path real users can't exercise without Postgres.

#![cfg(feature = "integration")]

use std::process::Command;

/// DSN of the docker-compose postgres. Matches docker-compose.test.yml.
const DSN: &str = "postgres://pgman_test:pgman_test@127.0.0.1:55432/pgman_test?sslmode=disable";

/// Path to the freshly-built `pgman` binary. Cargo sets this env var
/// when compiling integration tests so we don't have to guess the
/// path or shell out to `cargo run`.
fn pgman_binary() -> &'static str {
    env!("CARGO_BIN_EXE_pgman")
}

#[test]
fn batch_select_1_emits_csv_and_exits_zero() {
    let out = Command::new(pgman_binary())
        .args([
            "--batch",
            "--dsn",
            DSN,
            "--sql",
            "SELECT 1 AS one",
            "--format",
            "csv",
        ])
        .output()
        .expect("spawn pgman");
    assert!(
        out.status.success(),
        "exit {:?}, stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("one"), "header should appear: {stdout}");
    assert!(stdout.contains("1"), "value should appear: {stdout}");
}

#[test]
fn batch_json_shape() {
    let out = Command::new(pgman_binary())
        .args([
            "--batch",
            "--dsn",
            DSN,
            "--sql",
            "SELECT 1 AS id, 'alice' AS name",
            "--format",
            "json",
        ])
        .output()
        .expect("spawn pgman");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // `id` is an int4 column — a JSON number, not a quoted string.
    assert!(
        stdout.contains(r#""id":1"#) && stdout.contains(r#""name":"alice""#),
        "expected JSON shape; got {stdout}"
    );
}

#[test]
fn batch_json_types_null_bool_numbers_and_text() {
    // The exact reproduction from the JSON-typing fix: NULL must not
    // collapse into the same `""` an empty string gets, and numbers /
    // booleans must come back as JSON numbers/booleans rather than
    // stringified text.
    let out = Command::new(pgman_binary())
        .args([
            "--batch",
            "--dsn",
            DSN,
            "--sql",
            "select null::text as a, '' as b, 42 as c, true as d, 1.5 as e",
            "--format",
            "json",
        ])
        .output()
        .expect("spawn pgman");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.trim_end(),
        r#"[{"a":null,"b":"","c":42,"d":true,"e":1.5}]"#
    );
}

#[test]
fn batch_multistatement_routes_through_simple_query_protocol() {
    // A multi-statement input that `client.prepare()` rejects but
    // `batch_execute()` accepts. Exit success means the splitter
    // detected the multi-stmt shape correctly.
    //
    // `--yes` is required and is not incidental to the test. The safety
    // gate classifies a bare `BEGIN` as `Other` — it cannot tell what a
    // transaction will go on to do — so batch mode refuses it without
    // explicit confirmation. That refusal is the gate working. This test
    // is about statement splitting, so it confirms and moves on.
    //
    // The writable profile, because under a read-only one transaction
    // control in a script is refused outright, `--yes` or not (see
    // `batch_refuses_to_turn_the_read_only_session_writable`).
    let home = scratch_home("multistmt", WRITABLE_PROFILE);
    let out = batch_with_home(&home, &["--yes", "--sql", "BEGIN; SELECT 1; COMMIT"]);
    assert!(
        out.status.success(),
        "multi-statement batch should succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn batch_query_failure_exits_one_and_writes_stderr() {
    let out = Command::new(pgman_binary())
        .args([
            "--batch",
            "--dsn",
            DSN,
            "--sql",
            "SELECT * FROM nonexistent_table_for_pgman_tests",
        ])
        .output()
        .expect("spawn pgman");
    // batch::run returns Ok(1) for query failure → exit code 1.
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("nonexistent_table_for_pgman_tests"),
        "stderr should name the offending relation: {stderr}"
    );
}

#[test]
fn batch_surfaces_server_notice_on_stderr() {
    // DO block with RAISE NOTICE — the connection driver's poll_message
    // loop should pick it up and route through the notice channel,
    // which the batch path drains to stderr.
    // `--yes` for the same reason as the multi-statement test above: a
    // `DO $$ .. $$` block is arbitrary PL/pgSQL, so the gate classifies
    // it `Other` and asks. This test is about notice routing. The
    // writable profile, because under a read-only one `DO` is refused
    // outright — its body is opaque to the classifier.
    let home = scratch_home("notice", WRITABLE_PROFILE);
    let out = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            "DO $$ BEGIN RAISE NOTICE 'pgman-test-notice'; END $$",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("pgman-test-notice"),
        "notice should land on stderr; got: {stderr:?}"
    );
    // Status flag carries the severity, surfaced via the `[SEVERITY] message` shape.
    assert!(
        stderr.contains("NOTICE"),
        "severity tag should appear; got: {stderr:?}"
    );
}

#[test]
fn batch_filters_control_characters_out_of_server_supplied_text() {
    // A `RAISE NOTICE` (or an error, or a cell in `--format expanded`)
    // carrying `ESC ] 0 ; pwned BEL` used to reach the terminal raw — an
    // OSC title-set, and anything else an escape sequence can do. `chr()`
    // builds the bytes server-side so this file carries none itself.
    let home = scratch_home("ctl-chars", WRITABLE_PROFILE);
    let cases: &[(&[&str], bool)] = &[
        (
            &[
                "--yes",
                "--sql",
                "DO $$ BEGIN RAISE NOTICE 'pgman-ctl-notice %', chr(27) || ']0;pwned' || chr(7); END $$",
            ],
            true,
        ),
        (
            &[
                "--yes",
                "--sql",
                "DO $$ BEGIN RAISE EXCEPTION 'pgman-ctl-error %', chr(27) || '[31m' || chr(7); END $$",
            ],
            false,
        ),
        (
            &[
                "--format",
                "expanded",
                "--sql",
                "SELECT chr(27) || ']0;pwned' || chr(7) AS \"pgman-ctl-cell\"",
            ],
            true,
        ),
    ];
    for (args, expect_success) in cases {
        let out = batch_with_home(&home, args);
        assert_eq!(
            out.status.success(),
            *expect_success,
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let all = [out.stdout.as_slice(), out.stderr.as_slice()].concat();
        let text = String::from_utf8_lossy(&all);
        assert!(
            text.contains("pgman-ctl"),
            "{args:?}: the text itself must still print: {text:?}"
        );
        assert!(
            !text.contains('\x1b') && !text.contains('\x07'),
            "{args:?}: control characters reached the terminal: {text:?}"
        );
    }
}

#[tokio::test]
async fn cancel_token_aborts_pg_sleep() {
    // End-to-end: run a long pg_sleep, send a real CancelRequest,
    // verify the query returns with a cancellation error before the
    // sleep completes. This is what Ctrl-C actually does — the
    // unit tests cover the routing; this one covers the wire.
    use pgman::conn::{connect_only, NoticeMsg, NotificationMsg};
    use std::time::{Duration, Instant};

    let dsn = pgman::conn::Dsn::parse(DSN).expect("parse DSN");
    let (notice_tx, _notice_rx) = tokio::sync::mpsc::unbounded_channel::<NoticeMsg>();
    let (notification_tx, _notification_rx) =
        tokio::sync::mpsc::unbounded_channel::<NotificationMsg>();
    let (client, _tunnel) = connect_only(dsn, false, 0, notice_tx, notification_tx)
        .await
        .expect("connect");

    let cancel = client.cancel_token();
    let started = Instant::now();
    // Schedule a cancel 200ms in; the sleep is for 10s so it can
    // only finish via the cancel.
    let cancel_handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = cancel.cancel_query(tokio_postgres::NoTls).await;
    });

    let res = pgman::conn::run_statement(&client, "SELECT pg_sleep(10)").await;
    let _ = cancel_handle.await;
    let elapsed = started.elapsed();

    assert!(
        res.is_err(),
        "pg_sleep should have been cancelled, but returned Ok"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "should have cancelled in well under 10s; took {elapsed:?}"
    );
    let err = res.unwrap_err().msg;
    assert!(
        err.to_lowercase().contains("cancel")
            || err.to_lowercase().contains("statement terminated"),
        "expected cancellation message; got: {err}"
    );
}

#[tokio::test]
async fn every_connection_identifies_itself_as_pgman_in_pg_stat_activity() {
    // `pg_stat_activity.application_name` is how a DBA tells pgman's
    // backend from the application pool's — and what the sessions
    // panel's `user/app` column shows for another pgman.
    use pgman::conn::{connect_only, NoticeMsg, NotificationMsg};

    let dsn = pgman::conn::Dsn::parse(DSN).expect("parse DSN");
    let (notice_tx, _notice_rx) = tokio::sync::mpsc::unbounded_channel::<NoticeMsg>();
    let (notification_tx, _notification_rx) =
        tokio::sync::mpsc::unbounded_channel::<NotificationMsg>();
    let (client, _tunnel) = connect_only(dsn, true, 0, notice_tx, notification_tx)
        .await
        .expect("connect");
    let grid = pgman::conn::run_statement(
        &client,
        "SELECT application_name FROM pg_stat_activity WHERE pid = pg_backend_pid()",
    )
    .await
    .expect("query pg_stat_activity");
    assert_eq!(
        grid.rows
            .first()
            .and_then(|r| r.first())
            .map(String::as_str),
        Some(format!("pgman/{}", env!("CARGO_PKG_VERSION")).as_str()),
        "got: {:?}",
        grid.rows
    );
}

#[test]
fn batch_expanded_format_renders_one_record_per_block() {
    let out = Command::new(pgman_binary())
        .args([
            "--batch",
            "--dsn",
            DSN,
            "--sql",
            "SELECT 1 AS id, 'one' AS name UNION ALL SELECT 2, 'two' ORDER BY id",
            "--format",
            "expanded",
        ])
        .output()
        .expect("spawn pgman");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("RECORD 1"));
    assert!(stdout.contains("RECORD 2"));
    assert!(stdout.contains("id   | 1"));
    assert!(stdout.contains("name | one"));
}

/// The other half of the two `--yes` tests above, and the reason they
/// each carry a comment rather than a quietly-added flag.
///
/// Those two spent two months failing because the safety gate learned to
/// refuse `Other`-classified statements in batch mode after they were
/// written. Adding `--yes` fixes them, but it also means neither of them
/// exercises the refusal any more — and until now that refusal was
/// covered only by unit tests of `check_batch_safety`, never end-to-end
/// through the actual binary.
///
/// That gap is worth closing rather than assuming: a CLI path that skips
/// a safety gate its unit tests pass is a real and well-precedented bug.
/// So this pins that the *binary*, not just the function, refuses.
#[test]
fn batch_refuses_a_guarded_statement_without_yes() {
    // The writable profile: this test is about `Confirm` without `--yes`,
    // and under a read-only profile `DO` never gets as far as the
    // category guard (it is refused as opaque procedural code).
    let home = scratch_home("guarded-no-yes", WRITABLE_PROFILE);
    let out = batch_with_home(
        &home,
        &[
            "--sql",
            "DO $$ BEGIN RAISE NOTICE 'pgman-test-notice'; END $$",
        ],
    );

    assert!(
        !out.status.success(),
        "batch mode must REFUSE a guarded statement when --yes is absent, \
         but it exited 0"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("blocked by safety"),
        "the refusal must say why it refused: {stderr}"
    );
    assert!(
        stderr.contains("--yes"),
        "the refusal must tell the operator how to proceed: {stderr}"
    );
}

// --- the safety gate, end to end against a real server ------------------
//
// These are the security-review reproductions. They were reported as
// "`pgman --batch` with every guard at its default and only
// `read_only = false` still ran a `DROP TABLE`", so they are only worth
// anything if they run against a real Postgres with a real safety profile.
//
// pgman reads `$HOME/.config/pgman/safety.toml` (`util::config_dir`), so each
// test builds a throwaway HOME and writes its own profile there. The
// developer's real config is never read and never touched.

/// A scratch `HOME` containing `.config/pgman/safety.toml` with `body`.
/// Returned path is handed to the child process as `HOME`.
fn scratch_home(name: &str, body: &str) -> std::path::PathBuf {
    let home = scratch_home_without_profile(name);
    std::fs::write(home.join(".config/pgman/safety.toml"), body).expect("write safety.toml");
    home
}

/// A scratch `$HOME` with the config dir but NO `safety.toml` — the
/// built-in default profile, which is what a fresh install runs under.
fn scratch_home_without_profile(name: &str) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!("pgman-it-{name}-{}", std::process::id()));
    let cfg = home.join(".config/pgman");
    std::fs::create_dir_all(&cfg).expect("create scratch config dir");
    let _ = std::fs::remove_file(cfg.join("safety.toml"));
    home
}

/// Run `pgman --batch` with `HOME` pointed at a scratch profile.
fn batch_with_home(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(pgman_binary())
        .env("HOME", home)
        // The GitHub runner exports XDG_CONFIG_HOME, and pgman honours
        // it over $HOME — so point all three XDG dirs at the scratch
        // home too, or the profile under it is never read.
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .args(["--batch", "--dsn", DSN])
        .args(args)
        .output()
        .expect("spawn pgman")
}

/// `read_only = false`, everything else default — the exact profile the
/// security report used. `drop` is still `block` by default.
const WRITABLE_PROFILE: &str = "[default]\nread_only = false\n";

/// `true` if `table` is still there. Uses a separate, plainly-safe query.
fn table_exists(home: &std::path::Path, table: &str) -> bool {
    let out = batch_with_home(
        home,
        &[
            "--sql",
            &format!("SELECT to_regclass('{table}') IS NOT NULL AS present"),
            "--format",
            "csv",
        ],
    );
    assert!(
        out.status.success(),
        "existence probe failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The value row, not the whole output: the header `present` has a
    // `t` in it, and `contains('t')` made this probe answer "yes" to
    // everything — every "still exists after the refused DROP" assertion
    // built on it was unfalsifiable.
    // Booleans arrive over the text protocol as psql prints them: `t`
    // / `f` (they were `true` / `false` while the grid decoded them
    // client-side).
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value = stdout.lines().nth(1).unwrap_or("").trim();
    assert!(
        matches!(value, "t" | "f"),
        "existence probe must answer t or f, got {stdout:?}"
    );
    value == "t"
}

#[test]
fn batch_refuses_a_drop_hidden_behind_a_dollar_in_an_identifier() {
    let home = scratch_home("dollar", WRITABLE_PROFILE);
    // `a$b$c` is one identifier to Postgres. The splitter used to read `$b$`
    // as opening a dollar-quote, swallow the rest of the script into a
    // fragment that classified as SELECT, and hand the ORIGINAL string to
    // batch_execute — so all three statements ran under an `Allow`.
    let setup = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            "CREATE TABLE IF NOT EXISTS pgman_repro_dollar (id int)",
        ],
    );
    assert!(
        setup.status.success(),
        "setup failed: {}",
        String::from_utf8_lossy(&setup.stderr)
    );

    let out = batch_with_home(
        &home,
        &[
            "--sql",
            "SELECT 1; SELECT 1 AS a$b$c; DROP TABLE pgman_repro_dollar",
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "the DROP must be refused; exited 0 with stdout {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr.contains("blocked by safety") && stderr.contains("DROP"),
        "the refusal must name the DROP: {stderr}"
    );
    assert!(
        table_exists(&home, "pgman_repro_dollar"),
        "the table must still be there"
    );

    let _ = batch_with_home(&home, &["--yes", "--sql", "TRUNCATE pgman_repro_dollar"]);
}

#[test]
fn batch_refuses_a_drop_hidden_behind_a_quoted_identifier() {
    let home = scratch_home("quoted", WRITABLE_PROFILE);
    // `"a--b"` is one identifier; the `--` inside it is part of the name. The
    // comment stripper used to eat it as a line comment along with the rest
    // of the line, and the DROP came back attached to a SELECT fragment.
    //
    // Both tables are created so that, had the guard missed it, the script
    // would have run clean and the DROP would genuinely have landed — the
    // test must not pass merely because Postgres errored first.
    let setup = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            r#"CREATE TABLE IF NOT EXISTS "a--b" (id int); CREATE TABLE IF NOT EXISTS pgman_repro_quoted (id int)"#,
        ],
    );
    assert!(
        setup.status.success(),
        "setup failed: {}",
        String::from_utf8_lossy(&setup.stderr)
    );

    let out = batch_with_home(
        &home,
        &[
            "--sql",
            r#"SELECT 1; SELECT * FROM "a--b"; DROP TABLE pgman_repro_quoted"#,
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "the DROP must be refused; exited 0 with stdout {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr.contains("blocked by safety") && stderr.contains("DROP"),
        "the refusal must name the DROP: {stderr}"
    );
    assert!(
        table_exists(&home, "pgman_repro_quoted"),
        "the table must still be there"
    );
}

/// The non-ASCII reproduction, live. `name` is the identifier byte sequence
/// under test; `table` is a scratch table the hidden `DROP` targets.
///
/// `é$b$c` is one identifier to Postgres — every byte from 0x80 up continues
/// an identifier — so the `$b$` in it opens nothing. pgman's lexer stopped the
/// identifier at the `é`, read `$b$…$b$` as a dollar-quoted body, swallowed the
/// `DROP` into it, classified the fragment `Select` -> `Allow`, and handed the
/// ORIGINAL script to `batch_execute`. Reproduced against this server: exit 0,
/// "batch executed", and the table gone, with `drop` still at its `block`
/// default.
fn assert_a_drop_hidden_behind_a_non_ascii_identifier_is_refused(
    slug: &str,
    name: &str,
    table: &str,
) {
    let home = scratch_home(slug, WRITABLE_PROFILE);
    let setup = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            &format!("CREATE TABLE IF NOT EXISTS {table} (id int)"),
        ],
    );
    assert!(
        setup.status.success(),
        "setup failed: {}",
        String::from_utf8_lossy(&setup.stderr)
    );

    // The trailing `SELECT 1 AS x$b$y` closes the tag the old lexer thought it
    // had opened, so the script would have run clean: the test cannot pass
    // merely because Postgres errored first.
    let sql = format!("SELECT 1; SELECT 1 AS {name}$b$c; DROP TABLE {table}; SELECT 1 AS x$b$y");
    let out = batch_with_home(&home, &["--sql", &sql]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "the DROP must be refused; exited 0 with stdout {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr.contains("blocked by safety") && stderr.contains("DROP"),
        "the refusal must name the DROP: {stderr}"
    );
    assert!(table_exists(&home, table), "the table must still be there");

    let _ = batch_with_home(
        &home,
        &["--yes", "--sql", &format!("DROP TABLE IF EXISTS {table}")],
    );
}

#[test]
fn batch_refuses_a_drop_hidden_behind_a_two_byte_identifier_character() {
    assert_a_drop_hidden_behind_a_non_ascii_identifier_is_refused(
        "nonascii2",
        "é",
        "pgman_repro_nonascii_two",
    );
}

#[test]
fn batch_refuses_a_drop_hidden_behind_a_three_byte_identifier_character() {
    assert_a_drop_hidden_behind_a_non_ascii_identifier_is_refused(
        "nonascii3",
        "中",
        "pgman_repro_nonascii_three",
    );
}

#[test]
fn batch_refuses_a_script_it_cannot_split_and_never_connects() {
    // An unterminated literal means the statement boundaries are a guess.
    // pgman refuses rather than guessing, and --yes does not buy a way past.
    let home = scratch_home("unsplittable", WRITABLE_PROFILE);
    let out = batch_with_home(&home, &["--yes", "--sql", "SELECT 1; SELECT 'oops"]);
    assert!(!out.status.success(), "an unverifiable script must not run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("could not split this script safely"),
        "the refusal must say why: {stderr}"
    );
}

#[test]
fn batch_refuses_to_turn_the_read_only_session_writable() {
    // `read_only = true` is applied server-side, but the setting itself is a
    // plain session GUC — Postgres happily lets a script turn it off. The
    // client is the only thing standing in the way, so it has to be.
    let home = scratch_home("readonly", "[default]\nread_only = true\n");
    for sql in [
        "SET default_transaction_read_only = off",
        "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE",
        "SET default_transaction_read_only = off; SELECT 1",
        // `SET` spelled as a function call — classified a plain read once,
        // and run with no prompt.
        "SELECT set_config('default_transaction_read_only', 'off', false)",
        "SELECT set_config('transaction_read_only', 'off', true)",
        "SELECT pg_catalog.set_config('DEFAULT_TRANSACTION_READ_ONLY', 'off', false)",
        // A quoted GUC name is still the GUC.
        "SET \"transaction_read_only\" = off",
        "SET \"default_transaction_read_only\" TO off",
        // Procedural code is opaque: the `SET` inside is a dollar-quoted
        // string to the classifier.
        "DO $$ BEGIN SET default_transaction_read_only = off; END $$",
        "CALL pgman_it_no_such_procedure()",
        // Ending the transaction is what lets a lifted setting bite: the
        // next transaction begins under it. This one created its table.
        "SELECT set_config('default_transaction_read_only', 'off', false); \
         COMMIT; CREATE TABLE pgman_it_ro_escape(x int)",
        "SELECT 1; COMMIT; CREATE TABLE pgman_it_ro_escape(x int)",
        "SELECT 1; END; CREATE TABLE pgman_it_ro_escape(x int)",
        "BEGIN; SELECT 1",
        "SELECT 1; ROLLBACK",
        "START TRANSACTION; SELECT 1",
    ] {
        let out = batch_with_home(&home, &["--yes", "--sql", sql]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "{sql:?} must be refused even with --yes"
        );
        assert!(
            stderr.contains("read-only by safety.toml"),
            "{sql:?} must be refused for the right reason: {stderr}"
        );
    }
    // The floor is intact afterwards, and nothing landed.
    let out = batch_with_home(
        &home,
        &[
            "--sql",
            "SHOW default_transaction_read_only",
            "--format",
            "csv",
        ],
    );
    assert!(
        out.status.success(),
        "SHOW failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "default_transaction_read_only\non",
        "the session must still be read-only"
    );
    assert!(
        !table_exists(&home, "pgman_it_ro_escape"),
        "a table was created on a read-only profile"
    );
}

#[test]
fn batch_connect_failure_carries_the_same_hint_the_tui_shows() {
    // `--dsn` with a deliberately wrong password. The docker-compose
    // Postgres sets POSTGRES_PASSWORD, so host connections require
    // scram/md5 auth — a wrong password genuinely fails rather than
    // silently succeeding.
    let wrong =
        "postgres://pgman_test:definitely-the-wrong-password@127.0.0.1:55432/pgman_test?sslmode=disable";
    let out = Command::new(pgman_binary())
        .args(["--batch", "--dsn", wrong, "--sql", "SELECT 1"])
        .output()
        .expect("spawn pgman");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("connect failed:"), "got: {stderr}");
    assert!(
        stderr.contains("hint: wrong password"),
        "the batch path should carry the same hint conn::connect_hint gives the TUI: {stderr}"
    );
}

#[test]
fn batch_read_only_transaction_refusal_carries_the_configuration_hint() {
    // Distinct from `batch_refuses_to_turn_the_read_only_session_writable`
    // above: there, pgman's own client-side guard refuses a GUC-escape
    // attempt before anything reaches the server. Here the guard rails
    // let the statement through (`ddl` defaults to `confirm`, `--yes`
    // clears that) and it's Postgres itself that refuses — SQLSTATE
    // 25006 — because the session is `default_transaction_read_only =
    // on`. That refusal should carry the same "where does this come
    // from" hint.
    let home = scratch_home("readonly-server-refusal", "[default]\nread_only = true\n");
    let out = batch_with_home(
        &home,
        &["--yes", "--sql", "CREATE TABLE pgman_ro_hint_test (id int)"],
    );
    assert!(!out.status.success(), "the write must be refused");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.to_lowercase().contains("read-only transaction"),
        "the server's own message should show: {stderr}"
    );
    // The profile came from a file, so the hint names that file — the
    // scratch one, not whatever the developer has under their own $HOME.
    let path = home.join(".config/pgman/safety.toml");
    assert!(
        stderr.contains("hint:")
            && stderr.contains(&path.display().to_string())
            && stderr.contains("read_only")
            && stderr.contains("docs/configuration.md"),
        "got: {stderr}"
    );
    assert!(
        !stderr.contains("--init-config"),
        "with a file on disk the hint must not tell the operator to write one: {stderr}"
    );
}

#[test]
fn batch_read_only_transaction_refusal_without_a_profile_says_how_to_get_one() {
    // Same server-side 25006 refusal, but under the built-in default
    // profile: there is no safety.toml, so naming one (with a full path)
    // sent the operator looking for a file that was not there. The hint
    // has to say read-only is the default and how to write the file.
    let home = scratch_home_without_profile("readonly-server-refusal-default");
    let out = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            "CREATE TABLE pgman_ro_hint_default (id int)",
        ],
    );
    assert!(!out.status.success(), "the write must be refused");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.to_lowercase().contains("read-only transaction"),
        "the server's own message should show: {stderr}"
    );
    assert!(
        stderr.contains("hint: read-only by default · pgman --init-config writes safety.toml; set read_only = false for this database"),
        "got: {stderr}"
    );
    let path = home.join(".config/pgman/safety.toml");
    assert!(
        !stderr.contains(&path.display().to_string()),
        "must not name a file that does not exist: {stderr}"
    );
}

/// A statement ending in a `-- line comment` used to swallow the `;` the
/// re-join inserted, so `UPDATE … WHERE id=1 --` followed by `; OR true`
/// reached the server as `WHERE id=1 OR true` — past
/// `update_without_where = block`. Live: the rows must not all change.
#[test]
fn batch_cannot_widen_a_where_clause_through_a_trailing_comment() {
    let home = scratch_home("trailing-comment", WRITABLE_PROFILE);
    let setup = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            "CREATE TABLE IF NOT EXISTS pgman_repro_tc (id int, v text); \
             DELETE FROM pgman_repro_tc WHERE true; \
             INSERT INTO pgman_repro_tc VALUES (1,'a'),(2,'b'),(3,'c')",
        ],
    );
    assert!(
        setup.status.success(),
        "setup failed: {}",
        String::from_utf8_lossy(&setup.stderr)
    );

    let out = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            "UPDATE pgman_repro_tc SET v='PWNED' WHERE id=1 --\n; OR true",
        ],
    );
    // Either refused outright, or the second fragment (`OR true`) is a
    // syntax error that rolls the implicit transaction back. Never three
    // PWNED rows.
    let probe = batch_with_home(
        &home,
        &[
            "--sql",
            "SELECT count(*) FROM pgman_repro_tc WHERE v = 'PWNED'",
            "--format",
            "csv",
        ],
    );
    let pwned = String::from_utf8_lossy(&probe.stdout);
    assert!(
        !pwned.contains('3'),
        "the WHERE clause was widened: {pwned} (run stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The legitimate shape of the same script must still work: a statement
/// that ends in a comment, then another statement.
#[test]
fn batch_runs_a_script_whose_statement_ends_in_a_line_comment() {
    let home = scratch_home("trailing-comment-ok", WRITABLE_PROFILE);
    let setup = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            "CREATE TABLE IF NOT EXISTS pgman_repro_tc2 (id int); \
             DELETE FROM pgman_repro_tc2 WHERE true; \
             INSERT INTO pgman_repro_tc2 VALUES (1),(2)",
        ],
    );
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let out = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            "DELETE FROM pgman_repro_tc2 WHERE id=1 --x\n; INSERT INTO pgman_repro_tc2 VALUES (3)",
        ],
    );
    assert!(
        out.status.success(),
        "a trailing comment must not break a script: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let probe = batch_with_home(
        &home,
        &[
            "--sql",
            "SELECT string_agg(id::text, ',' ORDER BY id) FROM pgman_repro_tc2",
            "--format",
            "csv",
        ],
    );
    assert!(
        String::from_utf8_lossy(&probe.stdout).contains("2,3"),
        "expected rows 2,3: {}",
        String::from_utf8_lossy(&probe.stdout)
    );
}

/// The result-set path (the TUI grid and `--batch csv/tsv/expanded`
/// share `conn::run_statement`) renders every common column type the
/// way psql prints it. It used to decode only bool / int / float /
/// text over the binary protocol and print `?` for everything else —
/// timestamps, dates, numeric, uuid, json, arrays, intervals, bytea,
/// money, inet — in the grid and in the CSV alike.
#[test]
fn batch_csv_renders_every_common_column_type_as_psql_prints_it() {
    let out = Command::new(pgman_binary())
        .args([
            "--batch",
            "--dsn",
            DSN,
            "--format",
            "csv",
            "--sql",
            "SELECT '2024-05-06 07:08:09+00'::timestamptz AT TIME ZONE 'UTC' AS ts_utc, \
             '2024-05-06 07:08:09'::timestamp AS ts, \
             '2024-05-06'::date AS d, \
             '12:34:56'::time AS t, \
             1.50::numeric AS n, \
             '6ba7b810-9dad-11d1-80b4-00c04fd430c8'::uuid AS u, \
             '{\"a\":1}'::json AS j, \
             '{\"b\": 2}'::jsonb AS jb, \
             ARRAY[1,2]::int[] AS arr, \
             interval '1 day 02:00:00' AS iv, \
             '\\xdead'::bytea AS b, \
             12.34::money AS m, \
             '10.0.0.1'::inet AS ip, \
             42 AS i, 'x' AS s, true AS ok, NULL::text AS nul",
        ])
        .output()
        .expect("spawn pgman");
    assert!(
        out.status.success(),
        "exit {:?}, stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines = stdout.lines();
    let header = csv_fields(lines.next().expect("header row"));
    let cells = csv_fields(lines.next().expect("data row"));
    assert_eq!(
        cells.len(),
        header.len(),
        "row and header disagree on width: {stdout}"
    );
    assert!(
        !cells.iter().any(|c| c == "?"),
        "a cell rendered as `?` (undecodable type): {stdout}"
    );
    let expected = [
        "2024-05-06 07:08:09",
        "2024-05-06 07:08:09",
        "2024-05-06",
        "12:34:56",
        "1.50",
        "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
        // json keeps its input spacing; jsonb normalises.
        "{\"a\":1}",
        "{\"b\": 2}",
        "{1,2}",
        "1 day 02:00:00",
        "\\xdead",
        "$12.34",
        "10.0.0.1",
        "42",
        "x",
        "t",
        "",
    ];
    for (i, want) in expected.iter().enumerate() {
        assert_eq!(cells[i], *want, "column {i} ({}): {stdout}", header[i]);
    }
}

/// Split one CSV line the way `--format csv` writes it: `,`-separated,
/// a field holding `,` / `"` / newline wrapped in `"` with inner `"`
/// doubled. Test-side only; enough for one row of known shape.
fn csv_fields(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                cur.push('"');
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// The no-result-set branch is untouched by the text-protocol routing:
/// DDL and an `INSERT` without `RETURNING` still report the affected
/// count through the `status` column.
#[test]
fn batch_csv_non_row_statement_still_reports_the_affected_count() {
    let home = scratch_home("no-result-set", WRITABLE_PROFILE);
    let out = batch_with_home(
        &home,
        &[
            "--yes",
            "--format",
            "csv",
            "--sql",
            "CREATE TEMP TABLE pgman_types_probe (id int)",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("status\n0 row(s) affected"),
        "expected the status grid: {stdout}"
    );
}

/// Connect through the library (not the binary), read-only, with the
/// notice/notification channels left to drain into nothing.
async fn lib_client() -> (
    std::sync::Arc<tokio_postgres::Client>,
    Option<pgman::tunnel::SshTunnel>,
) {
    let (notice_tx, _notice_rx) = tokio::sync::mpsc::unbounded_channel();
    let (notification_tx, _notification_rx) = tokio::sync::mpsc::unbounded_channel();
    let dsn = pgman::conn::Dsn::parse(DSN).expect("test DSN parses");
    pgman::conn::connect_only(dsn, true, 30_000, notice_tx, notification_tx)
        .await
        .expect("connect to the compose postgres")
}

/// The cap is honoured and `truncated` says whether anything was past it
/// — including the boundary, where exactly `cap` rows is NOT truncated.
#[tokio::test]
async fn typed_capped_runner_stops_at_the_cap_and_says_so() {
    let (client, _tunnel) = lib_client().await;
    let sql = "SELECT g FROM generate_series(1, 10) g";
    for (cap, want_rows, want_truncated) in [
        (Some(3), 3, true),
        (Some(10), 10, false),
        (Some(11), 10, false),
        (None, 10, false),
    ] {
        let (typed, truncated) = pgman::conn::run_statement_typed_capped(&client, sql, cap)
            .await
            .expect("query runs");
        assert_eq!(typed.rows.len(), want_rows, "cap {cap:?}");
        assert_eq!(truncated, want_truncated, "cap {cap:?}");
    }
    // The connection is still usable after a stream was dropped mid-result.
    let (typed, _) = pgman::conn::run_statement_typed_capped(&client, "SELECT 1", Some(1))
        .await
        .expect("the connection survives a dropped stream");
    assert_eq!(typed.rows, vec![vec![Some("1".to_string())]]);
}

/// Drive `pgman mcp serve` over stdio: send `requests` (one frame each),
/// close stdin, and return every frame it wrote. Every stdout line must
/// parse as JSON — stdout is protocol only.
fn mcp_session(
    home: &std::path::Path,
    cwd: &std::path::Path,
    extra: &[&str],
    requests: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    mcp_session_env(home, cwd, extra, &[], requests)
}

/// [`mcp_session`] with extra environment variables for the server.
fn mcp_session_env(
    home: &std::path::Path,
    cwd: &std::path::Path,
    extra: &[&str],
    env: &[(&str, std::ffi::OsString)],
    requests: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    use std::io::Write;
    let mut child = Command::new(pgman_binary())
        .envs(env.iter().map(|(k, v)| (k, v)))
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .current_dir(cwd)
        .args(["mcp", "serve"])
        .args(extra)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn pgman mcp serve");
    {
        let mut stdin = child.stdin.take().unwrap();
        for r in requests {
            writeln!(stdin, "{r}").unwrap();
        }
    }
    let out = child.wait_with_output().expect("mcp serve exits");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| {
            serde_json::from_str(l).unwrap_or_else(|e| panic!("non-JSON on stdout ({e}): {l}"))
        })
        .collect()
}

/// [`mcp_session_env`], but each request waits for the reply before the
/// next is sent — for behaviour that depends on order across calls.
fn mcp_session_env_sequential(
    home: &std::path::Path,
    env: &[(&str, std::ffi::OsString)],
    requests: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    use std::io::{BufRead, Write};
    let mut child = Command::new(pgman_binary())
        .envs(env.iter().map(|(k, v)| (k, v)))
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .current_dir(home)
        .args(["mcp", "serve"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn pgman mcp serve");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut frames = Vec::new();
    for r in requests {
        writeln!(stdin, "{r}").unwrap();
        loop {
            let mut line = String::new();
            assert!(
                stdout.read_line(&mut line).unwrap() > 0,
                "server closed early"
            );
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            let done = v["id"] == r["id"];
            frames.push(v);
            if done {
                break;
            }
        }
    }
    drop(stdin);
    assert!(child.wait().unwrap().success());
    frames
}

fn tool_call(id: u64, name: &str, args: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": name, "arguments": args}})
}

/// The reply with `id`, as (text, isError). Calls run concurrently, so
/// replies arrive in any order.
fn reply(frames: &[serde_json::Value], id: u64) -> (String, bool) {
    let f = frames
        .iter()
        .find(|f| f["id"] == id)
        .unwrap_or_else(|| panic!("no reply to {id}: {frames:?}"));
    (
        f["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string(),
        f["result"]["isError"].as_bool().unwrap(),
    )
}

/// A personal connection to the compose postgres. The profile is
/// WRITABLE on purpose: MCP must be read-only regardless.
fn mcp_home(name: &str) -> std::path::PathBuf {
    let home = scratch_home(name, WRITABLE_PROFILE);
    std::fs::write(
        home.join(".config/pgman/connections.toml"),
        format!("[[connections]]\nname = \"test\"\nurl = \"{DSN}\"\n"),
    )
    .unwrap();
    home
}

#[test]
fn mcp_query_returns_typed_rows_and_says_when_it_truncated() {
    let home = mcp_home("mcp-rows");
    let frames = mcp_session(
        &home,
        &home,
        &[],
        &[tool_call(
            1,
            "query",
            serde_json::json!({"connection": "test", "max_rows": 2,
                "sql": "SELECT g AS n, NULL::text AS t FROM generate_series(1, 5) g"}),
        )],
    );
    let (text, is_error) = reply(&frames, 1);
    assert!(!is_error, "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        v["rows"],
        serde_json::json!([{"n": 1, "t": null}, {"n": 2, "t": null}])
    );
    assert_eq!(v["truncated"], true);
    assert!(
        v.get("replica").is_none(),
        "a direct connection gets no replica report: {text}"
    );
}

/// The read-only floor holds server-side, not just in the classifier:
/// `nextval` passes the guards (it is a SELECT) and the server refuses it.
#[test]
fn mcp_query_is_read_only_even_under_a_writable_profile() {
    let home = mcp_home("mcp-ro");
    let setup = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            "CREATE SEQUENCE IF NOT EXISTS pgman_mcp_seq",
        ],
    );
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let frames = mcp_session(
        &home,
        &home,
        &[],
        &[
            tool_call(
                1,
                "query",
                serde_json::json!({"connection": "test",
                "sql": "SELECT nextval('pgman_mcp_seq')"}),
            ),
            tool_call(
                2,
                "query",
                serde_json::json!({"connection": "test",
                "sql": "CREATE TABLE pgman_mcp_should_not_exist (id int)"}),
            ),
        ],
    );
    let (text, is_error) = reply(&frames, 1);
    assert!(is_error, "nextval ran: {text}");
    assert!(text.contains("read-only"), "{text}");
    let (text, is_error) = reply(&frames, 2);
    assert!(is_error, "CREATE ran: {text}");
    assert!(!table_exists(&home, "pgman_mcp_should_not_exist"));
}

/// A connection the checkout declares is listed and refused until the
/// server is started with `--project-connections`.
#[test]
fn mcp_discovered_connections_need_the_flag() {
    let home = mcp_home("mcp-discovered");
    let checkout = home.join("checkout");
    std::fs::create_dir_all(checkout.join(".pgman")).unwrap();
    std::fs::write(
        checkout.join(".pgman/pgman.toml"),
        format!("[[connections]]\nname = \"proj\"\nurl = \"{DSN}\"\n"),
    )
    .unwrap();
    let ask = [
        tool_call(1, "connections", serde_json::json!({})),
        tool_call(
            2,
            "query",
            serde_json::json!({"connection": "proj", "sql": "SELECT 1"}),
        ),
    ];
    let frames = mcp_session(&home, &checkout, &[], &ask);
    let (text, _) = reply(&frames, 1);
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let proj = v["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "proj")
        .expect("listed");
    assert_eq!(proj["usable"], false);
    assert!(
        !text.contains("pgman_test:pgman_test"),
        "password leaked: {text}"
    );
    let (text, is_error) = reply(&frames, 2);
    assert!(is_error && text.contains("--project-connections"), "{text}");

    let frames = mcp_session(&home, &checkout, &["--project-connections"], &ask[1..]);
    let (text, is_error) = reply(&frames, 2);
    assert!(!is_error, "{text}");
}

/// A real session holding a lock on a table shows up in `sessions_locks`
/// scoped to that table, with the lock named — and its long-open
/// transaction is reported even without the table filter.
#[tokio::test(flavor = "multi_thread")]
async fn mcp_sessions_locks_reports_a_lock_holder_on_the_named_table() {
    let home = mcp_home("mcp-locks");
    let setup = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            "CREATE TABLE IF NOT EXISTS pgman_mcp_locked (id int)",
        ],
    );
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );

    // Hold an open transaction with a lock on the table while MCP looks.
    let (notice_tx, _n) = tokio::sync::mpsc::unbounded_channel();
    let (notification_tx, _m) = tokio::sync::mpsc::unbounded_channel();
    let dsn = pgman::conn::Dsn::parse(DSN).unwrap();
    let (holder, _t) = pgman::conn::connect_only(dsn, false, 30_000, notice_tx, notification_tx)
        .await
        .unwrap();
    holder
        .batch_execute("BEGIN; LOCK TABLE pgman_mcp_locked IN SHARE MODE")
        .await
        .unwrap();
    let pid: i32 = holder
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);

    let home2 = home.clone();
    let frames = tokio::task::spawn_blocking(move || {
        mcp_session(
            &home2,
            &home2,
            &[],
            &[
                tool_call(
                    1,
                    "sessions_locks",
                    serde_json::json!({"connection": "test",
                    "tables": ["pgman_mcp_locked"], "min_xact_age_secs": 3600}),
                ),
                tool_call(
                    2,
                    "sessions_locks",
                    serde_json::json!({"connection": "test",
                    "min_xact_age_secs": 0}),
                ),
                tool_call(
                    3,
                    "sessions_locks",
                    serde_json::json!({"connection": "test",
                    "tables": ["x'; DROP TABLE pgman_mcp_locked; --"]}),
                ),
            ],
        )
    })
    .await
    .unwrap();
    holder.batch_execute("ROLLBACK").await.unwrap();

    let (text, is_error) = reply(&frames, 1);
    assert!(!is_error, "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["server_is_replica"], false);
    let row = v["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["pid"] == pid)
        .unwrap_or_else(|| panic!("lock holder {pid} not reported: {text}"));
    assert!(
        row["locks_on_tables"]
            .as_str()
            .unwrap()
            .contains("pgman_mcp_locked ShareLock"),
        "{row}"
    );

    let (text, _) = reply(&frames, 2);
    assert!(
        text.contains(&format!("\"pid\":{pid}")),
        "open transaction not reported: {text}"
    );

    let (text, is_error) = reply(&frames, 3);
    assert!(
        is_error && text.contains("not a plain table name"),
        "{text}"
    );
    assert!(table_exists(&home, "pgman_mcp_locked"));
}

/// `pending_patches` end to end: a version table holding 1 and 2, and a
/// patch directory with 1–3 plus a file that forgot its version row.
#[test]
fn mcp_pending_patches_reports_what_the_database_has_not_recorded() {
    let home = mcp_home("mcp-patches");
    let setup = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            "CREATE TABLE IF NOT EXISTS pgman_dbversion (scriptId int, description text); \
             DELETE FROM pgman_dbversion WHERE TRUE; \
             INSERT INTO pgman_dbversion VALUES (1, 'one'), (2, 'two')",
        ],
    );
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let checkout = home.join("checkout");
    let dir = checkout.join("schema_updates");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(checkout.join(".pgman")).unwrap();
    std::fs::write(
        checkout.join(".pgman/pgman.toml"),
        "[patches]\ndir = \"schema_updates\"\ntable = \"pgman_dbversion\"\ncolumn = \"scriptId\"\n",
    )
    .unwrap();
    for (file, body) in [
        ("001-a.sql", "INSERT INTO pgman_dbversion (scriptId, description) VALUES (1, 'a');"),
        ("002-b.sql", "INSERT INTO pgman_dbversion (scriptId, description) VALUES (2, 'b');"),
        ("003-c.sql", "-- pending\nINSERT INTO pgman_dbversion (scriptId, description)\n  VALUES (3, 'c, with a comma');"),
        ("004-d.sql", "ALTER TABLE x ADD COLUMN y int;"),
    ] {
        std::fs::write(dir.join(file), body).unwrap();
    }
    let frames = mcp_session(
        &home,
        &checkout,
        &[],
        &[
            tool_call(
                1,
                "pending_patches",
                serde_json::json!({"connection": "test"}),
            ),
            tool_call(
                2,
                "pending_patches",
                serde_json::json!({"connection": "test", "dir": ".."}),
            ),
        ],
    );
    let (text, is_error) = reply(&frames, 1);
    assert!(!is_error, "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["applied_max"], 2);
    assert_eq!(
        v["pending"],
        serde_json::json!([{"file": "003-c.sql", "id": 3, "filename_number": 3}])
    );
    assert_eq!(v["unversioned_recent"][0]["file"], "004-d.sql");
    let (text, is_error) = reply(&frames, 2);
    assert!(is_error && text.contains("outside"), "{text}");
}

/// A fake `aws` CLI on the server's PATH: logs its arguments, answers
/// `sts get-caller-identity` with `$FAKE_AWS_ACCOUNT` and
/// `secretsmanager get-secret-value` with credentials for the compose
/// postgres. Returns the PATH to use and the log file.
#[cfg(unix)]
fn fake_aws(home: &std::path::Path) -> (std::ffi::OsString, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let bin = home.join("fakebin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = home.join("aws-calls.log");
    let _ = std::fs::remove_file(&log);
    let script = bin.join("aws");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\ncase \"$1 $2\" in\n  \"sts get-caller-identity\") echo \"$FAKE_AWS_ACCOUNT\" ;;\n  \"secretsmanager get-secret-value\") echo '{{\"username\":\"pgman_test\",\"password\":\"pgman_test\",\"engine\":\"postgres\"}}' ;;\n  \"rds describe-db-instances\") cat \"$FAKE_RDS_JSON\" ;;\n  \"secretsmanager list-secrets\") echo '{{\"SecretList\":[{{\"Name\":\"demo/database/ledger/credentials\"}},{{\"Name\":\"demo/database/ledger_replica/credentials\"}},{{\"Name\":\"demo-database-password\"}}]}}' ;;\n  \"ec2 describe-instances\") echo '{{\"Reservations\":[{{\"Instances\":[{{\"InstanceId\":\"i-1\",\"VpcId\":\"vpc-1\",\"PublicDnsName\":\"bastion.example.com\"}}]}}]}}' ;;\n  \"configure get\") echo us-west-1 ;;\n  *) echo \"unexpected: $*\" >&2; exit 1 ;;\nesac\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = std::ffi::OsString::from(&bin);
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    (path, log)
}

/// The password comes from the secret, read through the aws CLI with the
/// account checked first — and a wrong account stops before the secret
/// is read. A primary serves only sessions_locks.
#[cfg(unix)]
#[test]
fn mcp_secret_connections_read_the_secret_and_respect_the_role() {
    let home = scratch_home("mcp-secret", WRITABLE_PROFILE);
    std::fs::write(
        home.join(".config/pgman/connections.toml"),
        "[[connections]]\nname = \"replica\"\nurl = \"postgres://127.0.0.1:55432/pgman_test?sslmode=disable\"\n\
         secret = \"test/database/pgman/credentials\"\naws_region = \"us-west-1\"\n\
         aws_account = \"210987654321\"\nrole = \"replica\"\n\
         [[connections]]\nname = \"primary\"\nurl = \"postgres://127.0.0.1:55432/pgman_test?sslmode=disable\"\n\
         secret = \"test/database/pgman/credentials\"\nrole = \"primary\"\n",
    )
    .unwrap();
    let (path, log) = fake_aws(&home);
    let run = |account: &str, requests: &[serde_json::Value]| {
        mcp_session_env(
            &home,
            &home,
            &[],
            &[("PATH", path.clone()), ("FAKE_AWS_ACCOUNT", account.into())],
            requests,
        )
    };

    let frames = run(
        "210987654321",
        &[
            tool_call(
                1,
                "query",
                serde_json::json!({"connection": "replica", "sql": "SELECT current_user AS u"}),
            ),
            tool_call(2, "connections", serde_json::json!({})),
        ],
    );
    let (text, is_error) = reply(&frames, 1);
    assert!(!is_error, "{text}");
    assert!(text.contains("\"u\":\"pgman_test\""), "{text}");
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("sts get-caller-identity"), "{calls}");
    assert!(
        calls.contains(
            "secretsmanager get-secret-value --secret-id test/database/pgman/credentials"
        ),
        "{calls}"
    );
    let (text, _) = reply(&frames, 2);
    assert!(
        text.contains("Secrets Manager") && text.contains("\"role\":\"replica\""),
        "{text}"
    );

    std::fs::remove_file(&log).unwrap();
    let frames = run(
        "111111111111",
        &[tool_call(
            1,
            "query",
            serde_json::json!({"connection": "replica", "sql": "SELECT 1"}),
        )],
    );
    let (text, is_error) = reply(&frames, 1);
    assert!(is_error && text.contains("111111111111"), "{text}");
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        !calls.contains("secretsmanager"),
        "secret read despite the wrong account: {calls}"
    );

    let frames = run(
        "210987654321",
        &[
            tool_call(
                1,
                "query",
                serde_json::json!({"connection": "primary", "sql": "SELECT 1"}),
            ),
            tool_call(
                2,
                "sessions_locks",
                serde_json::json!({"connection": "primary"}),
            ),
        ],
    );
    let (text, is_error) = reply(&frames, 1);
    assert!(is_error && text.contains("PRIMARY"), "{text}");
    let (text, is_error) = reply(&frames, 2);
    assert!(!is_error, "{text}");
}

/// `environments` end to end with the fake aws CLI: the account is
/// checked before instances are listed, a matched primary brings its
/// replica, and a wrong account lists nothing.
#[cfg(unix)]
#[test]
fn mcp_environments_lists_rds_instances_with_their_replicas() {
    let home = scratch_home("mcp-envs", WRITABLE_PROFILE);
    std::fs::write(
        home.join(".config/pgman/environments.toml"),
        "[[environments]]\nname = \"demo\"\ndatabases = [\"ledger\", \"ledgerlog\"]\n\
         source = { kind = \"rds\", instances = [\"demo\"], region = \"us-west-1\", account = \"210987654321\" }\n",
    )
    .unwrap();
    let rds = home.join("rds.json");
    std::fs::write(
        &rds,
        r#"{"DBInstances": [
          {"DBInstanceIdentifier": "demo", "Engine": "postgres", "EngineVersion": "16.10", "DBInstanceStatus": "available",
           "DBInstanceClass": "db.t4g.medium", "Endpoint": {"Address": "demo.x.rds.amazonaws.com", "Port": 5432},
           "ReadReplicaDBInstanceIdentifiers": ["demo-read-only-replica"], "DBName": "ledger"},
          {"DBInstanceIdentifier": "demo-read-only-replica", "Engine": "postgres", "EngineVersion": "16.10",
           "DBInstanceStatus": "available", "DBInstanceClass": "db.t4g.medium",
           "Endpoint": {"Address": "demo-read-only-replica.x.rds.amazonaws.com", "Port": 5432},
           "ReadReplicaSourceDBInstanceIdentifier": "demo", "DBName": "ledger"},
          {"DBInstanceIdentifier": "production", "Engine": "postgres", "EngineVersion": "16.10",
           "DBInstanceStatus": "available", "DBInstanceClass": "db.t4g.medium", "DBName": "prod"}
        ]}"#,
    )
    .unwrap();
    let (path, log) = fake_aws(&home);
    let run = |account: &str| {
        mcp_session_env(
            &home,
            &home,
            &[],
            &[
                ("PATH", path.clone()),
                ("FAKE_AWS_ACCOUNT", account.into()),
                ("FAKE_RDS_JSON", rds.clone().into()),
            ],
            &[tool_call(1, "environments", serde_json::json!({}))],
        )
    };

    let frames = run("210987654321");
    let (text, is_error) = reply(&frames, 1);
    assert!(!is_error, "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let rows: Vec<(String, String, String)> = v["environments"][0]["databases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["server"].as_str().unwrap().to_string(),
                d["database"].as_str().unwrap().to_string(),
                d["role"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("demo".into(), "ledger".into(), "primary".into()),
            ("demo".into(), "ledgerlog".into(), "primary".into()),
            (
                "demo-read-only-replica".into(),
                "ledger".into(),
                "replica".into()
            ),
            (
                "demo-read-only-replica".into(),
                "ledgerlog".into(),
                "replica".into()
            ),
        ]
    );
    let calls = std::fs::read_to_string(&log).unwrap();
    let sts = calls
        .find("sts get-caller-identity")
        .expect("account checked");
    let rds_call = calls
        .find("rds describe-db-instances")
        .expect("instances listed");
    assert!(sts < rds_call, "account must be checked first: {calls}");

    std::fs::remove_file(&log).unwrap();
    let frames = run("111111111111");
    let (text, _) = reply(&frames, 1);
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(
        v["environments"][0]["error"]
            .as_str()
            .unwrap()
            .contains("111111111111"),
        "{text}"
    );
    assert!(v["environments"][0]["databases"]
        .as_array()
        .unwrap()
        .is_empty());
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        !calls.contains("rds describe"),
        "listed despite the wrong account: {calls}"
    );
}

/// A `primary` connection may answer "did the patch land?" but still not
/// run client SQL; a `replica` connection that is really a primary says
/// so in every result.
#[test]
fn mcp_roles_primary_verifies_patches_and_a_false_replica_is_flagged() {
    let home = scratch_home("mcp-roles", WRITABLE_PROFILE);
    std::fs::write(
        home.join(".config/pgman/connections.toml"),
        format!(
            "[[connections]]\nname = \"primary\"\nurl = \"{DSN}\"\nrole = \"primary\"\n\
             [[connections]]\nname = \"not-really-a-replica\"\nurl = \"{DSN}\"\nrole = \"replica\"\n"
        ),
    )
    .unwrap();
    let setup = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            "CREATE TABLE IF NOT EXISTS pgman_dbversion_roles (scriptId int); \
             DELETE FROM pgman_dbversion_roles WHERE TRUE; \
             INSERT INTO pgman_dbversion_roles VALUES (1)",
        ],
    );
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let dir = home.join("patches");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("001-a.sql"),
        "INSERT INTO pgman_dbversion_roles (scriptId) VALUES (1);",
    )
    .unwrap();
    std::fs::write(
        dir.join("002-b.sql"),
        "INSERT INTO pgman_dbversion_roles (scriptId) VALUES (2);",
    )
    .unwrap();
    let patches = serde_json::json!({"dir": "patches", "table": "pgman_dbversion_roles", "column": "scriptId"});
    let mut on_primary = patches.clone();
    on_primary["connection"] = "primary".into();
    let mut on_replica = patches;
    on_replica["connection"] = "not-really-a-replica".into();
    let frames = mcp_session(
        &home,
        &home,
        &[],
        &[
            tool_call(1, "pending_patches", on_primary),
            tool_call(
                2,
                "query",
                serde_json::json!({"connection": "primary", "sql": "SELECT 1"}),
            ),
            tool_call(3, "pending_patches", on_replica),
        ],
    );
    let (text, is_error) = reply(&frames, 1);
    assert!(!is_error, "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["pending"][0]["id"], 2);
    assert!(
        v.get("replica").is_none(),
        "no replica report on a primary: {text}"
    );
    let (text, is_error) = reply(&frames, 2);
    assert!(is_error && text.contains("PRIMARY"), "{text}");
    let (text, is_error) = reply(&frames, 3);
    assert!(!is_error, "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["replica"]["in_recovery"], false);
    assert!(
        v["replica"]["note"]
            .as_str()
            .unwrap()
            .contains("NOT in recovery"),
        "{text}"
    );
}

/// `pgman env discover` proposes from the account and saves only when
/// asked, appending; a second save of the same environment is refused.
/// No secret value is ever read.
#[cfg(unix)]
#[test]
fn env_discover_proposes_then_appends_on_save() {
    let home = scratch_home("env-discover", WRITABLE_PROFILE);
    let cfg = home.join(".config/pgman/environments.toml");
    std::fs::write(
        &cfg,
        "# mine\n[[environments]]\nname = \"prod\"\nsource = { kind = \"rds\", instances = [\"production\"] }\n",
    )
    .unwrap();
    let rds = home.join("rds.json");
    std::fs::write(
        &rds,
        r#"{"DBInstances": [
          {"DBInstanceIdentifier": "demo", "Engine": "postgres", "EngineVersion": "16.10", "DBInstanceStatus": "available",
           "DBInstanceClass": "db.t4g.medium", "Endpoint": {"Address": "demo.x.rds.amazonaws.com", "Port": 5432},
           "ReadReplicaDBInstanceIdentifiers": ["demo-read-only-replica"], "DBName": "ledger", "DBSubnetGroup": {"VpcId": "vpc-1"}},
          {"DBInstanceIdentifier": "demo-read-only-replica", "Engine": "postgres", "EngineVersion": "16.10",
           "DBInstanceStatus": "available", "DBInstanceClass": "db.t4g.medium",
           "Endpoint": {"Address": "demo-read-only-replica.x.rds.amazonaws.com", "Port": 5432},
           "ReadReplicaSourceDBInstanceIdentifier": "demo", "DBName": "ledger", "DBSubnetGroup": {"VpcId": "vpc-1"}},
          {"DBInstanceIdentifier": "production", "Engine": "postgres", "EngineVersion": "16.10",
           "DBInstanceStatus": "available", "DBInstanceClass": "db.t4g.medium", "DBName": "prod"}
        ]}"#,
    )
    .unwrap();
    let (path, log) = fake_aws(&home);
    let run = |args: &[&str]| {
        Command::new(pgman_binary())
            .env("PATH", &path)
            .env("FAKE_AWS_ACCOUNT", "210987654321")
            .env("FAKE_RDS_JSON", &rds)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .current_dir(&home)
            .args(["env", "discover"])
            .args(args)
            .output()
            .expect("spawn pgman env discover")
    };

    let out = run(&[]);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("# discovered in AWS account 210987654321, us-west-1"),
        "{text}"
    );
    assert!(
        text.contains("ssh_tunnel = \"bastion.example.com\""),
        "{text}"
    );
    assert!(
        text.contains("\"demo-read-only-replica\" = \"demo/database/ledger_replica/credentials\""),
        "{text}"
    );
    assert!(
        text.contains("# production: already covered by environment \"prod\""),
        "{text}"
    );
    assert!(
        !text.contains("demo-database-password"),
        "never a master-password secret: {text}"
    );
    assert!(
        std::fs::read_to_string(&cfg)
            .unwrap()
            .ends_with("instances = [\"production\"] }\n"),
        "printing saves nothing"
    );

    let out = run(&["--save"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("added demo to "));
    let saved = std::fs::read_to_string(&cfg).unwrap();
    assert!(
        saved.starts_with("# mine\n[[environments]]\nname = \"prod\""),
        "kept: {saved}"
    );
    assert!(saved.contains("name = \"demo\""), "{saved}");
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&cfg).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    let out = run(&["--save", "demo"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("already covered by environment \"demo\"")
    );
    assert_eq!(
        std::fs::read_to_string(&cfg).unwrap(),
        saved,
        "a refused save writes nothing"
    );

    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        !calls.contains("get-secret-value"),
        "no secret value read: {calls}"
    );
}

/// Over MCP: discovery proposes and writes nothing; save appends the named
/// proposal; a covered one, or any save under --read-only, is refused.
#[cfg(unix)]
#[test]
fn mcp_environments_discover_and_save() {
    let home = scratch_home("mcp-env-save", WRITABLE_PROFILE);
    let cfg = home.join(".config/pgman/environments.toml");
    let _ = std::fs::remove_file(&cfg);
    let rds = home.join("rds.json");
    std::fs::write(
        &rds,
        r#"{"DBInstances": [
          {"DBInstanceIdentifier": "demo", "Engine": "postgres", "EngineVersion": "16.10", "DBInstanceStatus": "available",
           "DBInstanceClass": "db.t4g.medium", "Endpoint": {"Address": "demo.x.rds.amazonaws.com", "Port": 5432},
           "DBName": "ledger", "DBSubnetGroup": {"VpcId": "vpc-1"}}
        ]}"#,
    )
    .unwrap();
    let (path, log) = fake_aws(&home);
    let run = |extra: &[&str], requests: &[serde_json::Value]| {
        mcp_session_env(
            &home,
            &home,
            extra,
            &[
                ("PATH", path.clone()),
                ("FAKE_AWS_ACCOUNT", "210987654321".into()),
                ("FAKE_RDS_JSON", rds.clone().into()),
            ],
            requests,
        )
    };

    let frames = run(
        &[],
        &[tool_call(1, "environments_discover", serde_json::json!({}))],
    );
    let (text, is_error) = reply(&frames, 1);
    assert!(!is_error, "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["discovered_in"], "AWS account 210987654321, us-west-1");
    assert_eq!(v["proposals"][0]["name"], "demo");
    let toml = v["proposals"][0]["toml"].as_str().unwrap();
    assert!(
        toml.contains("\"demo\" = \"demo/database/ledger/credentials\""),
        "{toml}"
    );
    assert!(!cfg.exists(), "discovery writes nothing");

    let frames = run(
        &["--read-only"],
        &[tool_call(
            1,
            "environments_save",
            serde_json::json!({"names": ["demo"]}),
        )],
    );
    let (text, is_error) = reply(&frames, 1);
    assert!(is_error && text.contains("--read-only"), "{text}");
    assert!(!cfg.exists(), "--read-only saves nothing");

    let frames = run(
        &[],
        &[
            tool_call(
                1,
                "environments_save",
                serde_json::json!({"names": ["demo"]}),
            ),
            tool_call(
                2,
                "environments_save",
                serde_json::json!({"names": ["demo"]}),
            ),
            tool_call(
                3,
                "environments_save",
                serde_json::json!({"names": ["nope"]}),
            ),
        ],
    );
    // Calls run concurrently: whichever of 1 and 2 lands first adds demo,
    // and the other finds it covered. Never both — that was a lost update.
    let (a, b) = (reply(&frames, 1), reply(&frames, 2));
    let (added, refused) = if a.1 { (b, a) } else { (a, b) };
    assert!(!added.1, "{added:?} / {refused:?}");
    assert!(added.0.contains("\"added\":[\"demo\"]"), "{}", added.0);
    assert!(
        refused.1 && refused.0.contains("already covered"),
        "{}",
        refused.0
    );
    let saved = std::fs::read_to_string(&cfg).unwrap();
    assert_eq!(saved.matches("name = \"demo\"").count(), 1, "{saved}");
    let (text, is_error) = reply(&frames, 3);
    assert!(
        is_error && text.contains("no proposed environment"),
        "{text}"
    );
    assert_eq!(std::fs::read_to_string(&cfg).unwrap(), saved);

    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        !calls.contains("get-secret-value"),
        "no secret value read: {calls}"
    );
}

/// A save in another process waits for the lock, then lands.
#[cfg(unix)]
#[test]
fn env_discover_save_waits_for_another_process_holding_the_lock() {
    let home = scratch_home("env-lock", WRITABLE_PROFILE);
    let cfg = home.join(".config/pgman/environments.toml");
    let _ = std::fs::remove_file(&cfg);
    let rds = home.join("rds.json");
    std::fs::write(
        &rds,
        r#"{"DBInstances": [{"DBInstanceIdentifier": "demo", "Engine": "postgres", "EngineVersion": "16.10",
           "DBInstanceStatus": "available", "DBInstanceClass": "db.t4g.medium",
           "Endpoint": {"Address": "demo.x.rds.amazonaws.com", "Port": 5432}, "DBName": "ledger"}]}"#,
    )
    .unwrap();
    let (path, _log) = fake_aws(&home);
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(home.join(".config/pgman/.environments.toml.lock"))
        .unwrap();
    lock.lock().unwrap();
    let mut child = Command::new(pgman_binary())
        .env("PATH", &path)
        .env("FAKE_AWS_ACCOUNT", "210987654321")
        .env("FAKE_RDS_JSON", &rds)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .args(["env", "discover", "--save"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(
        child.try_wait().unwrap().is_none(),
        "still waiting for the lock"
    );
    assert!(
        !cfg.exists(),
        "nothing written while another process holds it"
    );
    lock.unlock().unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("added demo"));
    assert!(std::fs::read_to_string(&cfg)
        .unwrap()
        .contains("name = \"demo\""));
}

/// A probe against a real server: version and recovery from the status
/// read, the patch level from the version table, pending from the patch
/// files. Read-only: the table is set up beforehand.
#[test]
fn environment_probe_reads_version_patch_level_and_pending() {
    let home = scratch_home("env-probe", WRITABLE_PROFILE);
    // A table of this run's own: the profile blocks DROP, and the test
    // database is a throwaway container.
    let table = format!("pgman_it_versions_{}", std::process::id());
    let setup = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            &format!("CREATE TABLE {table} (id int); INSERT INTO {table} (id) VALUES (1), (3)"),
        ],
    );
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let dir = home.join("patches");
    std::fs::create_dir_all(&dir).unwrap();
    for id in 1..=3 {
        std::fs::write(
            dir.join(format!("{id:03}.sql")),
            format!("INSERT INTO {table} (id) VALUES ({id});\n"),
        )
        .unwrap();
    }
    let target = |table: &str, databases: &[&str]| pgman::inventory::EnvReport {
        name: "local".into(),
        source: "test".into(),
        targets: vec![pgman::inventory::Target {
            database: "pgman_test".into(),
            server: "local".into(),
            role: pgman::mcp::connections::Role::Direct,
            replica_of: None,
            host: None,
            port: None,
            engine: None,
            status: None,
            class: None,
            multi_az: None,
            connect: Ok(pgman::inventory::ConnectSpec {
                dsn: pgman::conn::Dsn::parse(DSN).unwrap(),
                secret: None,
                password_env: None,
            }),
        }],
        error: None,
        probe: Some(pgman::inventory::ProbeSettings {
            version_table: Some(pgman::inventory::VersionTable {
                table: table.into(),
                column: "id".into(),
                patches: Some(dir.display().to_string()),
                databases: databases.iter().map(|d| d.to_string()).collect(),
            }),
        }),
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let probe = |table: &str, databases: &[&str]| {
        rt.block_on(pgman::inventory::probe::run(
            target(table, databases),
            std::sync::Arc::new(|_| panic!("no secret configured")),
        ))
        .remove(0)
        .expect("connected, not via a replica")
        .expect("probe answered")
    };

    let p = probe(&table, &[]);
    assert!(p.server_version.is_some(), "{p:?}");
    assert_eq!(p.in_recovery, Some(false));
    assert_eq!(p.schema_version, Some(3));
    assert_eq!(p.pending, Some(1), "002 is not recorded");
    assert_eq!(p.schema_error, None);

    // A missing version table spoils only the patch level.
    let p = probe("pgman_it_no_such_table", &[]);
    assert!(p.server_version.is_some());
    assert!(
        p.schema_error
            .as_deref()
            .unwrap()
            .contains("does not exist"),
        "{p:?}"
    );
    assert_eq!(p.schema_version, None);

    // Named for other databases only: not read here, so no error.
    let p = probe("pgman_it_no_such_table", &["some_other_db"]);
    assert_eq!((p.schema_version, &p.schema_error), (None, &None), "{p:?}");
    assert!(p.server_version.is_some());
    let p = probe(&table, &["pgman_test"]);
    assert_eq!(p.schema_version, Some(3), "named: read");
}

/// Over MCP, `probe: true` reads the databases of environments that
/// opted in, and leaves the others alone; without it nothing connects.
#[cfg(unix)]
#[test]
fn mcp_environments_probe_reads_opted_in_environments_only() {
    let home = scratch_home("mcp-env-probe", WRITABLE_PROFILE);
    std::fs::write(
        home.join(".config/pgman/connections.toml"),
        "[[connections]]\nname = \"local\"\nurl = \"postgres://pgman_test:pgman_test@127.0.0.1:55432/pgman_test?sslmode=disable\"\n",
    )
    .unwrap();
    std::fs::write(
        home.join(".config/pgman/environments.toml"),
        "[[environments]]\nname = \"probed\"\nsource = { kind = \"static\", connections = [\"local\"] }\nprobe = true\n\
         [[environments]]\nname = \"quiet\"\nsource = { kind = \"static\", connections = [\"local\"] }\n",
    )
    .unwrap();
    let frames = mcp_session_env(
        &home,
        &home,
        &[],
        &[],
        &[
            tool_call(1, "environments", serde_json::json!({"probe": true})),
            tool_call(2, "environments", serde_json::json!({})),
            tool_call(3, "environments", serde_json::json!({"probe": "yes"})),
        ],
    );
    let (text, is_error) = reply(&frames, 1);
    assert!(!is_error, "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let probe = &v["environments"][0]["databases"][0]["probe"];
    assert!(probe["server_version"].is_string(), "{text}");
    assert_eq!(probe["in_recovery"], false, "{text}");
    assert!(
        probe["sessions"].as_i64().unwrap() >= 1,
        "the probe's own session counts: {text}"
    );
    assert!(probe["size"].is_string(), "{text}");
    assert!(v["environments"][1]["probe"]
        .as_str()
        .unwrap()
        .starts_with("not enabled"));
    let (text, _) = reply(&frames, 2);
    assert!(
        !text.contains("server_version"),
        "nothing connects unasked: {text}"
    );
    let (text, is_error) = reply(&frames, 3);
    assert!(is_error && text.contains("'probe' must be"), "{text}");
}

/// A stand-in `ssh` on PATH: logs each launch, then forwards its `-L`
/// port to the target, so a tunnelled connection reaches the test
/// Postgres without an ssh server.
#[cfg(unix)]
fn fake_ssh(home: &std::path::Path) -> (std::ffi::OsString, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let bin = home.join("fakessh");
    std::fs::create_dir_all(&bin).unwrap();
    let log = home.join("ssh-launches.log");
    let _ = std::fs::remove_file(&log);
    let script = bin.join("ssh");
    std::fs::write(
        &script,
        format!(
            r#"#!/usr/bin/env python3
import socket, sys, threading
open({log:?}, "a").write(" ".join(sys.argv[1:]) + "\n")
fwd = sys.argv[sys.argv.index("-L") + 1].split(":")
lport, host, port = int(fwd[1]), fwd[2], int(fwd[3])
srv = socket.socket(); srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", lport)); srv.listen(16)
def pipe(a, b):
    try:
        while True:
            d = a.recv(65536)
            if not d: break
            b.sendall(d)
    except OSError: pass
    finally:
        for s in (a, b):
            try: s.close()
            except OSError: pass
while True:
    c, _ = srv.accept()
    u = socket.create_connection((host, port))
    threading.Thread(target=pipe, args=(c, u), daemon=True).start()
    threading.Thread(target=pipe, args=(u, c), daemon=True).start()
"#,
            log = log.display().to_string()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = std::ffi::OsString::from(&bin);
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    (path, log)
}

/// Two calls to a tunnelled connection share one ssh: the tunnel stays
/// open between calls, while each call still gets a fresh read-only
/// session.
#[cfg(unix)]
#[test]
fn mcp_reuses_one_tunnel_across_calls() {
    let home = scratch_home("mcp-tunnel-reuse", WRITABLE_PROFILE);
    std::fs::write(
        home.join(".config/pgman/connections.toml"),
        "[[connections]]\nname = \"via\"\nurl = \"postgres://pgman_test:pgman_test@127.0.0.1:55432/pgman_test?sslmode=disable\"\n\
         ssh_tunnel = \"bastion.example\"\n",
    )
    .unwrap();
    let (path, log) = fake_ssh(&home);
    let frames = mcp_session_env(
        &home,
        &home,
        &[],
        &[("PATH", path)],
        &[tool_call(
            1,
            "query",
            serde_json::json!({"connection": "via", "sql": "SELECT pg_backend_pid() AS pid"}),
        )],
    );
    let (first, is_error) = reply(&frames, 1);
    assert!(!is_error, "{first}");
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 1);
    // A second session of the server is a second process: it opens its
    // own. Within one server, calls share the tunnel:
    let (path, log) = fake_ssh(&home);
    let frames = mcp_session_env_sequential(
        &home,
        &[("PATH", path)],
        &[
            tool_call(
                1,
                "query",
                serde_json::json!({"connection": "via", "sql": "SELECT pg_backend_pid() AS pid"}),
            ),
            tool_call(
                2,
                "query",
                serde_json::json!({"connection": "via", "sql": "SELECT pg_backend_pid() AS pid"}),
            ),
        ],
    );
    let (a, e1) = reply(&frames, 1);
    let (b, e2) = reply(&frames, 2);
    assert!(!e1 && !e2, "{a}\n{b}");
    assert_eq!(
        std::fs::read_to_string(&log).unwrap().lines().count(),
        1,
        "one ssh for both calls"
    );
    assert_ne!(a, b, "a fresh backend (session) per call");
}

/// RDS has a database nobody may connect to (`rdsadmin`), and sizing it
/// fails. Connecting as a user like that must still work: the start
/// card's list was failing the whole connection.
#[test]
fn connecting_works_when_a_database_on_the_server_refuses_connect() {
    let home = scratch_home("noconnect", WRITABLE_PROFILE);
    for sql in [
        "DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'pgman_lim') THEN \
         CREATE ROLE pgman_lim LOGIN PASSWORD 'pgman_lim'; END IF; END $$",
        "CREATE DATABASE pgman_noconnect",
        "REVOKE CONNECT ON DATABASE pgman_noconnect FROM PUBLIC",
    ] {
        // CREATE DATABASE fails harmlessly once the database exists.
        let _ = batch_with_home(&home, &["--yes", "--sql", sql]);
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let dsn = pgman::conn::Dsn::parse(
        "postgres://pgman_lim:pgman_lim@127.0.0.1:55432/pgman_test?sslmode=disable",
    )
    .unwrap();
    let booted = rt.block_on(async {
        let (notice_tx, _n) = tokio::sync::mpsc::unbounded_channel();
        let (notification_tx, _m) = tokio::sync::mpsc::unbounded_channel();
        pgman::conn::connect_and_bootstrap(
            dsn,
            true,
            5_000,
            pgman::conn::BOOTSTRAP_SQL.to_string(),
            notice_tx,
            notification_tx,
        )
        .await
    });
    let booted = booted.unwrap_or_else(|e| panic!("connect failed: {e}"));
    let names: Vec<&str> = booted.grid.rows.iter().map(|r| r[0].as_str()).collect();
    assert!(names.contains(&"pgman_test"), "{names:?}");
    assert!(
        names.contains(&"pgman_noconnect"),
        "listed, just not sized: {names:?}"
    );

    // And a list that fails outright still leaves a connection.
    let dsn = pgman::conn::Dsn::parse(
        "postgres://pgman_lim:pgman_lim@127.0.0.1:55432/pgman_test?sslmode=disable",
    )
    .unwrap();
    let booted = rt.block_on(async {
        let (notice_tx, _n) = tokio::sync::mpsc::unbounded_channel();
        let (notification_tx, _m) = tokio::sync::mpsc::unbounded_channel();
        pgman::conn::connect_and_bootstrap(
            dsn,
            true,
            5_000,
            "select 1/0".to_string(),
            notice_tx,
            notification_tx,
        )
        .await
    });
    let booted = booted.unwrap_or_else(|e| panic!("a failed list failed the connection: {e}"));
    assert!(booted.grid.rows.is_empty());
}
