//! Integration tests against a real Postgres. Gated behind
//! `--features integration` so plain `cargo test` doesn't try to
//! reach a server that isn't there. Bring the DB up with:
//!
//!   docker compose -f docker-compose.test.yml up -d --wait
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

/// DSN of the compose file's streaming standby of [`DSN`].
const STANDBY_DSN: &str =
    "postgres://pgman_test:pgman_test@127.0.0.1:55433/pgman_test?sslmode=disable";

/// A read-only probe connection to `dsn`.
async fn probe_client(dsn: &str) -> std::sync::Arc<tokio_postgres::Client> {
    pgman::conn::connect_via(pgman::conn::Dsn::parse(dsn).unwrap(), None, 10_000)
        .await
        .unwrap_or_else(|e| panic!("{dsn}: {e} (is the compose file's standby up?)"))
}

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

/// `batch_with_home`, against `dsn` instead of the default database.
fn batch_with_home_dsn(home: &std::path::Path, dsn: &str, args: &[&str]) -> std::process::Output {
    Command::new(pgman_binary())
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .args(["--batch", "--dsn", dsn])
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
            "#!/bin/sh\necho \"$@\" >> '{}'\ncase \"$1 $2\" in\n  \"sts get-caller-identity\") echo \"$FAKE_AWS_ACCOUNT\" ;;\n  \"secretsmanager get-secret-value\") echo '{{\"username\":\"pgman_test\",\"password\":\"pgman_test\",\"engine\":\"postgres\"}}' ;;\n  \"rds describe-db-instances\") cat \"$FAKE_RDS_JSON\" ;;\n  \"rds describe-db-log-files\") echo '{{\"DescribeDBLogFiles\":[{{\"LogFileName\":\"error/postgresql.log.1\",\"LastWritten\":1}}]}}' ;;\n  \"rds download-db-log-file-portion\") printf '%s' '{{\"LogFileData\":\"2026-10-19 20:54:06 UTC:10.0.0.1(1):ledger@ledger:[14]:ERROR:  permission denied for database rdsadmin\\n2026-10-19 20:54:06 UTC:10.0.0.1(1):ledger@ledger:[14]:STATEMENT:  select secret\"}}' ;;\n  \"rds describe-db-engine-versions\") echo '{{\"DBEngineVersions\":[{{\"ValidUpgradeTarget\":[{{\"EngineVersion\":\"16.15\",\"IsMajorVersionUpgrade\":false}},{{\"EngineVersion\":\"18.6\",\"IsMajorVersionUpgrade\":true}}]}}]}}' ;;\n  \"secretsmanager list-secrets\") echo '{{\"SecretList\":[{{\"Name\":\"demo/database/ledger/credentials\"}},{{\"Name\":\"demo/database/ledger_replica/credentials\"}},{{\"Name\":\"demo-database-password\"}}]}}' ;;\n  \"ec2 describe-instances\") echo '{{\"Reservations\":[{{\"Instances\":[{{\"InstanceId\":\"i-1\",\"VpcId\":\"vpc-1\",\"PublicDnsName\":\"bastion.example.com\"}}]}}]}}' ;;\n  \"configure get\") echo us-west-1 ;;\n  *) echo \"unexpected: $*\" >&2; exit 1 ;;\nesac\n",
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
            (
                "demo-read-only-replica".into(),
                "ledger".into(),
                "replica".into()
            ),
            ("demo".into(), "ledgerlog".into(), "primary".into()),
            (
                "demo-read-only-replica".into(),
                "ledgerlog".into(),
                "replica".into()
            ),
        ]
    );
    assert_eq!(
        v["environments"][0]["databases"][0]["engine"], "postgres 16.10 (↑16.15 · 18.6)",
        "{text}"
    );
    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        calls.matches("describe-db-engine-versions").count(),
        1,
        "one lookup per engine version, not per instance: {calls}"
    );
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
            &format!(
                "CREATE TABLE {table} (id int); INSERT INTO {table} (id) VALUES (1), (3); \
                 CREATE TABLE {table}_text (id text); \
                 INSERT INTO {table}_text (id) VALUES ('1.2'), ('1.10'), (NULL)"
            ),
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
            ..Default::default()
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

    // A text column of ids that aren't integers: the highest shown as
    // written, not an error.
    let p = probe(&format!("{table}_text"), &[]);
    assert_eq!(
        (p.schema_version, p.schema_label.as_deref(), &p.schema_error),
        (None, Some("1.10"), &None),
        "{p:?}"
    );
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

/// The schema fetch learns each partition's parent from a real server,
/// sub-partitions included.
#[test]
fn schema_fetch_reads_partition_parents() {
    let home = scratch_home("partitions", WRITABLE_PROFILE);
    let suffix = std::process::id();
    let sql = format!(
        "CREATE TABLE pgman_it_ev_{suffix} (id int, at date) PARTITION BY RANGE (at); \
         CREATE TABLE pgman_it_ev_{suffix}_2024 PARTITION OF pgman_it_ev_{suffix} \
           FOR VALUES FROM ('2024-01-01') TO ('2025-01-01') PARTITION BY RANGE (at); \
         CREATE TABLE pgman_it_ev_{suffix}_2024_01 PARTITION OF pgman_it_ev_{suffix}_2024 \
           FOR VALUES FROM ('2024-01-01') TO ('2024-02-01')"
    );
    let out = batch_with_home(&home, &["--yes", "--sql", &sql]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rt = tokio::runtime::Runtime::new().unwrap();
    let cache = rt.block_on(async {
        let client = pgman::conn::connect_via(pgman::conn::Dsn::parse(DSN).unwrap(), None, 5_000)
            .await
            .unwrap();
        pgman::query::schema::fetch(&client).await
    });
    let parent = |c: &str| {
        cache
            .partition_parent
            .get(&("public".to_string(), c.to_string()))
            .map(|(_, p)| p.clone())
    };
    let root = format!("pgman_it_ev_{suffix}");
    assert_eq!(parent(&format!("{root}_2024")), Some(root.clone()));
    assert_eq!(
        parent(&format!("{root}_2024_01")),
        Some(format!("{root}_2024"))
    );
    assert_eq!(parent(&root), None, "the table itself is no partition");
    assert_eq!(
        cache.partition_root("public", &format!("{root}_2024_01")),
        Some(("public".to_string(), root.clone()))
    );
}

/// `schema_lint` over MCP finds what the wizard finds.
#[test]
fn mcp_schema_lint_reports_a_table_without_a_primary_key() {
    let home = scratch_home("mcp-lint", WRITABLE_PROFILE);
    let table = format!("pgman_it_nopk_{}", std::process::id());
    let out = batch_with_home(
        &home,
        &["--yes", "--sql", &format!("CREATE TABLE {table} (x int)")],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::write(
        home.join(".config/pgman/connections.toml"),
        format!("[[connections]]\nname = \"local\"\nurl = \"{DSN}\"\n"),
    )
    .unwrap();
    let frames = mcp_session_env(
        &home,
        &home,
        &[],
        &[],
        &[tool_call(
            1,
            "schema_lint",
            serde_json::json!({"connection": "local"}),
        )],
    );
    let (text, is_error) = reply(&frames, 1);
    assert!(!is_error, "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["server_is_replica"], false);
    let hit = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["code"] == "LINT001" && f["object"].as_str().unwrap().ends_with(&table));
    assert!(hit, "{text}");
}

/// With no version table configured, a probe recognises the migration
/// tool from its own table: one database per tool, real table shapes.
#[test]
fn probes_recognise_common_migration_tools() {
    let home = scratch_home("migtools", WRITABLE_PROFILE);
    let pid = std::process::id();
    let tools: [(&str, &str); 5] = [
        (
            "flyway",
            "CREATE TABLE flyway_schema_history (installed_rank int PRIMARY KEY, version varchar(50), \
             description varchar(200), type varchar(20), script varchar(1000), checksum int, \
             installed_by varchar(100), installed_on timestamp DEFAULT now(), execution_time int, success boolean); \
             INSERT INTO flyway_schema_history (installed_rank, version, description, type, script, installed_by, execution_time, success) VALUES \
             (1, '1', 'init', 'SQL', 'V1__init.sql', 'me', 1, true), \
             (2, '1.1', 'users', 'SQL', 'V1_1__users.sql', 'me', 1, true), \
             (3, '1.2', 'orders', 'SQL', 'V1_2__orders.sql', 'me', 1, false)",
        ),
        (
            "django",
            "CREATE TABLE django_migrations (id serial PRIMARY KEY, app varchar(255), name varchar(255), applied timestamptz); \
             INSERT INTO django_migrations (app, name, applied) VALUES \
             ('auth', '0001_initial', now() - interval '1 day'), ('shop', '0007_orders', now())",
        ),
        (
            "prisma",
            "CREATE TABLE _prisma_migrations (id varchar(36) PRIMARY KEY, checksum varchar(64), finished_at timestamptz, \
             migration_name varchar(255), logs text, rolled_back_at timestamptz, started_at timestamptz DEFAULT now(), applied_steps_count int DEFAULT 0); \
             INSERT INTO _prisma_migrations (id, checksum, finished_at, migration_name, started_at) VALUES \
             ('a', 'x', now(), '20240101_init', now() - interval '1 hour')",
        ),
        (
            "rails",
            "CREATE TABLE schema_migrations (version varchar PRIMARY KEY); \
             INSERT INTO schema_migrations VALUES ('20231231000000'), ('20240101120000')",
        ),
        (
            "golang",
            "CREATE TABLE schema_migrations (version bigint NOT NULL PRIMARY KEY, dirty boolean NOT NULL); \
             INSERT INTO schema_migrations VALUES (12, true)",
        ),
    ];
    let migdir = home.join("db-migration");
    std::fs::create_dir_all(&migdir).unwrap();
    for f in [
        "V1__init.sql",
        "V1_1__users.sql",
        "V1_2__orders.sql",
        "V2__more.sql",
    ] {
        std::fs::write(migdir.join(f), "-- x\n").unwrap();
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut results = std::collections::HashMap::new();
    for (tool, setup) in tools {
        let db = format!("pgman_it_{tool}_{pid}");
        let _ = batch_with_home(&home, &["--yes", "--sql", &format!("CREATE DATABASE {db}")]);
        let dsn = DSN.replace("/pgman_test?", &format!("/{db}?"));
        let out = batch_with_home_dsn(&home, &dsn, &["--yes", "--sql", setup]);
        assert!(
            out.status.success(),
            "{tool}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let report = pgman::inventory::EnvReport {
            name: tool.into(),
            source: "test".into(),
            targets: vec![pgman::inventory::Target {
                database: db.clone(),
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
                    dsn: pgman::conn::Dsn::parse(&dsn).unwrap(),
                    secret: None,
                    password_env: None,
                }),
            }],
            error: None,
            probe: Some(pgman::inventory::ProbeSettings {
                version_table: None,
                migrations_dir: Some(migdir.display().to_string()),
            }),
        };
        let p = rt
            .block_on(pgman::inventory::probe::run(
                report.clone(),
                std::sync::Arc::new(|_| unreachable!()),
            ))
            .remove(0)
            .unwrap()
            .unwrap();
        results.insert(tool, p);
        if tool == "flyway" {
            // An environment that names its own version table, for other
            // databases only: no guessing here.
            let mut explicit = report.clone();
            explicit.probe = Some(pgman::inventory::ProbeSettings {
                version_table: Some(pgman::inventory::VersionTable {
                    table: "t_version".into(),
                    column: "id".into(),
                    patches: None,
                    databases: vec!["elsewhere".into()],
                }),
                migrations_dir: None,
            });
            let p = rt
                .block_on(pgman::inventory::probe::run(
                    explicit,
                    std::sync::Arc::new(|_| unreachable!()),
                ))
                .remove(0)
                .unwrap()
                .unwrap();
            assert_eq!(p.migration_tool, None, "configured, so not detected");
        }
        if tool == "django" {
            // A migrations_dir that can't be read says so on the row.
            let mut missing = report;
            missing.probe = Some(pgman::inventory::ProbeSettings {
                version_table: None,
                migrations_dir: Some("/nonexistent/pgman/migrations".into()),
            });
            let p = rt
                .block_on(pgman::inventory::probe::run(
                    missing,
                    std::sync::Arc::new(|_| unreachable!()),
                ))
                .remove(0)
                .unwrap()
                .unwrap();
            let e = p.schema_error.unwrap_or_default();
            assert!(
                e.starts_with("migrations_dir: cannot list /nonexistent"),
                "{e}"
            );
        }
    }
    let f = &results["flyway"];
    assert_eq!(
        (f.migration_tool.as_deref(), f.schema_label.as_deref()),
        (Some("flyway"), Some("1.1"))
    );
    assert_eq!(f.pending, Some(2), "1.2 failed and 2 never ran");
    assert_eq!(
        f.schema_error.as_deref(),
        Some("flyway migration 1.2 failed")
    );
    assert_eq!(
        (
            results["django"].schema_label.as_deref(),
            results["django"].migrations_applied
        ),
        (Some("shop.0007_orders"), Some(2))
    );
    assert_eq!(
        results["prisma"].schema_label.as_deref(),
        Some("20240101_init")
    );
    let r = &results["rails"];
    assert_eq!(
        (r.migration_tool.as_deref(), r.schema_label.as_deref()),
        (Some("migrations"), Some("20240101120000"))
    );
    assert_eq!(r.schema_error, None, "Rails has no dirty column");
    let g = &results["golang"];
    assert_eq!(
        (g.schema_label.as_deref(), g.schema_error.as_deref()),
        (
            Some("12"),
            Some("schema_migrations: migration 12 (dirty) failed")
        )
    );
}

/// A session waiting on another's lock counts as blocked in a probe.
#[test]
fn a_probe_counts_a_blocked_session() {
    let home = scratch_home("blocked", WRITABLE_PROFILE);
    let table = format!("pgman_it_lock_{}", std::process::id());
    let out = batch_with_home(
        &home,
        &["--yes", "--sql", &format!("CREATE TABLE {table} (x int)")],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let connect = || async {
            let (c, conn) = tokio_postgres::connect(DSN, tokio_postgres::NoTls)
                .await
                .unwrap();
            tokio::spawn(conn);
            c
        };
        let holder = connect().await;
        let waiter = connect().await;
        holder
            .batch_execute(&format!(
                "BEGIN; LOCK TABLE {table} IN ACCESS EXCLUSIVE MODE"
            ))
            .await
            .unwrap();
        let blocked_query = format!("SELECT count(*) FROM {table}");
        let waiting = tokio::spawn(async move { waiter.simple_query(&blocked_query).await });
        // Let the waiter reach the lock.
        let probe_client =
            pgman::conn::connect_via(pgman::conn::Dsn::parse(DSN).unwrap(), None, 5_000)
                .await
                .unwrap();
        let mut blocked = 0;
        for _ in 0..50 {
            let p =
                pgman::query::probe::run(&probe_client, pgman::query::probe::SchemaSource::None)
                    .await
                    .unwrap();
            blocked = p.blocked.unwrap_or(0);
            if blocked > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(blocked >= 1, "the waiting session counts as blocked");
        holder.batch_execute("ROLLBACK").await.unwrap();
        waiting.await.unwrap().unwrap();
    });
}

/// The unused-index and bloat checks find a real index nothing scans and
/// a table whose dead rows passed the threshold.
#[test]
fn schema_lint_finds_an_unused_index_and_a_bloated_table() {
    let home = scratch_home("lint-stats", WRITABLE_PROFILE);
    let t = format!("pgman_it_bloat_{}", std::process::id());
    let setup = format!(
        "CREATE TABLE {t} (id int PRIMARY KEY, tag text) WITH (autovacuum_enabled = false); \
         CREATE INDEX {t}_tag ON {t} (tag); \
         INSERT INTO {t} SELECT g, 'x' || g FROM generate_series(1, 3000) g; \
         DELETE FROM {t} WHERE id <= 1000"
    );
    let out = batch_with_home(&home, &["--yes", "--sql", &setup]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::write(
        home.join(".config/pgman/connections.toml"),
        format!("[[connections]]\nname = \"local\"\nurl = \"{DSN}\"\n"),
    )
    .unwrap();
    // Statistics reach pg_stat_* shortly after the writing session ends.
    let mut last = String::new();
    for _ in 0..30 {
        let frames = mcp_session_env(
            &home,
            &home,
            &[],
            &[],
            &[tool_call(
                1,
                "schema_lint",
                serde_json::json!({"connection": "local"}),
            )],
        );
        let (text, is_error) = reply(&frames, 1);
        assert!(!is_error, "{text}");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let has =
            |code: &str, needle: &str| {
                v["findings"].as_array().unwrap().iter().any(|f| {
                    f["code"] == code && f["object"].as_str().unwrap_or("").contains(needle)
                })
            };
        if has("LINT102", &format!("{t}_tag")) && has("LINT104", &t) {
            return;
        }
        last = text;
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    panic!("LINT102 on {t}_tag and LINT104 on {t} expected: {last}");
}

/// `environment_logs` reads a listed server's log errors and refuses a
/// server the environment doesn't list.
#[cfg(unix)]
#[test]
fn mcp_environment_logs_reads_only_listed_servers() {
    let home = scratch_home("mcp-logs", WRITABLE_PROFILE);
    std::fs::write(
        home.join(".config/pgman/environments.toml"),
        "[[environments]]\nname = \"demo\"\nsource = { kind = \"rds\", instances = [\"demo\"], region = \"us-west-1\", account = \"210987654321\" }\n",
    )
    .unwrap();
    let rds = home.join("rds.json");
    std::fs::write(
        &rds,
        r#"{"DBInstances": [{"DBInstanceIdentifier": "demo", "Engine": "postgres", "EngineVersion": "16.10",
          "DBInstanceStatus": "available", "DBInstanceClass": "db.t4g.medium",
          "Endpoint": {"Address": "demo.x.rds.amazonaws.com", "Port": 5432}, "DBName": "ledger"}]}"#,
    )
    .unwrap();
    let (path, log) = fake_aws(&home);
    let frames = mcp_session_env(
        &home,
        &home,
        &[],
        &[
            ("PATH", path),
            ("FAKE_AWS_ACCOUNT", "210987654321".into()),
            ("FAKE_RDS_JSON", rds.clone().into()),
        ],
        &[
            tool_call(
                1,
                "environment_logs",
                serde_json::json!({"environment": "demo", "server": "demo"}),
            ),
            tool_call(
                2,
                "environment_logs",
                serde_json::json!({"environment": "demo", "server": "production"}),
            ),
        ],
    );
    let (text, is_error) = reply(&frames, 1);
    assert!(!is_error, "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        v["errors"][0]["message"],
        "permission denied for database rdsadmin"
    );
    assert!(
        !text.contains("select secret"),
        "STATEMENT lines never surface: {text}"
    );
    let (text, is_error) = reply(&frames, 2);
    assert!(
        is_error && text.contains("isn't one of demo's servers"),
        "{text}"
    );
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        !calls.contains("--db-instance-identifier production"),
        "an unlisted server's logs are never asked for: {calls}"
    );
}

/// Applying a patch: it runs and records itself; a failing statement
/// rolls the whole patch back; a locked table fails fast.
#[test]
fn applying_a_patch_runs_records_rolls_back_and_times_out() {
    let home = scratch_home("apply", WRITABLE_PROFILE);
    let pid = std::process::id();
    let vt = format!("pgman_it_dbv_{pid}");
    let tbl = format!("pgman_it_target_{pid}");
    let out = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            &format!("CREATE TABLE {vt} (scriptid int); INSERT INTO {vt} VALUES (1); CREATE TABLE {tbl} (id int)"),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cfg = pgman::query::patches::PatchesConfig {
        dir: String::new(),
        table: vt.clone(),
        column: "scriptid".into(),
    };
    let mut writable = pgman::safety::SafetyConfig::default();
    writable.default.read_only = false;
    let patch = |id: i64, body: &str| {
        let text = format!("INSERT INTO {vt} (scriptid) VALUES ({id});\n{body}");
        (
            pgman::query::patches::patch_file(&format!("{id:03}-x.sql"), &text, &cfg),
            text,
        )
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let connect = || async {
            let (notice_tx, _n) = tokio::sync::mpsc::unbounded_channel();
            let (notification_tx, _m) = tokio::sync::mpsc::unbounded_channel();
            pgman::conn::connect_only(
                pgman::conn::Dsn::parse(DSN).unwrap(),
                false,
                30_000,
                notice_tx,
                notification_tx,
            )
            .await
            .unwrap()
            .0
        };
        let client = connect().await;

        // 2: applies and records itself.
        let (p2, sql2) = patch(2, &format!("ALTER TABLE {tbl} ADD COLUMN c int;"));
        let stmts = pgman::query::apply::check_patch(&writable, "pgman_test", &sql2, false)
            .unwrap()
            .statements;
        let done = pgman::query::apply::apply(&client, &cfg, &p2, &stmts)
            .await
            .unwrap();
        assert!(done.recorded, "{done:?}");
        let applied = pgman::query::apply::applied_ids(&client, &cfg)
            .await
            .unwrap();
        assert!(applied.contains(&2));
        let next =
            pgman::query::apply::next_patch(&[p2.clone(), patch(3, "SELECT 1;").0], &applied);
        assert_eq!(next.map(|p| p.file), Some("003-x.sql".to_string()));

        // 3: the second statement fails, so its version row is rolled back too.
        let (p3, sql3) = patch(3, "SELECT * FROM pgman_it_no_such_table;");
        let stmts = pgman::query::apply::check_patch(&writable, "pgman_test", &sql3, false)
            .unwrap()
            .statements;
        let err = pgman::query::apply::apply(&client, &cfg, &p3, &stmts)
            .await
            .unwrap_err();
        assert!(
            err.contains("statement 2 of 2 failed, nothing applied"),
            "{err}"
        );
        let applied = pgman::query::apply::applied_ids(&client, &cfg)
            .await
            .unwrap();
        assert!(!applied.contains(&3), "rolled back");

        // 4: a table someone else holds: fails on the lock timeout, not a hang.
        let holder = connect().await;
        holder
            .batch_execute(&format!("BEGIN; LOCK TABLE {tbl} IN ACCESS EXCLUSIVE MODE"))
            .await
            .unwrap();
        let (p4, sql4) = patch(4, &format!("ALTER TABLE {tbl} ADD COLUMN d int;"));
        let stmts = pgman::query::apply::check_patch(&writable, "pgman_test", &sql4, false)
            .unwrap()
            .statements;
        let started = std::time::Instant::now();
        let err = pgman::query::apply::apply(&client, &cfg, &p4, &stmts)
            .await
            .unwrap_err();
        assert!(err.contains("lock timeout"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(15));
        holder.batch_execute("ROLLBACK").await.unwrap();
        assert!(!pgman::query::apply::applied_ids(&client, &cfg)
            .await
            .unwrap()
            .contains(&4));
    });
}

/// An environment database's pending patches: planned read-only with
/// their checks, then applied one transaction each, stopping at the first
/// failure, and refusing a plan the database has moved past.
#[test]
fn environment_patches_plan_then_apply_in_order_and_stop_at_a_failure() {
    let home = scratch_home("env-patch", WRITABLE_PROFILE);
    let pid = std::process::id();
    let vt = format!("pgman_it_pv_{pid}");
    let tbl = format!("pgman_it_pt_{pid}");
    let out = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            &format!("CREATE TABLE {vt} (id int); INSERT INTO {vt} VALUES (1); CREATE TABLE {tbl} (id int)"),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let dir = home.join("patches");
    std::fs::create_dir_all(&dir).unwrap();
    let row = |id: i64| format!("INSERT INTO {vt} (id) VALUES ({id});\n");
    for (name, body) in [
        ("001-old.sql", row(1)),
        (
            "002-col.sql",
            format!("{}ALTER TABLE {tbl} ADD COLUMN c int;\n", row(2)),
        ),
        (
            "003-bad.sql",
            format!("{}SELECT * FROM pgman_it_no_such_table;\n", row(3)),
        ),
        (
            "004-col.sql",
            format!("{}ALTER TABLE {tbl} ADD COLUMN d int;\n", row(4)),
        ),
        (
            "005-idx.sql",
            format!(
                "{}CREATE INDEX CONCURRENTLY {tbl}_c ON {tbl} (c);\n",
                row(5)
            ),
        ),
    ] {
        std::fs::write(dir.join(name), body).unwrap();
    }
    let mut env = pgman::inventory::parse_config(&format!(
        "[[environments]]\nname = \"it-prod\"\nsource = {{ kind = \"static\", connections = [\"local\"] }}\n\
         version_table = {{ table = \"{vt}\", column = \"id\", patches = \"{}\" }}\n",
        dir.display()
    ))
    .unwrap()
    .remove(0);
    let target = pgman::inventory::Target {
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
    };
    let mut writable = pgman::safety::SafetyConfig::default();
    writable.default.read_only = false;
    let no_secrets: pgman::inventory::patching::FetchSecret =
        std::sync::Arc::new(|_| Err("no secrets in this test".into()));
    use pgman::inventory::patching::{apply, plan};
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let p = plan(&env, &target, &writable, &no_secrets, false)
            .await
            .unwrap();
        assert!(p.protected, "named prod");
        assert_eq!(p.applied_max, Some(1));
        let files: Vec<&str> = p.patches.iter().map(|x| x.file.file.as_str()).collect();
        assert_eq!(files, ["002-col.sql", "003-bad.sql", "004-col.sql"]);
        let (at, why) = p.stopped_at.clone().unwrap();
        assert_eq!(at, "005-idx.sql");
        assert!(why.contains("CONCURRENTLY"), "{why}");

        // A read-only profile plans nothing.
        let ro = plan(
            &env,
            &target,
            &pgman::safety::SafetyConfig::default(),
            &no_secrets,
            false,
        )
        .await
        .unwrap();
        assert!(ro.patches.is_empty() && ro.stopped_at.unwrap().1.contains("read-only"));

        let r = apply(
            &env,
            &target,
            &writable,
            &no_secrets,
            &p,
            pgman::inventory::patching::RunMode::Apply,
            &Default::default(),
        )
        .await;
        assert_eq!(r.error, None);
        assert_eq!(r.results.len(), 2, "stops at 003: {r:?}");
        assert_eq!(r.results[0].outcome, Ok(2));
        assert!(r.results[1]
            .outcome
            .as_ref()
            .unwrap_err()
            .contains("nothing applied"));
        assert!(
            r.summary()
                .starts_with("applied 1, then 003-bad.sql failed"),
            "{}",
            r.summary()
        );
        assert_eq!(r.recorded_after, Some(2), "002 recorded, 003 rolled back");

        // Fix 003 on disk and plan again.
        std::fs::write(dir.join("003-bad.sql"), row(3)).unwrap();
        let p2 = plan(&env, &target, &writable, &no_secrets, false)
            .await
            .unwrap();
        assert_eq!(p2.applied_max, Some(2));
        assert_eq!(p2.patches.len(), 2);

        // Someone else applies 004 meanwhile: the plan is refused before 003 runs.
        let out = batch_with_home(
            &home,
            &["--yes", "--sql", &format!("INSERT INTO {vt} VALUES (4)")],
        );
        assert!(out.status.success());
        let r = apply(
            &env,
            &target,
            &writable,
            &no_secrets,
            &p2,
            pgman::inventory::patching::RunMode::Apply,
            &Default::default(),
        )
        .await;
        let err = r.results[0].outcome.as_ref().unwrap_err();
        assert!(
            err.contains("004-col.sql was applied by someone else"),
            "{err}"
        );

        // Now 003 is a gap below the recorded 4: planned again, it's left
        // out and named, not run after 004.
        let gap = plan(&env, &target, &writable, &no_secrets, false)
            .await
            .unwrap();
        assert!(gap.patches.is_empty(), "{:?}", gap.patches);
        assert_eq!(gap.below_recorded, ["003-bad.sql"]);
        let out = batch_with_home(
            &home,
            &["--yes", "--sql", &format!("DELETE FROM {vt} WHERE id = 4")],
        );
        assert!(out.status.success());

        // A replica is refused outright.
        let mut replica = target.clone();
        replica.role = pgman::mcp::connections::Role::Replica;
        assert!(plan(&env, &replica, &writable, &no_secrets, false)
            .await
            .unwrap_err()
            .contains("replica"));
        env.protected = Some(false);
        assert!(
            !plan(&env, &target, &writable, &no_secrets, false)
                .await
                .unwrap()
                .protected
        );

        // A plan whose statements aren't its text's is refused.
        let mut fresh = plan(&env, &target, &writable, &no_secrets, false)
            .await
            .unwrap();
        assert_eq!(fresh.patches[0].file.file, "003-bad.sql");
        fresh.patches[0].statements.push("SELECT 1".into());
        let r = apply(
            &env,
            &target,
            &writable,
            &no_secrets,
            &fresh,
            pgman::inventory::patching::RunMode::Apply,
            &Default::default(),
        )
        .await;
        let err = r.results[0].outcome.as_ref().unwrap_err();
        assert!(err.contains("differ from the plan"), "{err}");

        // A patch that runs but doesn't record its own id stops the run.
        let mut fresh = plan(&env, &target, &writable, &no_secrets, false)
            .await
            .unwrap();
        let wrong = format!("INSERT INTO {vt} (id) VALUES (33);");
        fresh.patches[0].text = wrong.clone();
        fresh.patches[0].statements = vec![wrong.trim_end_matches(';').to_string()];
        let r = apply(
            &env,
            &target,
            &writable,
            &no_secrets,
            &fresh,
            pgman::inventory::patching::RunMode::Apply,
            &Default::default(),
        )
        .await;
        let err = r.results[0].outcome.as_ref().unwrap_err();
        assert!(err.contains("recorded no version row 3"), "{err}");
        assert_eq!(r.results.len(), 1, "and nothing after it runs");
        // (It did record 33, which would make every file a gap below it.)
        let out = batch_with_home(
            &home,
            &["--yes", "--sql", &format!("DELETE FROM {vt} WHERE id = 33")],
        );
        assert!(out.status.success());

        // Two files with one id, or a name that disagrees with its id: fix the files first.
        std::fs::write(dir.join("003-dup.sql"), row(3)).unwrap();
        let (at, why) = plan(&env, &target, &writable, &no_secrets, false)
            .await
            .unwrap()
            .stopped_at
            .unwrap();
        assert!(
            at.starts_with("003-") && why.contains("records version 3 too"),
            "{why}"
        );
        std::fs::remove_file(dir.join("003-dup.sql")).unwrap();
        // (Without 005, which would sort first and stop on CONCURRENTLY.)
        std::fs::remove_file(dir.join("005-idx.sql")).unwrap();
        std::fs::write(dir.join("003-bad.sql"), row(7)).unwrap();
        let (at, why) = plan(&env, &target, &writable, &no_secrets, false)
            .await
            .unwrap()
            .stopped_at
            .unwrap();
        assert_eq!(at, "003-bad.sql");
        assert!(
            why.contains("its name says 3 but it records version 7"),
            "{why}"
        );
    });
}

/// A patch that needs no transaction is refused until planned outside
/// one; then it runs statement by statement, and a failure part-way says
/// what stayed applied.
#[test]
fn a_patch_planned_outside_a_transaction_runs_vacuum_and_concurrently() {
    let home = scratch_home("env-patch-outside", WRITABLE_PROFILE);
    let pid = std::process::id();
    let vt = format!("pgman_it_ov_{pid}");
    let tbl = format!("pgman_it_ot_{pid}");
    let out = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            &format!("CREATE TABLE {vt} (id int); INSERT INTO {vt} VALUES (1); CREATE TABLE {tbl} (id int); INSERT INTO {tbl} VALUES (1), (1)"),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let dir = home.join("patches");
    std::fs::create_dir_all(&dir).unwrap();
    let row = |id: i64| format!("INSERT INTO {vt} (id) VALUES ({id});\n");
    for (name, body) in [
        (
            "002-col.sql",
            // Fails first: an ordinary patch in an outside plan still
            // runs in its own transaction, so this rolls back whole.
            format!(
                "{}ALTER TABLE {tbl} ADD COLUMN c int;\nSELECT * FROM pgman_it_no_such_table;\n",
                row(2)
            ),
        ),
        ("003-vac.sql", format!("{}VACUUM ANALYZE {tbl};\n", row(3))),
        (
            "004-idx.sql",
            format!(
                // The unique one fails on the duplicate ids, leaving it INVALID.
                "{}CREATE INDEX CONCURRENTLY {tbl}_c ON {tbl} (c);\n\
                 CREATE UNIQUE INDEX CONCURRENTLY {tbl}_u ON {tbl} (id);\n",
                row(4)
            ),
        ),
    ] {
        std::fs::write(dir.join(name), body).unwrap();
    }
    let env = pgman::inventory::parse_config(&format!(
        "[[environments]]\nname = \"it-outside\"\nsource = {{ kind = \"static\", connections = [\"local\"] }}\n\
         version_table = {{ table = \"{vt}\", column = \"id\", patches = \"{}\" }}\n",
        dir.display()
    ))
    .unwrap()
    .remove(0);
    let target = pgman::inventory::Target {
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
    };
    let mut writable = pgman::safety::SafetyConfig::default();
    writable.default.read_only = false;
    let no_secrets: pgman::inventory::patching::FetchSecret =
        std::sync::Arc::new(|_| Err("no secrets in this test".into()));
    use pgman::inventory::patching::{apply, plan};
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let inside = plan(&env, &target, &writable, &no_secrets, false)
            .await
            .unwrap();
        let files: Vec<&str> = inside
            .patches
            .iter()
            .map(|x| x.file.file.as_str())
            .collect();
        assert_eq!(files, ["002-col.sql"]);
        let (at, why) = inside.stopped_at.clone().unwrap();
        assert!(at == "003-vac.sql" && why.contains("VACUUM"), "{why}");
        assert!(inside.outside_would_help);

        let p = plan(&env, &target, &writable, &no_secrets, true)
            .await
            .unwrap();
        let shape: Vec<(&str, Option<&str>)> = p
            .patches
            .iter()
            .map(|x| (x.file.file.as_str(), x.outside))
            .collect();
        assert_eq!(
            shape,
            [
                ("002-col.sql", None),
                ("003-vac.sql", Some("VACUUM")),
                ("004-idx.sql", Some("CONCURRENTLY")),
            ]
        );
        assert!(p.needs_typed() && !p.protected && !p.outside_would_help);
        let r = apply(
            &env,
            &target,
            &writable,
            &no_secrets,
            &p,
            pgman::inventory::patching::RunMode::Apply,
            &Default::default(),
        )
        .await;
        let err = r.results[0].outcome.as_ref().unwrap_err();
        assert!(err.contains("nothing applied"), "{err}");
        assert_eq!(r.recorded_after, Some(1), "002 rolled back, row and all");
        std::fs::write(
            dir.join("002-col.sql"),
            format!("{}ALTER TABLE {tbl} ADD COLUMN c int;\n", row(2)),
        )
        .unwrap();

        // A plan made outside can't be run as one made inside.
        let p = plan(&env, &target, &writable, &no_secrets, true)
            .await
            .unwrap();
        let mut forged = p.clone();
        forged.outside_transaction = false;
        let r = apply(
            &env,
            &target,
            &writable,
            &no_secrets,
            &forged,
            pgman::inventory::patching::RunMode::Apply,
            &Default::default(),
        )
        .await;
        let err = r.results[1].outcome.as_ref().unwrap_err();
        assert!(err.contains("can't run in a transaction"), "{err}");

        // 002 ran in a transaction above; plan again from where it is.
        let p = plan(&env, &target, &writable, &no_secrets, true)
            .await
            .unwrap();
        // Nor run with a patch's no-transaction mark taken off.
        let mut forged = p.clone();
        forged.patches[0].outside = None;
        let r = apply(
            &env,
            &target,
            &writable,
            &no_secrets,
            &forged,
            pgman::inventory::patching::RunMode::Apply,
            &Default::default(),
        )
        .await;
        let err = r.results[0].outcome.as_ref().unwrap_err();
        assert!(err.contains("differ from the plan"), "{err}");
        let r = apply(
            &env,
            &target,
            &writable,
            &no_secrets,
            &p,
            pgman::inventory::patching::RunMode::Apply,
            &Default::default(),
        )
        .await;
        assert_eq!(r.error, None);
        assert_eq!(r.results[0].outcome, Ok(2), "VACUUM ran: {r:?}");
        let err = r.results[1].outcome.as_ref().unwrap_err();
        assert!(
            err.starts_with("statement 3 of 3 failed outside a transaction")
                && err.contains("2 earlier statement(s) stay applied")
                && err.contains("version 4 IS recorded")
                && err.contains("leaves an INVALID index"),
            "{err}"
        );
        assert_eq!(r.recorded_after, Some(4));
        let out = batch_with_home(
            &home,
            &[
                "--format",
                "csv",
                "--sql",
                &format!(
                    "SELECT c.relname, i.indisvalid FROM pg_index i \
                     JOIN pg_class c ON c.oid = i.indexrelid \
                     WHERE i.indrelid = '{tbl}'::regclass ORDER BY 1"
                ),
            ],
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(&format!("{tbl}_c,t")) && stdout.contains(&format!("{tbl}_u,f")),
            "the first index stayed, the failed one is INVALID: {stdout}"
        );
    });
}

/// Outside a transaction a statement runs with no statement timeout —
/// a long concurrent build cut short is left INVALID — and the session's
/// own timeout is back afterwards, for the patches after it.
#[test]
fn outside_a_transaction_lifts_the_statement_timeout_then_restores_it() {
    let home = scratch_home("apply-outside-timeout", WRITABLE_PROFILE);
    let vt = format!("pgman_it_otv_{}", std::process::id());
    let out = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            &format!("CREATE TABLE {vt} (id int); INSERT INTO {vt} VALUES (1)"),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cfg = pgman::query::patches::PatchesConfig {
        dir: String::new(),
        table: vt.clone(),
        column: "id".into(),
    };
    let text = format!("INSERT INTO {vt} (id) VALUES (2);\nSELECT pg_sleep(1.5);");
    let file = pgman::query::patches::patch_file("002-x.sql", &text, &cfg);
    let statements = vec![
        format!("INSERT INTO {vt} (id) VALUES (2)"),
        "SELECT pg_sleep(1.5)".to_string(),
    ];
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (notice_tx, _n) = tokio::sync::mpsc::unbounded_channel();
        let (notification_tx, _m) = tokio::sync::mpsc::unbounded_channel();
        // The session's own limit is shorter than the statement.
        let client = pgman::conn::connect_only(
            pgman::conn::Dsn::parse(DSN).unwrap(),
            false,
            500,
            notice_tx,
            notification_tx,
        )
        .await
        .unwrap()
        .0;
        let done = pgman::query::apply::apply_outside(&client, &cfg, &file, &statements)
            .await
            .expect("ran past the session's 500 ms");
        assert!(done.recorded);
        let timeout: String = client
            .query_one("SHOW statement_timeout", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(timeout, "500ms", "restored for the next patch");
        // And restored after a failure too.
        let bad = vec!["SELECT * FROM pgman_it_no_such_table".to_string()];
        assert!(
            pgman::query::apply::apply_outside(&client, &cfg, &file, &bad)
                .await
                .is_err()
        );
        let timeout: String = client
            .query_one("SHOW statement_timeout", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(timeout, "500ms");
    });
}

/// A cancel during a run stops the running statement on the server:
/// outside a transaction what ran stays, inside one the patch rolls back.
#[test]
fn cancelling_a_run_stops_the_running_statement() {
    let home = scratch_home("env-patch-cancel", WRITABLE_PROFILE);
    let pid = std::process::id();
    let vt = format!("pgman_it_cv_{pid}");
    let tbl = format!("pgman_it_ct_{pid}");
    let out = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            &format!("CREATE TABLE {vt} (id int); INSERT INTO {vt} VALUES (1); CREATE TABLE {tbl} (id int)"),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let dir = home.join("patches");
    std::fs::create_dir_all(&dir).unwrap();
    let row = |id: i64| format!("INSERT INTO {vt} (id) VALUES ({id});\n");
    std::fs::write(
        dir.join("002-vac.sql"),
        format!("{}VACUUM {tbl};\nSELECT pg_sleep(60);\n", row(2)),
    )
    .unwrap();
    std::fs::write(
        dir.join("003-slow.sql"),
        format!("{}SELECT pg_sleep(60);\n", row(3)),
    )
    .unwrap();
    let env = pgman::inventory::parse_config(&format!(
        "[[environments]]\nname = \"it-cancel\"\nsource = {{ kind = \"static\", connections = [\"local\"] }}\n\
         version_table = {{ table = \"{vt}\", column = \"id\", patches = \"{}\" }}\n",
        dir.display()
    ))
    .unwrap()
    .remove(0);
    let target = pgman::inventory::Target {
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
    };
    let mut writable = pgman::safety::SafetyConfig::default();
    writable.default.read_only = false;
    let no_secrets: pgman::inventory::patching::FetchSecret =
        std::sync::Arc::new(|_| Err("no secrets in this test".into()));
    use pgman::inventory::patching::{apply, plan, CancelSlot};
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        // Cancels once the run has connected and is into its sleep.
        let run = |p: pgman::inventory::patching::PatchPlan| {
            let (env, target, writable, no_secrets) = (
                env.clone(),
                target.clone(),
                writable.clone(),
                no_secrets.clone(),
            );
            async move {
                let slot = CancelSlot::default();
                let started = std::time::Instant::now();
                let running = {
                    let slot = slot.clone();
                    tokio::spawn(async move {
                        apply(
                            &env,
                            &target,
                            &writable,
                            &no_secrets,
                            &p,
                            pgman::inventory::patching::RunMode::Apply,
                            &slot,
                        )
                        .await
                    })
                };
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                assert!(slot.cancel(), "connected within 2 s");
                let report = running.await.unwrap();
                assert!(
                    started.elapsed() < std::time::Duration::from_secs(20),
                    "stopped well before the 60 s sleep"
                );
                assert!(!slot.cancel(), "emptied once the run is over");
                report
            }
        };

        let p = plan(&env, &target, &writable, &no_secrets, true)
            .await
            .unwrap();
        assert_eq!(p.patches.len(), 2);
        // Cancelled before it connected: it starts nothing.
        let early = CancelSlot::default();
        assert!(!early.cancel());
        let r = apply(
            &env,
            &target,
            &writable,
            &no_secrets,
            &p,
            pgman::inventory::patching::RunMode::Apply,
            &early,
        )
        .await;
        assert_eq!(r.results.len(), 1);
        assert!(r.results[0]
            .outcome
            .as_ref()
            .unwrap_err()
            .starts_with("cancelled before it started"));
        assert_eq!(r.recorded_after, Some(1), "nothing ran");
        let r = run(p).await;
        let err = r.results[0].outcome.as_ref().unwrap_err();
        assert!(
            err.contains("canceling statement due to user request")
                && err.contains("version 2 IS recorded"),
            "outside: what ran stays: {err}"
        );
        assert_eq!(r.results.len(), 1, "and nothing after it runs");

        let p = plan(&env, &target, &writable, &no_secrets, true)
            .await
            .unwrap();
        assert_eq!(p.patches[0].file.file, "003-slow.sql");
        let r = run(p).await;
        let err = r.results[0].outcome.as_ref().unwrap_err();
        assert!(
            err.contains("nothing applied") && err.contains("canceling statement"),
            "inside: rolled back: {err}"
        );
        assert_eq!(r.recorded_after, Some(2));
    });
}

/// The local test server as an environment's only database, `db`, with
/// its version table `vt` and patches in `dir`.
fn local_env(
    name: &str,
    db: &str,
    vt: &str,
    dir: &std::path::Path,
) -> (pgman::inventory::Environment, pgman::inventory::Target) {
    let env = pgman::inventory::parse_config(&format!(
        "[[environments]]\nname = \"{name}\"\nsource = {{ kind = \"static\", connections = [\"local\"] }}\n\
         version_table = {{ table = \"{vt}\", column = \"id\", patches = \"{}\" }}\n",
        dir.display()
    ))
    .unwrap()
    .remove(0);
    let mut dsn = pgman::conn::Dsn::parse(DSN).unwrap();
    dsn.dbname = db.to_string();
    let target = pgman::inventory::Target {
        database: db.into(),
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
            dsn,
            secret: None,
            password_env: None,
        }),
    };
    (env, target)
}

/// A plain connection to `db` on the local test server.
async fn raw(db: &str) -> tokio_postgres::Client {
    let dsn = DSN.replace("/pgman_test?", &format!("/{db}?"));
    let (c, conn) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(conn);
    c
}

/// A dry run runs every patch in one transaction — a later patch sees
/// what an earlier one did — and rolls it all back: nothing recorded,
/// nothing changed, and a real run afterwards goes through. A patch
/// that can't run in a transaction ends it.
#[test]
fn a_dry_run_runs_every_patch_then_rolls_it_all_back() {
    let home = scratch_home("env-patch-dry", WRITABLE_PROFILE);
    let pid = std::process::id();
    let vt = format!("pgman_it_dv_{pid}");
    let tbl = format!("pgman_it_dt_{pid}");
    let dir = home.join("patches");
    std::fs::create_dir_all(&dir).unwrap();
    let row = |id: i64| format!("INSERT INTO {vt} (id) VALUES ({id});\n");
    let write = |name: &str, body: String| std::fs::write(dir.join(name), body).unwrap();
    write(
        "002-col.sql",
        format!("{}ALTER TABLE {tbl} ADD COLUMN c int;\n", row(2)),
    );
    // Only works after 002: the dry run holds one transaction throughout.
    write(
        "003-default.sql",
        format!(
            "{}ALTER TABLE {tbl} ALTER COLUMN c SET DEFAULT 7;\n",
            row(3)
        ),
    );
    write(
        "004-bad.sql",
        format!("{}SELECT * FROM pgman_it_no_such_table;\n", row(4)),
    );
    let (env, target) = local_env("it-dry", "pgman_test", &vt, &dir);
    let mut writable = pgman::safety::SafetyConfig::default();
    writable.default.read_only = false;
    let no_secrets: pgman::inventory::patching::FetchSecret =
        std::sync::Arc::new(|_| Err("no secrets in this test".into()));
    use pgman::inventory::patching::{apply, plan, RunMode};
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let c = raw("pgman_test").await;
        c.batch_execute(&format!(
            "CREATE TABLE {vt} (id int); INSERT INTO {vt} VALUES (1); CREATE TABLE {tbl} (id int)"
        ))
        .await
        .unwrap();
        let columns = || async {
            c.query_one(
                "SELECT count(*) FROM information_schema.columns WHERE table_name = $1",
                &[&tbl],
            )
            .await
            .unwrap()
            .get::<_, i64>(0)
        };
        let p = plan(&env, &target, &writable, &no_secrets, false)
            .await
            .unwrap();
        assert_eq!(p.patches.len(), 3);
        let r = apply(
            &env,
            &target,
            &writable,
            &no_secrets,
            &p,
            RunMode::DryRun,
            &Default::default(),
        )
        .await;
        assert_eq!(r.error, None);
        assert_eq!(r.results[0].outcome, Ok(2));
        assert_eq!(r.results[1].outcome, Ok(2), "003 saw 002's column");
        let err = r.results[2].outcome.as_ref().unwrap_err();
        assert!(err.contains("pgman_it_no_such_table"), "{err}");
        assert_eq!(r.recorded_after, Some(1), "all rolled back: {r:?}");
        assert_eq!(columns().await, 1, "the column went with it");
        assert!(r
            .summary()
            .starts_with("dry run: ran 2, then 004-bad.sql failed"));

        // A patch with no transaction ends the dry run before it starts.
        write("004-bad.sql", format!("{}VACUUM {tbl};\n", row(4)));
        let p = plan(&env, &target, &writable, &no_secrets, true)
            .await
            .unwrap();
        assert_eq!(p.patches[2].outside, Some("VACUUM"));
        let r = apply(
            &env,
            &target,
            &writable,
            &no_secrets,
            &p,
            RunMode::DryRun,
            &Default::default(),
        )
        .await;
        assert_eq!(r.results.len(), 3);
        assert_eq!(
            r.results[2].outcome,
            Err("VACUUM can't run in a transaction, so the dry run stops here".into())
        );
        assert_eq!(r.recorded_after, Some(1));
        assert_eq!(columns().await, 1);

        // Nothing was left behind: the same plan applies for real.
        let r = apply(
            &env,
            &target,
            &writable,
            &no_secrets,
            &p,
            RunMode::Apply,
            &Default::default(),
        )
        .await;
        assert_eq!(r.failed(), None, "{r:?}");
        assert_eq!(r.recorded_after, Some(4));
        assert_eq!(columns().await, 2);
    });
}

/// Applying to a copy patches a fresh template copy of the database for
/// real and drops it, leaving the database itself as it was; with anyone
/// else connected to the database it makes no copy at all.
#[test]
fn applying_to_a_copy_patches_the_copy_then_drops_it() {
    let home = scratch_home("env-patch-copy", WRITABLE_PROFILE);
    let pid = std::process::id();
    let src = format!("pgman_it_src_{pid}");
    let vt = "pgman_it_cv";
    let dir = home.join("patches");
    std::fs::create_dir_all(&dir).unwrap();
    for id in [2, 3] {
        std::fs::write(
            dir.join(format!("00{id}-t.sql")),
            format!("INSERT INTO {vt} (id) VALUES ({id});\nCREATE TABLE t{id} (id int);\n"),
        )
        .unwrap();
    }
    let (env, target) = local_env("it-copy", &src, vt, &dir);
    let mut writable = pgman::safety::SafetyConfig::default();
    writable.default.read_only = false;
    let no_secrets: pgman::inventory::patching::FetchSecret =
        std::sync::Arc::new(|_| Err("no secrets in this test".into()));
    use pgman::inventory::patching::{apply, plan, RunMode};
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let admin = raw("pgman_test").await;
        for sql in [
            format!("DROP DATABASE IF EXISTS {src}"),
            format!("CREATE DATABASE {src}"),
        ] {
            admin.batch_execute(&sql).await.unwrap();
        }
        let on_src = raw(&src).await;
        on_src
            .batch_execute(&format!(
                "CREATE TABLE {vt} (id int); INSERT INTO {vt} VALUES (1)"
            ))
            .await
            .unwrap();
        let p = plan(&env, &target, &writable, &no_secrets, false)
            .await
            .unwrap();
        assert_eq!(p.patches.len(), 2);
        // Connected to it: no copy.
        let r = apply(
            &env,
            &target,
            &writable,
            &no_secrets,
            &p,
            RunMode::Copy,
            &Default::default(),
        )
        .await;
        let err = r.error.clone().expect("refused");
        assert!(err.contains("needs nobody else connected"), "{err}");
        assert_eq!(r.copy, None);
        drop(on_src);
        // Wait for that session (and the plan's) to be gone.
        let others = || async {
            admin
                .query_one(
                    "SELECT count(*) FROM pg_stat_activity WHERE datname = $1",
                    &[&src],
                )
                .await
                .unwrap()
                .get::<_, i64>(0)
        };
        for _ in 0..40 {
            if others().await == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let r = apply(
            &env,
            &target,
            &writable,
            &no_secrets,
            &p,
            RunMode::Copy,
            &Default::default(),
        )
        .await;
        assert_eq!(r.error, None, "{r:?}");
        assert_eq!(r.failed(), None, "{r:?}");
        assert_eq!(r.recorded_after, Some(3), "the copy took both");
        let copy = r.copy.clone().expect("a copy was made");
        assert!(
            copy.name.starts_with(&format!("{src}_pgman_copy_")),
            "{copy:?}"
        );
        assert_eq!(copy.dropped, Ok(()));
        let left: i64 = admin
            .query_one(
                "SELECT count(*) FROM pg_database WHERE datname LIKE $1",
                &[&format!("{src}_pgman_copy_%")],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(left, 0, "the copy is gone");
        // The one DROP DATABASE pgman runs takes only a name it made.
        let err = pgman::query::apply::drop_copy(&admin, &src)
            .await
            .unwrap_err();
        assert!(err.contains("isn't a copy pgman made"), "{err}");
        let on_src = raw(&src).await;
        let max: i32 = on_src
            .query_one(&format!("SELECT max(id) FROM {vt}"), &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(max, 1, "the database itself is untouched");
        drop(on_src);
        for _ in 0..40 {
            if others().await == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        admin
            .batch_execute(&format!("DROP DATABASE IF EXISTS {src}"))
            .await
            .unwrap();
    });
}

/// `apply_patches` end to end over stdio: the operator is asked, a wrong
/// name applies nothing, the right one applies the pending patch.
#[test]
fn mcp_apply_patches_asks_the_operator_and_applies_on_the_typed_name() {
    use std::io::{BufRead, Write};
    let home = scratch_home("mcp-apply", WRITABLE_PROFILE);
    let pid = std::process::id();
    let vt = format!("pgman_it_mv_{pid}");
    let out = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            &format!("CREATE TABLE {vt} (id int); INSERT INTO {vt} VALUES (1)"),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let dir = home.join("patches");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("002-x.sql"),
        format!("INSERT INTO {vt} (id) VALUES (2);\n"),
    )
    .unwrap();
    std::fs::write(
        home.join(".config/pgman/connections.toml"),
        format!("[[connections]]\nname = \"local\"\nurl = \"{DSN}\"\n"),
    )
    .unwrap();
    std::fs::write(
        home.join(".config/pgman/environments.toml"),
        format!(
            "[[environments]]\nname = \"it-prod\"\nsource = {{ kind = \"static\", connections = [\"local\"] }}\n\
             version_table = {{ table = \"{vt}\", column = \"id\", patches = \"{}\" }}\n",
            dir.display()
        ),
    )
    .unwrap();
    let mut child = Command::new(pgman_binary())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .current_dir(&home)
        .args(["mcp", "serve"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = std::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let mut next =
        || -> serde_json::Value { serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap() };
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"capabilities":{{"elicitation":{{}}}}}}}}"#).unwrap();
    next();
    let args =
        serde_json::json!({"environment": "it-prod", "server": "local", "database": "pgman_test"});
    let mut call = |id: u64, answer: serde_json::Value| -> (String, bool, String) {
        writeln!(stdin, "{}", tool_call(id, "apply_patches", args.clone())).unwrap();
        let ask = next();
        assert_eq!(ask["method"], "elicitation/create", "{ask}");
        let message = ask["params"]["message"].as_str().unwrap().to_string();
        writeln!(
            stdin,
            "{}",
            serde_json::json!({"jsonrpc": "2.0", "id": ask["id"], "result": answer})
        )
        .unwrap();
        let (text, is_error) = reply(&[next()], id);
        (text, is_error, message)
    };
    let (text, is_error, message) = call(
        2,
        serde_json::json!({"action": "accept", "content": {"environment": "it-pro"}}),
    );
    assert!(
        message.contains("002-x.sql") && message.contains("type its name"),
        "{message}"
    );
    assert!(is_error && text.contains("doesn't match it-prod"), "{text}");
    assert!(!table_has_id(&home, &vt, 2), "a wrong name applies nothing");

    let (text, is_error, _) = call(
        3,
        serde_json::json!({"action": "accept", "content": {"environment": "it-prod"}}),
    );
    assert!(!is_error, "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["results"][0]["applied"], true, "{text}");
    assert!(table_has_id(&home, &vt, 2));

    // A decline is final: the next call is refused without asking.
    std::fs::write(
        dir.join("003-x.sql"),
        format!("INSERT INTO {vt} (id) VALUES (3);\n"),
    )
    .unwrap();
    let (text, is_error, _) = call(4, serde_json::json!({"action": "decline"}));
    assert!(is_error && text.contains("That answer is final"), "{text}");
    writeln!(stdin, "{}", tool_call(5, "apply_patches", args.clone())).unwrap();
    let frame = next();
    assert_eq!(frame["id"], 5, "a reply, not another question: {frame}");
    let (text, is_error) = reply(&[frame], 5);
    assert!(
        is_error && text.contains("declined patches for it-prod"),
        "{text}"
    );
    assert!(!table_has_id(&home, &vt, 3));
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

fn table_has_id(home: &std::path::Path, table: &str, id: i64) -> bool {
    let out = batch_with_home(
        home,
        &[
            "--sql",
            &format!("SELECT count(*) AS n FROM {table} WHERE id = {id}"),
        ],
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l.trim() == "1")
}

/// A statement the scanner reads as one can be several to a server whose
/// lexer a patch has changed (`standard_conforming_strings = off`): each
/// planned statement goes to the server alone, so the hidden `COMMIT` and
/// `DROP` never run. Found by the 2026-10-20 safety review.
#[test]
fn a_patch_cannot_smuggle_statements_past_the_scanner() {
    let home = scratch_home("apply-smuggle", WRITABLE_PROFILE);
    let pid = std::process::id();
    let vt = format!("pgman_it_sv_{pid}");
    let victim = format!("pgman_it_victim_{pid}");
    let out = batch_with_home(
        &home,
        &["--yes", "--sql", &format!("CREATE TABLE {vt} (id int); INSERT INTO {vt} VALUES (1); CREATE TABLE {victim} (id int)")],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cfg = pgman::query::patches::PatchesConfig {
        dir: String::new(),
        table: vt.clone(),
        column: "id".into(),
    };
    let text = format!(
        "INSERT INTO {vt} (id) VALUES (2);\nSET standard_conforming_strings = off;\nSELECT '\\''; COMMIT; DROP TABLE {victim} --';\n"
    );
    let patch = pgman::query::patches::patch_file("002-x.sql", &text, &cfg);
    let mut writable = pgman::safety::SafetyConfig::default();
    writable.default.read_only = false;
    // The check refuses it outright now…
    let refused =
        pgman::query::apply::check_patch(&writable, "pgman_test", &text, false).unwrap_err();
    assert!(refused.contains("SET"), "{refused}");
    // …and were a statement like it to get through, the server runs it alone.
    let statements: Vec<String> = vec![
        format!("INSERT INTO {vt} (id) VALUES (2)"),
        "SELECT set_config('standard_conforming_strings', 'off', true)".into(),
        format!("SELECT '\\''; COMMIT; DROP TABLE {victim} --'"),
    ];
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (notice_tx, _n) = tokio::sync::mpsc::unbounded_channel();
        let (notification_tx, _m) = tokio::sync::mpsc::unbounded_channel();
        let client = pgman::conn::connect_only(
            pgman::conn::Dsn::parse(DSN).unwrap(),
            false,
            30_000,
            notice_tx,
            notification_tx,
        )
        .await
        .unwrap()
        .0;
        let err = pgman::query::apply::apply(&client, &cfg, &patch, &statements)
            .await
            .unwrap_err();
        assert!(err.contains("statement 3 of 3 failed"), "{err}");
    });
    assert!(table_exists(&home, &victim), "the hidden DROP never ran");
    assert!(!table_has_id(&home, &vt, 2), "and the patch rolled back");
}

/// A standby of a primary that has gone quiet has no lag: it has
/// replayed everything it received. It used to read "time since the last
/// write", growing (and turning yellow) on every idle replica.
#[test]
fn a_caught_up_standby_of_an_idle_primary_has_no_lag() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let primary = probe_client(DSN).await;
        let standby = probe_client(STANDBY_DSN).await;
        // A write the standby replays, then nothing for a while.
        let home = scratch_home("lag-idle", WRITABLE_PROFILE);
        let t = format!("pgman_it_lag_{}", std::process::id());
        let out = batch_with_home(
            &home,
            &["--yes", "--sql", &format!("CREATE TABLE {t} (id int)")],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let lsn: String = primary
            .query_one("SELECT pg_current_wal_lsn()::text", &[])
            .await
            .unwrap()
            .get(0);
        for _ in 0..100 {
            let caught: bool = standby
                .query_one(
                    &format!("SELECT pg_last_wal_replay_lsn() >= '{lsn}'::pg_lsn"),
                    &[],
                )
                .await
                .unwrap()
                .get(0);
            if caught {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
        let p = pgman::query::probe::run(&standby, pgman::query::probe::SchemaSource::None)
            .await
            .unwrap();
        assert_eq!(p.in_recovery, Some(true));
        let lag = p.lag_secs.expect("a standby reports lag");
        assert!(lag < 1.0, "caught up, so no lag — got {lag}s");
        let p = pgman::query::probe::run(&primary, pgman::query::probe::SchemaSource::None)
            .await
            .unwrap();
        assert_eq!(
            (p.in_recovery, p.lag_secs),
            (Some(false), None),
            "a primary has none"
        );
    });
}

/// A unicode-escaped function name doesn't get past `query`'s guards:
/// `U&"pg_\0072ead_file"` is `pg_read_file` to Postgres.
#[test]
fn mcp_query_refuses_unicode_escaped_function_names() {
    let home = scratch_home("mcp-uescape", "");
    std::fs::write(
        home.join(".config/pgman/connections.toml"),
        format!("[[connections]]\nname = \"local\"\nurl = \"{DSN}\"\n"),
    )
    .unwrap();
    let frames = mcp_session_env(
        &home,
        &home,
        &[],
        &[],
        &[
            tool_call(
                1,
                "query",
                serde_json::json!({"connection": "local", "sql": r#"SELECT U&"pg_\0072ead_file"('PG_VERSION')"#}),
            ),
            tool_call(
                2,
                "query",
                serde_json::json!({"connection": "local", "sql": r#"SELECT U&"set_\0063onfig"('default_transaction_read_only', 'off', false)"#}),
            ),
            tool_call(
                3,
                "query",
                serde_json::json!({"connection": "local", "sql": "SELECT 1 AS one"}),
            ),
        ],
    );
    for id in [1, 2] {
        let (text, is_error) = reply(&frames, id);
        assert!(is_error, "call {id} ran: {text}");
        assert!(!text.contains("16"), "no file contents: {text}");
    }
    let (text, is_error) = reply(&frames, 3);
    assert!(!is_error && text.contains("\"one\""), "{text}");
}

/// A batch export past the row cap says so and fails, instead of
/// handing back 1000 rows as if they were all of them; json returns all.
#[test]
fn batch_says_when_a_csv_export_is_truncated() {
    let home = scratch_home("batch-cap", "");
    let out = batch_with_home(&home, &["--sql", "SELECT generate_series(1, 1500) AS n"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("output stopped at 1000 rows"));
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 1001);
    let out = batch_with_home(
        &home,
        &[
            "--format",
            "json",
            "--sql",
            "SELECT generate_series(1, 1500) AS n",
        ],
    );
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1500);
    let out = batch_with_home(&home, &["--sql", "SELECT generate_series(1, 10) AS n"]);
    assert!(out.status.success() && out.stderr.is_empty());
}

/// One huge value doesn't come back whole over MCP: `max_rows` counts
/// rows, and a single cell could be hundreds of megabytes.
#[test]
fn mcp_query_cuts_a_huge_cell() {
    let home = scratch_home("mcp-bigcell", "");
    std::fs::write(
        home.join(".config/pgman/connections.toml"),
        format!("[[connections]]\nname = \"local\"\nurl = \"{DSN}\"\n"),
    )
    .unwrap();
    let frames = mcp_session_env(
        &home,
        &home,
        &[],
        &[],
        &[tool_call(
            1,
            "query",
            serde_json::json!({"connection": "local", "sql": "SELECT repeat('x', 20000000) AS big"}),
        )],
    );
    let (text, is_error) = reply(&frames, 1);
    assert!(!is_error);
    assert!(text.len() < 64 * 1024, "{} bytes came back", text.len());
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["cells_cut"], 1);
}

/// A host that never answers fails within the connect timeout instead of
/// waiting out the OS's TCP timeout. (10.255.255.1 is unroutable: on
/// most networks packets to it vanish, which is the case that hung.)
#[test]
fn a_host_that_never_answers_fails_within_the_connect_timeout() {
    let home = scratch_home("connect-timeout", "");
    let started = std::time::Instant::now();
    let out = Command::new(pgman_binary())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .args([
            "--batch",
            "--dsn",
            "postgres://u:p@10.255.255.1:5432/db?sslmode=disable",
            "--sql",
            "SELECT 1",
        ])
        .output()
        .unwrap();
    let took = started.elapsed();
    assert!(!out.status.success());
    assert!(took < std::time::Duration::from_secs(20), "took {took:?}");
    eprintln!("{}", String::from_utf8_lossy(&out.stderr));
}

/// A probe user that can't see other sessions' transactions (no
/// pg_read_all_stats) reports long transactions as unknown, not zero.
#[test]
fn a_probe_without_read_all_stats_doesnt_report_zero_long_transactions() {
    let home = scratch_home("probe-unpriv", WRITABLE_PROFILE);
    let role = format!("pgman_it_ro_{}", std::process::id());
    let out = batch_with_home(
        &home,
        &[
            "--yes",
            "--sql",
            &format!("CREATE ROLE {role} LOGIN PASSWORD 'x' NOSUPERUSER"),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let me = probe_client(DSN).await;
        let p = pgman::query::probe::run(&me, pgman::query::probe::SchemaSource::None)
            .await
            .unwrap();
        assert!(p.long_tx.is_some(), "a superuser sees every session");
        let them = probe_client(&format!(
            "postgres://{role}:x@127.0.0.1:55432/pgman_test?sslmode=disable"
        ))
        .await;
        let p = pgman::query::probe::run(&them, pgman::query::probe::SchemaSource::None)
            .await
            .unwrap();
        assert_eq!(p.long_tx, None, "unknown, not 0");
        assert!(p.sessions.is_some());
    });
}

/// A connection the inventory calls direct but that is really a hot
/// standby is refused before any patch runs: the server is asked.
#[test]
fn patches_are_refused_on_a_standby_the_inventory_calls_direct() {
    let home = scratch_home("patch-standby", WRITABLE_PROFILE);
    let dir = home.join("patches");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("002-x.sql"),
        "INSERT INTO pgman_it_sb_v (id) VALUES (2);\n",
    )
    .unwrap();
    let env = pgman::inventory::parse_config(&format!(
        "[[environments]]\nname = \"sb\"\nsource = {{ kind = \"static\", connections = [\"s\"] }}\n\
         version_table = {{ table = \"pgman_it_sb_v\", column = \"id\", patches = \"{}\" }}\n",
        dir.display()
    ))
    .unwrap()
    .remove(0);
    let target = pgman::inventory::Target {
        database: "pgman_test".into(),
        server: "standby".into(),
        role: pgman::mcp::connections::Role::Direct,
        replica_of: None,
        host: None,
        port: None,
        engine: None,
        status: None,
        class: None,
        multi_az: None,
        connect: Ok(pgman::inventory::ConnectSpec {
            dsn: pgman::conn::Dsn::parse(STANDBY_DSN).unwrap(),
            secret: None,
            password_env: None,
        }),
    };
    let mut writable = pgman::safety::SafetyConfig::default();
    writable.default.read_only = false;
    let no_secrets: pgman::inventory::patching::FetchSecret =
        std::sync::Arc::new(|_| Err("no secrets in this test".into()));
    let plan = pgman::inventory::patching::PatchPlan {
        environment: "sb".into(),
        server: "standby".into(),
        database: "pgman_test".into(),
        protected: false,
        applied_max: None,
        patches: Vec::new(),
        stopped_at: None,
        dir: dir.display().to_string(),
        recorded: Vec::new(),
        below_recorded: Vec::new(),
        outside_transaction: false,
        outside_would_help: false,
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let r = rt.block_on(pgman::inventory::patching::apply(
        &env,
        &target,
        &writable,
        &no_secrets,
        &plan,
        pgman::inventory::patching::RunMode::Apply,
        &Default::default(),
    ));
    let err = r.error.expect("refused");
    assert!(err.contains("is a standby"), "{err}");
}
