//! pgman binary entry point.
//!
//! Pre-v1 scaffold: parses args, sets up logging, and prints what it can
//! resolve from the environment. The TUI event loop is M0 (see BACKLOG.md).

use clap::Parser;
use pgman::{conn, creds, safety, splash, theme, util};

#[derive(Parser)]
#[command(name = "pgman", version, about = "k9s-style Postgres TUI for Java/AWS shops")]
struct Cli {
    /// Connect using an explicit postgres:// DSN.
    #[arg(long)]
    dsn: Option<String>,

    /// Connect to an AWS RDS instance by identifier (v2 — not yet implemented).
    #[arg(long)]
    rds: Option<String>,

    /// AWS profile to use with --rds (v2).
    #[arg(long)]
    profile: Option<String>,

    /// Colour theme: dark | light | high-contrast.
    #[arg(long, default_value = "dark")]
    theme: String,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_logging();
    tracing::info!("pgman {} starting", env!("CARGO_PKG_VERSION"));

    println!("{}", splash::frame(0));
    println!(
        "pgman {} — pre-v1 scaffold. The TUI event loop is M0 (see BACKLOG.md).\n",
        env!("CARGO_PKG_VERSION")
    );

    let (theme, warn) = theme::Theme::resolve(&cli.theme);
    if let Some(w) = warn {
        eprintln!("warning: {w}");
    }
    println!("theme:  {}", theme.name);
    println!("config: {}", util::config_dir().display());
    println!("log:    {}", util::cache_dir().join("pgman.log").display());

    // Resolve a DSN if given, so the safety profile can key off the database.
    let dsn = match cli.dsn.as_deref() {
        Some(raw) => match conn::Dsn::parse(raw) {
            Ok(d) => {
                println!("dsn:    ok — {}", d.redacted());
                Some(d)
            }
            Err(e) => {
                eprintln!("error:  invalid --dsn: {e}");
                std::process::exit(2);
            }
        },
        None => None,
    };

    // Safety profile preview for the target database.
    let safety_config = load_safety_config();
    let db = dsn.as_ref().map(|d| d.dbname.as_str()).unwrap_or("default");
    let profile = safety_config.profile_for(db);
    println!(
        "safety: db={db} read_only={} statement_timeout_ms={} auto_tx={}",
        profile.read_only, profile.statement_timeout_ms, profile.auto_tx
    );

    // Spring auto-connect preview.
    if let Ok(cwd) = std::env::current_dir() {
        if creds::spring::detect_java_project(&cwd) {
            println!("creds:  Java project detected — Spring auto-connect lands in M1.5");
        }
    }

    if let Some(rds) = &cli.rds {
        let prof = cli.profile.as_deref().unwrap_or("default");
        println!("note:   --rds {rds} (aws profile {prof}) resolution is v2");
    }

    Ok(())
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
