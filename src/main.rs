//! pgman binary entry point — argument parsing, logging, then the TUI.

use clap::Parser;
use pgman::app::DataSourcePick;
use pgman::{app, conn, creds, font_probe, project, safety, theme, tui, upgrade, util};

#[derive(Parser)]
#[command(name = "pgman", version, about = "k9s-style Postgres TUI for Java/AWS shops")]
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // `--upgrade` is the only flag that doesn't enter the TUI. Handle it
    // before we set up logging / probe the terminal.
    if cli.upgrade {
        return upgrade::run();
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
                        "  project connection '{}' has unparseable url {:?}; skipping",
                        c.name,
                        c.url
                    ),
                }
            }
        }
        if creds::spring::detect_java_project(&cwd) {
            tracing::info!("Java project detected at {}", cwd.display());
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
    let mut term = tui::Tui::enter()?;
    let result = application.run(&mut term).await;
    drop(term); // restore the terminal before surfacing any error
    result
}

/// Send `tracing` output to `~/.cache/pgman/pgman.log`. Level via `RUST_LOG`,
/// defaulting to `info`.
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
            s.jdbc_url.as_deref().unwrap_or("(no jdbc-url)")
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
                    let known_prefix =
                        n.starts_with("application") || n.starts_with("bootstrap");
                    let known_ext = n.ends_with(".properties")
                        || n.ends_with(".yml")
                        || n.ends_with(".yaml");
                    known_prefix && known_ext
                })
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let ext = path.extension().and_then(|e| e.to_str());
        let entries = match ext {
            Some("properties") => creds::spring::parse_properties_all(&text),
            Some("yml") | Some("yaml") => creds::spring::parse_yaml_all(&text),
            _ => continue,
        };
        if entries.is_empty() {
            continue;
        }
        let file_label = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("application");
        tracing::info!(
            "Spring properties: {} datasource(s) in {}",
            entries.len(),
            path.display()
        );
        for e in entries {
            let Some(raw) = creds::intellij::jdbc_to_dsn(&e.url) else {
                continue;
            };
            let Ok(mut dsn) = conn::Dsn::parse(&raw) else {
                continue;
            };
            // Spring's username/password keys win over anything in the URL —
            // operators usually only put credentials in one place.
            if let Some(u) = e.username {
                if !u.is_empty() {
                    dsn.user = Some(u);
                }
            }
            if let Some(p) = e.password {
                if !p.is_empty() {
                    dsn.password = Some(p);
                }
            }
            // Provenance line — note we log the redacted DSN, never the raw
            // password (CLAUDE.md: never log credentials).
            tracing::info!("  → pick {}.{} = {}", file_label, e.prefix, dsn.redacted());
            picks.push(DataSourcePick {
                name: format!("{} ({})", e.prefix, file_label),
                origin: "Spring",
                dsn,
            });
        }
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
