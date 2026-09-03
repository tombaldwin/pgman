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
    let out = Command::new(pgman_binary())
        .args([
            "--batch",
            "--yes",
            "--dsn",
            DSN,
            "--sql",
            "BEGIN; SELECT 1; COMMIT",
        ])
        .output()
        .expect("spawn pgman");
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
    // it `Other` and asks. This test is about notice routing.
    let out = Command::new(pgman_binary())
        .args([
            "--batch",
            "--yes",
            "--dsn",
            DSN,
            "--sql",
            "DO $$ BEGIN RAISE NOTICE 'pgman-test-notice'; END $$",
        ])
        .output()
        .expect("spawn pgman");
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
    let out = Command::new(pgman_binary())
        .args([
            "--batch",
            "--dsn",
            DSN,
            "--sql",
            "DO $$ BEGIN RAISE NOTICE 'pgman-test-notice'; END $$",
        ])
        .output()
        .expect("spawn pgman");

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
    let home = std::env::temp_dir().join(format!("pgman-it-{name}-{}", std::process::id()));
    let cfg = home.join(".config/pgman");
    std::fs::create_dir_all(&cfg).expect("create scratch config dir");
    std::fs::write(cfg.join("safety.toml"), body).expect("write safety.toml");
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
    String::from_utf8_lossy(&out.stdout).contains('t')
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
    assert!(
        stderr.contains("hint:")
            && stderr.contains("safety.toml")
            && stderr.contains("read_only")
            && stderr.contains("docs/configuration.md"),
        "got: {stderr}"
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
