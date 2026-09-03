//! pgman binary entry point — argument parsing, logging, then the TUI.

use clap::Parser;
use pgman::app::{AppMsg, DataSourcePick};
use pgman::{app, batch, conn, creds, font_probe, project, safety, tap, theme, tui, upgrade, util};
use std::io::IsTerminal;

/// Full flag documentation lives in `docs/commands.md` — the doc
/// comments here are what `--help` shows, so they stay to one short,
/// type-free sentence each.
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

    /// In --batch, proceed past statements the safety guard would only confirm.
    #[arg(long, help_heading = "Batch mode")]
    yes: bool,

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

    let dsn = match cli.dsn.as_deref() {
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
    let dsn_origin: Option<String> = if cli.dsn.is_some() {
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
                            dsn: Some(d),
                            unresolved: Vec::new(),
                            unresolved_host: Vec::new(),
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
            Ok(text) => application.preload_log(&text),
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
        let mut dsn = conn::Dsn::parse(raw).map_err(|e| format!("invalid --dsn: {e}"))?;
        apply_pgpassword(&mut dsn);
        return Ok(dsn);
    }
    let mut picks: Vec<DataSourcePick> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        if let Some((_, cfg)) = project::load_from(&cwd) {
            for c in &cfg.connections {
                if let Some(d) = project::connection_to_dsn(c) {
                    picks.push(DataSourcePick {
                        name: c.name.clone(),
                        origin: "project",
                        dsn: Some(d),
                        unresolved: Vec::new(),
                        unresolved_host: Vec::new(),
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
    batch_dsn_from_picks(picks)
}

/// Reduce the discovered candidate list to the one DSN `--batch` may
/// use, or the reason it may not. Pure — the tree-walking that produced
/// `picks` happens in `resolve_batch_dsn`.
///
/// Batch has no picker and nobody to prompt, so every question the TUI
/// would ask becomes a refusal here rather than a silent yes.
fn batch_dsn_from_picks(picks: Vec<DataSourcePick>) -> Result<conn::Dsn, String> {
    match picks.len() {
        0 => Err(
            "no DSN — pass --dsn or run from a project with .pgman/pgman.toml or .idea/dataSources.xml"
                .into(),
        ),
        1 => {
            let pick = picks.into_iter().next().expect("len checked");
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
            eprintln!("{}", format_connect_failure(&e, &dsn_for_hint));
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
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("pgman.log") {
                chmod_owner_only_if_exists(&entry.path());
            }
        }
    }
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_writer(appender)
        .with_env_filter(filter)
        .with_ansi(false)
        .init();
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
                dsn: Some(dsn),
                unresolved: Vec::new(),
                unresolved_host: Vec::new(),
            });
        }
    }
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
            if dsn.is_none() && unresolved.is_empty() && unresolved_host.is_empty() {
                // Not a Postgres URL and nothing unresolved to report —
                // e.g. a `jdbc:mysql:` block. Skipping it silently is
                // right; skipping a *marked* one is what hid the
                // problem from the operator.
                continue;
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
            picks.push(DataSourcePick {
                name,
                origin: "Spring",
                dsn,
                unresolved,
                unresolved_host,
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

    /// A discovered pick for the `batch_dsn_from_picks` tests.
    fn batch_pick(name: &str, url: &str) -> DataSourcePick {
        DataSourcePick {
            name: name.into(),
            origin: "project",
            dsn: conn::Dsn::parse(url).ok(),
            unresolved: Vec::new(),
            unresolved_host: Vec::new(),
        }
    }

    #[test]
    fn batch_uses_a_single_clean_discovered_pick() {
        let got = batch_dsn_from_picks(vec![batch_pick("only", "postgres://app@db/main")]).unwrap();
        assert_eq!(got.host, "db");
    }

    #[test]
    fn batch_refuses_a_discovered_ssh_tunnel() {
        // The TUI asks before spawning ssh; batch has nobody to ask, and
        // that is a reason to refuse, not to proceed quietly.
        let err = batch_dsn_from_picks(vec![batch_pick(
            "via-bastion",
            "postgres://app@db.internal:5432/main?ssh_tunnel=tom@bastion.example.com",
        )])
        .unwrap_err();
        assert!(err.contains("tom@bastion.example.com"), "got: {err}");
        assert!(err.contains("--dsn"), "should name the way forward: {err}");
    }

    #[test]
    fn batch_refuses_unresolved_placeholders_and_says_which_kind() {
        let mut host = batch_pick("app", "postgres://app@db/main");
        host.unresolved_host = vec!["DB_HOST".into()];
        let err = batch_dsn_from_picks(vec![host]).unwrap_err();
        assert!(err.starts_with("${DB_HOST} sits in the host"), "got: {err}");

        let mut user = batch_pick("app", "postgres://app@db/main");
        user.unresolved = vec!["DB_USER".into()];
        let err = batch_dsn_from_picks(vec![user]).unwrap_err();
        assert!(err.contains("export it"), "got: {err}");

        let mut broken = batch_pick("app", "postgres://app@db/main");
        broken.dsn = None;
        let err = batch_dsn_from_picks(vec![broken]).unwrap_err();
        assert!(err.contains("no usable connection URL"), "got: {err}");
    }

    #[test]
    fn batch_refuses_zero_or_ambiguous_candidates() {
        assert!(batch_dsn_from_picks(Vec::new())
            .unwrap_err()
            .contains("no DSN"));
        let err = batch_dsn_from_picks(vec![
            batch_pick("a", "postgres://app@a/main"),
            batch_pick("b", "postgres://app@b/main"),
        ])
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
}
