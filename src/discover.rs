//! Data-source discovery from the working tree: `.pgman/pgman.toml`
//! connections, Spring `application*.{yml,yaml,properties}` datasources and
//! IntelliJ `.idea/dataSources*.xml`. Everything found here names a host the
//! checkout chose, so every pick is untrusted until an operator acts on it —
//! see `DataSourcePick` and `App::refuse_if_unresolved`.
//!
//! Library code (not `main.rs`) so every entry point that connects to a
//! discovered source discovers it the same way.

use crate::app::DataSourcePick;
use crate::{app, conn, creds, project};

/// Read `.idea/dataSources.xml` + `.idea/dataSources.local.xml`, merge by
/// UUID (so the local file's `<user-name>` and schema-mapping db names
/// fill in for the committed file), and push every Postgres data source
/// onto `picks`. When schema-mapping exposes multiple databases for one
/// data source, we emit one pick per database (label suffixed with the
/// dbname so the picker disambiguates).
pub fn discover_intellij_datasources(cwd: &std::path::Path, picks: &mut Vec<DataSourcePick>) {
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
pub fn resolve_intellij_placeholders(
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
pub fn resolve_spring_value(value: &str) -> (String, Vec<String>) {
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
pub fn project_connection_pick(c: &project::Connection) -> Option<DataSourcePick> {
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
pub fn discover_spring_datasources(cwd: &std::path::Path, picks: &mut Vec<DataSourcePick>) {
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
