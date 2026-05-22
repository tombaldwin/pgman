//! pgman binary entry point — argument parsing, logging, then the TUI.

use clap::Parser;
use pgman::{app, conn, font_probe, safety, theme, tui, util};

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

    // Resolve the safety profile for the target database.
    let safety_config = load_safety_config();
    let db = dsn.as_ref().map(|d| d.dbname.as_str()).unwrap_or("default");
    let profile = safety_config.profile_for(db);
    let read_only = profile.read_only;
    let statement_timeout_ms = profile.statement_timeout_ms;

    let mut application = app::App::new(theme, dsn, read_only, statement_timeout_ms);
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
