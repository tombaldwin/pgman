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
    assert!(
        stdout.contains(r#""id":"1""#) && stdout.contains(r#""name":"alice""#),
        "expected JSON shape; got {stdout}"
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
