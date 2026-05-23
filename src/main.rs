//! pgman binary entry point — argument parsing, logging, then the TUI.

use clap::Parser;
use pgman::{app, conn, creds, font_probe, safety, theme, tui, util};

#[derive(Parser)]
#[command(name = "pgman", version, about = "k9s-style Postgres TUI for Java/AWS shops")]
struct Cli {
    /// Connect using a postgres:// DSN.
    #[arg(long)]
    dsn: Option<String>,

    /// Colour theme: dark | light | high-contrast.
    #[arg(long, default_value = "dark")]
    theme: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
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

    let dsn = match cli.dsn.as_deref() {
        Some(raw) => match conn::Dsn::parse(raw) {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("invalid --dsn: {e}");
                std::process::exit(2);
            }
        },
        None => None,
    };

    // Log a few startup-context bits before the TUI takes over the screen.
    if let Ok(cwd) = std::env::current_dir() {
        if creds::spring::detect_java_project(&cwd) {
            tracing::info!("Java project detected at {}", cwd.display());
        }
        if creds::intellij::detect_intellij_project(&cwd) {
            let ds_path = cwd.join(".idea/dataSources.xml");
            if let Ok(xml) = std::fs::read_to_string(&ds_path) {
                let sources = creds::intellij::parse(&xml);
                if !sources.is_empty() {
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
                }
            }
        }
    }

    let safety_config = load_safety_config();
    let mut application = app::App::new(theme, dsn, safety_config);
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
