//! pgman binary entry point — argument parsing, logging, then the TUI.

use clap::Parser;
use pgman::app::{AppMsg, DataSourcePick};
use pgman::{app, batch, conn, creds, font_probe, project, safety, tap, theme, tui, upgrade, util};

#[derive(Parser)]
#[command(
    name = "pgman",
    version = concat!(env!("CARGO_PKG_VERSION"), " · beta"),
    about = "k9s-style Postgres TUI for Java/AWS shops (public beta)"
)]
struct Cli {
    /// Connect using a postgres:// DSN.
    #[arg(long)]
    dsn: Option<String>,

    /// Colour theme: dark | light | high-contrast.
    #[arg(long, default_value = "dark")]
    theme: String,

    /// Pull the source repo and reinstall via cargo, then exit. Requires that
    /// pgman was installed from a local path (`cargo install --path …`).
    #[arg(long)]
    upgrade: bool,

    /// Run against a hand-crafted synthetic dataset — no database,
    /// no network, no disk writes. For screenshots / the README
    /// demo gif (`vhs demo.tape`) / talks. The frame is identical
    /// on every launch.
    #[arg(long)]
    demo: bool,

    /// Batch / pipe mode: run a SQL statement and write the result to
    /// stdout, then exit. No TUI. Suitable for shell scripts and CI.
    #[arg(long)]
    batch: bool,

    /// The SQL statement to run in `--batch` mode. If omitted, stdin
    /// is read until EOF.
    #[arg(long)]
    sql: Option<String>,

    /// Output format for `--batch`: csv (default) | tsv | json | expanded.
    #[arg(long, default_value = "csv")]
    format: String,

    /// Bind a TCP listener for the pgman-tap JAR (length-prefixed JSON
    /// events). Use `--tap-listen 127.0.0.1:7432` (or `:7432` for the
    /// same). When set, the listener starts before the TUI loop; events
    /// stream into `Mode::TapMonitor` (F4 from any mode). Omit to skip
    /// the listener entirely — pgman runs as a normal DB-side TUI.
    #[arg(long, value_name = "ADDR")]
    tap_listen: Option<String>,

    /// Bind an OTLP/HTTP listener so any OpenTelemetry-equipped JVM
    /// can stream Postgres spans straight into pgman without the
    /// pgman-tap JAR. Accepts `POST /v1/traces` with JSON bodies on
    /// the default OTLP port 4318. Example:
    /// `--tap-otlp :4318` then set
    /// `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://localhost:4318` and
    /// `OTEL_EXPORTER_OTLP_PROTOCOL=http/json` on the JVM.
    #[arg(long, value_name = "ADDR")]
    tap_otlp: Option<String>,

    /// Replay a captured tap event stream from a JSONL file (one
    /// `TapEvent` JSON object per line). Each event is fed into the
    /// same pipeline the live listeners use, so the TapMonitor /
    /// hotspots / N+1 views work identically against replayed data.
    /// Useful for demos and for exercising downstream layers
    /// (advisor, evidence-handoff) without a live JVM.
    #[arg(long, value_name = "PATH")]
    tap_replay: Option<std::path::PathBuf>,

    /// Bind a UDP listener for fire-and-forget tap events (one
    /// `TapEvent` JSON per datagram, no framing). Opt-in
    /// alternative to `--tap-listen` (TCP) for cases where the
    /// JVM side must never block on telemetry. UDP is lossy:
    /// dropped events are silently gone, with no
    /// `dropped_events_total` accounting on the receive side.
    #[arg(long, value_name = "ADDR")]
    tap_udp: Option<String>,

    /// Append every incoming TapEvent to this JSONL file (one
    /// event per line). Useful for capturing a real workload
    /// and replaying it later via `--tap-replay`. Captures
    /// from any active transport (TCP / UDP / OTLP). The file
    /// is opened append-only, so multiple sessions stack
    /// cleanly; rotate manually when it grows.
    #[arg(long, value_name = "PATH")]
    tap_record: Option<std::path::PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // `--upgrade` is the only flag that doesn't enter the TUI. Handle it
    // before we set up logging / probe the terminal.
    if cli.upgrade {
        return upgrade::run();
    }

    // `--batch` is the other non-TUI path. Don't init the rolling-file
    // logger either — keep tracing quiet so script output isn't
    // polluted; errors go to stderr in run().
    if cli.batch {
        let code = run_batch(&cli).await;
        std::process::exit(code);
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
        let mut application = pgman::demo::app(theme);
        let mut term = tui::Tui::enter()?;
        let result = application.run(&mut term).await;
        drop(term);
        return result;
    }

    let mut dsn = match cli.dsn.as_deref() {
        Some(raw) => match conn::Dsn::parse(raw) {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("invalid --dsn: {e}");
                std::process::exit(2);
            }
        },
        None => None,
    };
    let mut dsn_origin: Option<String> = if cli.dsn.is_some() {
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
                match project::connection_to_dsn(c) {
                    Some(d) => {
                        tracing::info!("  project connection '{}' → {}", c.name, d.redacted());
                        data_source_picks.push(DataSourcePick {
                            name: c.name.clone(),
                            origin: "project",
                            dsn: d,
                        });
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

    // Auto-pick when the operator didn't pass --dsn and there's exactly one
    // candidate. Multiple candidates → leave them in `data_source_picks` and
    // let the TUI render the picker (Mode::ConnPick).
    if dsn.is_none() && data_source_picks.len() == 1 {
        // Clone — keep the pick in the list so the connection-failure
        // screen can re-open the picker for a manual retry. The
        // Mode::ConnPick gate is `>= 2`, so a single entry won't pop the
        // picker at startup.
        let pick = &data_source_picks[0];
        tracing::info!(
            "auto-selecting {} data source '{}' → {}",
            pick.origin,
            pick.name,
            pick.dsn.redacted()
        );
        dsn_origin = Some(format!(
            "auto-picked {} data source '{}'",
            pick.origin, pick.name
        ));
        dsn = Some(pick.dsn.clone());
    }

    let safety_config = project::merge_safety(load_safety_config(), project_safety.as_ref());
    let mut application = app::App::new(theme, dsn, data_source_picks, safety_config);
    application.dsn_origin = dsn_origin;
    // Restore the editor draft from the last session (best-effort).
    // Cursor lands at the end so the operator can keep typing.
    if let Some(draft) = app::load_draft() {
        application.editor.cursor = draft.len();
        application.editor.buffer = draft;
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
        if java_project_detected {
            tracing::info!(
                "tap: Java project detected — auto-enabling --tap-listen :7432 (pass --tap-listen explicitly to override)"
            );
            Some(":7432".into())
        } else {
            None
        }
    });
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
                        // tokio::fs so a slow NFS doesn't
                        // block the runtime worker at startup.
                        let _ = tokio::fs::create_dir_all(parent).await;
                    }
                }
                match tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .await
                {
                    Ok(f) => {
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
                            let mut bytes = line.into_bytes();
                            bytes.push(b'\n');
                            let _ = f.write_all(&bytes).await;
                        }
                        last_drop_seen = cur_drops;
                    }
                    match tap::record_line(&event) {
                        Ok(line) => {
                            // Build the line + newline as one
                            // write so a panic mid-format can't
                            // leave a torn line.
                            let mut bytes = line.into_bytes();
                            bytes.push(b'\n');
                            // Write + flush sequentially. A
                            // failure disables the recorder so
                            // we don't spam logs every event.
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
                        Err(e) => tracing::warn!("tap-record: serialize failed: {e}"),
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
    if let Some(raw) = cli.dsn.as_deref() {
        return conn::Dsn::parse(raw).map_err(|e| format!("invalid --dsn: {e}"));
    }
    let mut picks: Vec<DataSourcePick> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        if let Some((_, cfg)) = project::load_from(&cwd) {
            for c in &cfg.connections {
                if let Some(d) = project::connection_to_dsn(c) {
                    picks.push(DataSourcePick {
                        name: c.name.clone(),
                        origin: "project",
                        dsn: d,
                    });
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
    match picks.len() {
        0 => Err(
            "no DSN — pass --dsn or run from a project with .pgman/pgman.toml / dataSources.xml"
                .into(),
        ),
        1 => Ok(picks.into_iter().next().unwrap().dsn),
        n => Err(format!(
            "found {n} candidate data sources — pass --dsn to disambiguate in batch mode"
        )),
    }
}

async fn run_batch(cli: &Cli) -> i32 {
    let dsn = match resolve_batch_dsn(cli) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{e}");
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
    let safety_cfg = load_safety_config();
    let profile = safety_cfg.profile_for(&dsn.dbname);
    let opts = batch::Opts {
        dsn,
        sql,
        format,
        read_only: profile.read_only,
        statement_timeout_ms: profile.statement_timeout_ms,
    };
    match batch::run(opts).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("connect failed: {e}");
            2
        }
    }
}

fn init_logging() {
    let dir = util::cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    let appender = tracing_appender::rolling::never(&dir, "pgman.log");
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_writer(appender)
        .with_env_filter(filter)
        .with_ansi(false)
        .init();
}

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
        let dsns = creds::intellij::expand_to_dsns(&s, meta);
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
            tracing::info!("  → pick {} = {}", label, dsn.redacted());
            picks.push(DataSourcePick {
                name: label,
                origin: "IntelliJ",
                dsn,
            });
        }
    }
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
    let mut emit = |label: &str, block: &[SpringDatasourcePartial]| {
        for p in block {
            let Some(url) = p.url.as_deref() else {
                continue;
            };
            let Some(raw) = creds::intellij::jdbc_to_dsn(url) else {
                continue;
            };
            let Ok(mut dsn) = conn::Dsn::parse(&raw) else {
                continue;
            };
            if let Some(u) = &p.username {
                if !u.is_empty() {
                    dsn.user = Some(u.clone());
                }
            }
            if let Some(pw) = &p.password {
                if !pw.is_empty() {
                    dsn.password = Some(pw.clone());
                }
            }
            // Provenance only — never the raw password (CLAUDE.md).
            tracing::info!("  → pick {}.{} = {}", label, p.prefix, dsn.redacted());
            picks.push(DataSourcePick {
                name: format!("{} ({})", p.prefix, label),
                origin: "Spring",
                dsn,
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
    fn parse_tap_addr_rejects_oversize_port() {
        let err = parse_tap_addr(":99999").unwrap_err();
        assert!(err.contains("port"), "expected port-range error: {err}");
    }
}
