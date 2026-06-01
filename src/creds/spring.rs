//! Discover datasource settings from a Spring project's configuration.
//!
//! `parse_properties` handles `application.properties`. `application.yml`
//! parsing (`parse_yaml`) is M1.5 — it needs `serde_yaml` and the real Spring
//! profile / `${}` placeholder mechanics verified first (see BACKLOG.md).

use std::path::Path;

/// Datasource settings discovered from a Spring config file. Any field may be
/// an unresolved `${...}` placeholder — see `placeholders`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpringDatasource {
    pub url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    /// `${...}` placeholder bodies referenced by the values above, in the
    /// order encountered. Resolving them is v2 (`creds::ssm` etc.).
    pub placeholders: Vec<String>,
}

/// True if `dir` looks like a Java project root (Maven or Gradle).
pub fn detect_java_project(dir: &Path) -> bool {
    ["pom.xml", "build.gradle", "build.gradle.kts"]
        .iter()
        .any(|f| dir.join(f).is_file())
}

/// Parse `spring.datasource.{url,username,password}` out of the body of an
/// `application.properties` file.
///
/// Known limitation: backslash line-continuations are not joined — M1.5.
pub fn parse_properties(text: &str) -> SpringDatasource {
    let mut ds = SpringDatasource::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let Some(sep) = line.find(['=', ':']) else {
            continue;
        };
        let key = line[..sep].trim();
        let value = line[sep + 1..].trim();
        match key {
            "spring.datasource.url" => ds.url = Some(value.to_string()),
            "spring.datasource.username" => ds.username = Some(value.to_string()),
            "spring.datasource.password" => ds.password = Some(value.to_string()),
            _ => continue,
        }
        extract_placeholders(value, &mut ds.placeholders);
    }
    ds
}

/// One datasource discovered in a Spring `.properties` file, identified by
/// its prefix (the key text before the trailing `.url` / `.username` /
/// `.password`). A file can declare more than one — e.g.
/// `dataSource.url`, `logDataSource.url`, `replicaDataSource.url`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpringDatasourceEntry {
    /// The prefix (e.g. "dataSource", "spring.datasource", "logDataSource").
    pub prefix: String,
    pub url: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// A possibly-**partial** datasource block: any of `url` /
/// `username` / `password` may be absent. This is what a Spring
/// *profile* overlay (`application-prod.yml`) looks like — it
/// commonly carries just a password or just a URL, expecting the
/// base `application.yml` to supply the rest. [`merge_partials`]
/// folds an overlay onto a base; [`parse_properties_partials`]
/// produces these without the JDBC-URL filter that
/// [`parse_properties_all`] applies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpringDatasourcePartial {
    pub prefix: String,
    pub url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// Parse every `<prefix>.{url,username,password}` triple from a
/// `.properties` body into [`SpringDatasourcePartial`]s, in
/// first-appearance order of the prefix (across **any** field, so
/// a password-only prefix is still emitted — that's the whole
/// point for profile overlays). No filtering: non-datasource
/// prefixes (`service.url`, …) are dropped later by the JDBC
/// check at pick-emission / in [`parse_properties_all`].
pub fn parse_properties_partials(text: &str) -> Vec<SpringDatasourcePartial> {
    use std::collections::HashMap;

    let mut order: Vec<String> = Vec::new();
    let mut entries: HashMap<String, SpringDatasourcePartial> = HashMap::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let Some(sep) = line.find(['=', ':']) else {
            continue;
        };
        let key = line[..sep].trim();
        let value = line[sep + 1..].trim();

        let (prefix, field) = if let Some(p) = key.strip_suffix(".url") {
            (p.to_string(), "url")
        } else if let Some(p) = key.strip_suffix(".username") {
            (p.to_string(), "username")
        } else if let Some(p) = key.strip_suffix(".password") {
            (p.to_string(), "password")
        } else {
            continue;
        };

        if !entries.contains_key(&prefix) {
            order.push(prefix.clone());
            entries.insert(
                prefix.clone(),
                SpringDatasourcePartial {
                    prefix: prefix.clone(),
                    ..Default::default()
                },
            );
        }
        let entry = entries.get_mut(&prefix).expect("just inserted");
        match field {
            "url" => entry.url = Some(value.to_string()),
            "username" => entry.username = Some(value.to_string()),
            "password" => entry.password = Some(value.to_string()),
            _ => {}
        }
    }

    order
        .into_iter()
        .filter_map(|p| entries.remove(&p))
        .collect()
}

/// Parse every `<prefix>.url` / `<prefix>.username` / `<prefix>.password`
/// triple from a `.properties` body. Order in the output preserves the
/// order in which each prefix first appeared.
///
/// Filters: a prefix is only emitted when its `.url` starts with `jdbc:`
/// — otherwise it's almost certainly not a datasource (e.g. `service.url`,
/// `swagger.url`). Built on [`parse_properties_partials`] + this filter.
pub fn parse_properties_all(text: &str) -> Vec<SpringDatasourceEntry> {
    parse_properties_partials(text)
        .into_iter()
        .filter(|p| p.url.as_deref().is_some_and(|u| u.starts_with("jdbc:")))
        .map(|p| SpringDatasourceEntry {
            prefix: p.prefix,
            url: p.url.unwrap_or_default(),
            username: p.username,
            password: p.password,
        })
        .collect()
}

/// Overlay `profile` partials onto `base` partials, keyed by
/// prefix — Spring profile semantics. For each prefix the result
/// takes the profile's `url` / `username` / `password` when the
/// profile sets a non-empty value, else the base's. Prefixes only
/// in the base keep their values; prefixes only in the profile are
/// appended. Output order: base order first, then profile-only
/// prefixes. Pure.
pub fn merge_partials(
    base: &[SpringDatasourcePartial],
    profile: &[SpringDatasourcePartial],
) -> Vec<SpringDatasourcePartial> {
    use std::collections::HashMap;
    let overlay_by_prefix: HashMap<&str, &SpringDatasourcePartial> =
        profile.iter().map(|p| (p.prefix.as_str(), p)).collect();

    // Helper: prefer `over` when it carries a non-empty value.
    fn pick(base: &Option<String>, over: Option<&Option<String>>) -> Option<String> {
        let over_val = over.and_then(|o| o.as_ref()).filter(|s| !s.is_empty());
        match over_val {
            Some(v) => Some(v.clone()),
            None => base.clone(),
        }
    }

    let mut out: Vec<SpringDatasourcePartial> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for b in base {
        seen.insert(b.prefix.as_str());
        let o = overlay_by_prefix.get(b.prefix.as_str());
        out.push(SpringDatasourcePartial {
            prefix: b.prefix.clone(),
            url: pick(&b.url, o.map(|o| &o.url)),
            username: pick(&b.username, o.map(|o| &o.username)),
            password: pick(&b.password, o.map(|o| &o.password)),
        });
    }
    for p in profile {
        if !seen.contains(p.prefix.as_str()) {
            out.push(p.clone());
        }
    }
    out
}

/// Split a Spring config-file stem into `(family, profile)`.
/// `"application"` → `("application", None)`;
/// `"application-prod"` → `("application", Some("prod"))`;
/// `"bootstrap-dev"` → `("bootstrap", Some("dev"))`. Splits on the
/// first `-` (Spring's `application-<profile>` convention); a
/// trailing `-` with no profile text is treated as no profile.
pub fn split_config_name(stem: &str) -> (String, Option<String>) {
    match stem.split_once('-') {
        Some((family, profile)) if !profile.is_empty() => {
            (family.to_string(), Some(profile.to_string()))
        }
        _ => (stem.to_string(), None),
    }
}

/// Parse `spring.datasource.*` out of an `application.yml` body.
///
/// Stub — M1.5 (see BACKLOG.md).
pub fn parse_yaml(_text: &str) -> SpringDatasource {
    SpringDatasource::default()
}

/// Flatten a Spring-style `application.yml` to dot-notation property
/// lines, then run it through `parse_properties_all`. Same output type
/// so `discover_spring_datasources` can treat both file types uniformly.
///
/// The flattener handles the subset Spring config files use in
/// practice: nested mappings, `key: value` leaves, `#` comments, and
/// quoted string values. YAML lists / multi-line strings / anchors are
/// out of scope — those lines are skipped rather than crashing, which
/// is safe because they never appear in a datasource block.
pub fn parse_yaml_all(text: &str) -> Vec<SpringDatasourceEntry> {
    let flattened = flatten_yaml(text);
    parse_properties_all(&flattened)
}

/// YAML counterpart to [`parse_properties_partials`]: flatten the
/// document, then parse partials (url optional, no JDBC filter).
/// Used for profile-overlay merging where a profile file may
/// carry only a password.
pub fn parse_yaml_partials(text: &str) -> Vec<SpringDatasourcePartial> {
    let flattened = flatten_yaml(text);
    parse_properties_partials(&flattened)
}

/// Walk a YAML body line-by-line, tracking indent-based nesting, and
/// emit one `path.to.key=value` line per scalar leaf. Pure / testable.
pub fn flatten_yaml(text: &str) -> String {
    // Stack of (indent_cols, key) pairs we've descended into.
    let mut path: Vec<(usize, String)> = Vec::new();
    let mut out = String::new();
    for raw in text.lines() {
        // YAML document separator `---` ends the current document and
        // starts a fresh one. Reset the path stack so a later
        // `spring.datasource.url` in a sibling doc doesn't overwrite an
        // earlier one via the flattened-namespace merge.
        if raw.trim() == "---" || raw.trim() == "..." {
            path.clear();
            continue;
        }
        // Strip an inline `# comment`. Per YAML, a `#` starts a comment
        // only when preceded by whitespace (or it's the first char) AND
        // not inside a quoted string.
        let line = strip_yaml_comment(raw);
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        // Skip list items / anchors / aliases / unsupported constructs —
        // they don't appear inside the keys we care about.
        let first_non_ws = trimmed.trim_start();
        if first_non_ws.starts_with('-')
            || first_non_ws.starts_with('&')
            || first_non_ws.starts_with('*')
        {
            continue;
        }
        let indent = trimmed.len() - first_non_ws.len();
        // Pop any path components whose indent is >= ours — we've
        // dedented past them.
        while path.last().map(|(d, _)| *d >= indent).unwrap_or(false) {
            path.pop();
        }
        // Find the colon that separates key from value. YAML accepts
        // `key:` (no value, opens a child) and `key: value` (leaf).
        let Some(colon) = first_non_ws.find(':') else {
            continue;
        };
        let key = first_non_ws[..colon].trim().to_string();
        if key.is_empty() {
            continue;
        }
        let value_part = first_non_ws[colon + 1..].trim();
        if value_part.is_empty() {
            // YAML ambiguity: `key:` with nothing after is either an
            // empty-string leaf (deliberate blank) or opens a child
            // map. We can't disambiguate without look-ahead, so do
            // BOTH — emit `path.key=` as an empty leaf AND push onto
            // the path stack so any actually-nested children below
            // still resolve as `path.key.child=…`. Downstream
            // (`parse_properties_all`) drops triples without a JDBC
            // URL, so the spurious empty leaf is harmless and the
            // blank password / username case is no longer silently
            // dropped.
            let mut dotted = String::new();
            for (_, segment) in &path {
                dotted.push_str(segment);
                dotted.push('.');
            }
            dotted.push_str(&key);
            out.push_str(&dotted);
            out.push_str("=\n");
            path.push((indent, key));
            continue;
        }
        // Leaf. Build the dotted path and emit a property line.
        let mut dotted = String::new();
        for (_, segment) in &path {
            dotted.push_str(segment);
            dotted.push('.');
        }
        dotted.push_str(&key);
        // Strip surrounding quotes from the value.
        let value = unquote_yaml_scalar(value_part);
        out.push_str(&dotted);
        out.push('=');
        out.push_str(value);
        out.push('\n');
    }
    out
}

/// Strip an inline `# comment` from a YAML line. Honours quotes and
/// requires the `#` to be preceded by whitespace (or sit at the start).
fn strip_yaml_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            // Inside a double-quoted string, `\X` escapes the next char
            // — including `\"`. Skip the pair so the quote-state
            // doesn't flip mid-string and a later ` #` doesn't get
            // mis-read as a comment.
            b'\\' if in_double && i + 1 < bytes.len() => i += 1,
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'#' if !in_single && !in_double => {
                let preceded_by_ws = i == 0 || bytes[i - 1].is_ascii_whitespace();
                if preceded_by_ws {
                    return &line[..i];
                }
            }
            _ => {}
        }
        i += 1;
    }
    line
}

fn unquote_yaml_scalar(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// Append the body of each `${...}` placeholder found in `value` to `into`.
fn extract_placeholders(value: &str, into: &mut Vec<String>) {
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                into.push(after[..end].to_string());
                rest = &after[end + 1..];
            }
            None => break, // unterminated — stop scanning
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_maven_and_gradle_projects() {
        let base = std::env::temp_dir().join(format!("pgman-spring-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        let maven = base.join("maven");
        std::fs::create_dir_all(&maven).unwrap();
        std::fs::write(maven.join("pom.xml"), "<project/>").unwrap();
        assert!(detect_java_project(&maven));

        let gradle = base.join("gradle");
        std::fs::create_dir_all(&gradle).unwrap();
        std::fs::write(gradle.join("build.gradle.kts"), "").unwrap();
        assert!(detect_java_project(&gradle));

        let plain = base.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(!detect_java_project(&plain));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn parses_datasource_properties() {
        let text = "\
# database
spring.datasource.url=jdbc:postgresql://db:5432/orders
spring.datasource.username = svc_orders
spring.application.name=orders-api
";
        let ds = parse_properties(text);
        assert_eq!(ds.url.as_deref(), Some("jdbc:postgresql://db:5432/orders"));
        assert_eq!(ds.username.as_deref(), Some("svc_orders"));
        assert!(ds.password.is_none());
    }

    #[test]
    fn collects_placeholders() {
        let text = "spring.datasource.password=${db.password}\n\
                     spring.datasource.url=${DB_HOST:localhost}/app";
        let ds = parse_properties(text);
        assert_eq!(ds.placeholders, vec!["db.password", "DB_HOST:localhost"]);
    }

    #[test]
    fn accepts_colon_separated_keys_and_skips_comments() {
        let text = "! a comment\nspring.datasource.password: secret123";
        let ds = parse_properties(text);
        assert_eq!(ds.password.as_deref(), Some("secret123"));
    }

    #[test]
    fn empty_input_yields_empty_datasource() {
        assert_eq!(parse_properties(""), SpringDatasource::default());
    }

    #[test]
    fn parse_properties_all_handles_non_spring_prefix() {
        // Real-world: the user's project uses `dataSource.*`, not the
        // Spring-Boot-canonical `spring.datasource.*`. Both should work.
        let text = "\
dataSource.url=jdbc:postgresql://localhost:5432/shop?escapeSyntaxCallMode=callIfNoReturn
dataSource.username=shop
dataSource.password=local-dev-placeholder

logDataSource.url=jdbc:postgresql://localhost:5432/shoplog
logDataSource.username=shop
logDataSource.password=local-dev-placeholder
";
        let entries = parse_properties_all(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].prefix, "dataSource");
        assert_eq!(entries[0].username.as_deref(), Some("shop"));
        assert!(entries[0].url.contains("shop"));
        assert_eq!(entries[1].prefix, "logDataSource");
        assert!(entries[1].url.contains("shoplog"));
    }

    #[test]
    fn parse_properties_all_filters_non_jdbc_urls() {
        // `service.url` is a URL but not a datasource; should be dropped.
        let text = "\
service.url=https://example.test
dataSource.url=jdbc:postgresql://h/x
";
        let entries = parse_properties_all(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].prefix, "dataSource");
    }

    #[test]
    fn parse_properties_all_skips_prefixes_with_no_url() {
        // `mailSender.username` alone is not a datasource — no jdbc URL.
        let text = "\
mailSender.username=svc
mailSender.password=secret
dataSource.url=jdbc:postgresql://h/x
";
        let entries = parse_properties_all(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].prefix, "dataSource");
    }

    #[test]
    fn flatten_yaml_handles_spring_datasource_block() {
        let yaml = "\
spring:
  datasource:
    url: jdbc:postgresql://h:5432/db
    username: alice
    password: s3cret
  profiles:
    active: dev
";
        let flat = flatten_yaml(yaml);
        assert!(flat.contains("spring.datasource.url=jdbc:postgresql://h:5432/db"));
        assert!(flat.contains("spring.datasource.username=alice"));
        assert!(flat.contains("spring.datasource.password=s3cret"));
        assert!(flat.contains("spring.profiles.active=dev"));
    }

    #[test]
    fn flatten_yaml_strips_inline_comments_and_quotes() {
        let yaml = "\
# top-level comment
spring:
  datasource:
    url: \"jdbc:postgresql://h/db\"  # the url
    username: 'alice'
";
        let flat = flatten_yaml(yaml);
        assert!(flat.contains("spring.datasource.url=jdbc:postgresql://h/db"));
        assert!(flat.contains("spring.datasource.username=alice"));
    }

    #[test]
    fn flatten_yaml_skips_lists_and_anchors_without_crashing() {
        // List items and anchors aren't supported but mustn't break the
        // surrounding leaves.
        let yaml = "\
spring:
  profiles:
    - dev
    - test
  datasource:
    url: jdbc:postgresql://h/db
";
        let flat = flatten_yaml(yaml);
        assert!(flat.contains("spring.datasource.url=jdbc:postgresql://h/db"));
    }

    #[test]
    fn parse_yaml_all_returns_a_datasource_entry() {
        let yaml = "\
spring:
  datasource:
    url: jdbc:postgresql://h/db
    username: alice
    password: secret
";
        let entries = parse_yaml_all(yaml);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].prefix, "spring.datasource");
        assert_eq!(entries[0].username.as_deref(), Some("alice"));
        assert!(entries[0].url.contains("postgresql"));
    }

    #[test]
    fn flatten_yaml_resets_path_on_document_separator() {
        // Multi-doc Spring profile files: `---` ends one doc and starts
        // another. Without resetting, the second doc's keys merged with
        // the first via the flat namespace.
        let yaml = "\
spring:
  datasource:
    url: jdbc:postgresql://prod/db
---
spring:
  datasource:
    url: jdbc:postgresql://dev/db
";
        let flat = flatten_yaml(yaml);
        // Both URLs survive as distinct property lines in the flattened
        // output (parse_properties_all then HashMap-merges by prefix —
        // that's a different concern; the flattener's job is to not
        // lose either).
        assert!(flat.contains("prod"), "prod URL missing: {flat:?}");
        assert!(flat.contains("dev"), "dev URL missing: {flat:?}");
    }

    #[test]
    fn flatten_yaml_honours_backslash_escape_in_double_quotes() {
        // Escaped `\"` inside a double-quoted string used to flip the
        // comment-stripper's quote state, truncating the value.
        let yaml = r#"
spring:
  datasource:
    password: "a\"b #c"
"#;
        let flat = flatten_yaml(yaml);
        // The value isn't unquoted (mismatched outer quotes after the
        // escape) — that's fine. The KEY POINT is that the full
        // quoted-string body, including the ` #c`, is preserved.
        assert!(
            flat.contains("#c"),
            "comment-stripper truncated inside an escaped quote: {flat:?}"
        );
    }

    #[test]
    fn flatten_yaml_emits_empty_leaf_for_blank_value() {
        // `password:` (deliberately blank) used to be silently dropped
        // because the empty value was always interpreted as opening a
        // child map. Now we emit an empty leaf AND push the path so
        // both interpretations work.
        let yaml = "\
spring:
  datasource:
    url: jdbc:postgresql://h/db
    password:
    username: alice
";
        let flat = flatten_yaml(yaml);
        assert!(
            flat.contains("spring.datasource.password="),
            "blank password not emitted: {flat:?}"
        );
        assert!(flat.contains("spring.datasource.username=alice"));
    }

    #[test]
    fn parse_yaml_all_finds_non_spring_prefix() {
        // Some Spring apps put the connection straight under top-level
        // `dataSource:` (mirroring the .properties shape).
        let yaml = "\
dataSource:
  url: jdbc:postgresql://localhost:5432/shop
  username: shop
  password: ignored
";
        let entries = parse_yaml_all(yaml);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].prefix, "dataSource");
    }

    #[test]
    fn parse_properties_all_handles_spring_canonical_prefix() {
        let text = "spring.datasource.url=jdbc:postgresql://h/x\n\
                    spring.datasource.username=svc";
        let entries = parse_properties_all(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].prefix, "spring.datasource");
    }

    fn partial(
        prefix: &str,
        url: Option<&str>,
        user: Option<&str>,
        pass: Option<&str>,
    ) -> SpringDatasourcePartial {
        SpringDatasourcePartial {
            prefix: prefix.to_string(),
            url: url.map(str::to_string),
            username: user.map(str::to_string),
            password: pass.map(str::to_string),
        }
    }

    #[test]
    fn partials_emit_password_only_prefix() {
        // The whole point: a profile overlay that sets only a
        // password must still surface its prefix (with url=None).
        let text = "spring.datasource.password=prod-secret";
        let ps = parse_properties_partials(text);
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].prefix, "spring.datasource");
        assert_eq!(ps[0].url, None);
        assert_eq!(ps[0].password.as_deref(), Some("prod-secret"));
    }

    #[test]
    fn partials_keep_non_jdbc_prefixes_unlike_all() {
        // partials are unfiltered; `_all` drops the non-jdbc one.
        let text = "service.url=https://api\nspring.datasource.url=jdbc:postgresql://h/x";
        assert_eq!(parse_properties_partials(text).len(), 2);
        assert_eq!(parse_properties_all(text).len(), 1);
    }

    #[test]
    fn merge_overlays_password_and_inherits_base_url() {
        let base = vec![partial(
            "spring.datasource",
            Some("jdbc:postgresql://db/orders"),
            Some("app"),
            Some("base-pw"),
        )];
        let profile = vec![partial("spring.datasource", None, None, Some("prod-pw"))];
        let merged = merge_partials(&base, &profile);
        assert_eq!(merged.len(), 1);
        // URL + username inherited from base; password overridden.
        assert_eq!(
            merged[0].url.as_deref(),
            Some("jdbc:postgresql://db/orders")
        );
        assert_eq!(merged[0].username.as_deref(), Some("app"));
        assert_eq!(merged[0].password.as_deref(), Some("prod-pw"));
    }

    #[test]
    fn merge_empty_overlay_value_does_not_clobber_base() {
        let base = vec![partial(
            "ds",
            Some("jdbc:postgresql://h/x"),
            Some("u"),
            Some("p"),
        )];
        // Profile present but with empty strings — must not erase base.
        let profile = vec![partial("ds", Some(""), Some(""), Some(""))];
        let merged = merge_partials(&base, &profile);
        assert_eq!(merged[0].url.as_deref(), Some("jdbc:postgresql://h/x"));
        assert_eq!(merged[0].username.as_deref(), Some("u"));
        assert_eq!(merged[0].password.as_deref(), Some("p"));
    }

    #[test]
    fn merge_appends_profile_only_prefix() {
        let base = vec![partial(
            "primary",
            Some("jdbc:postgresql://h/a"),
            None,
            None,
        )];
        let profile = vec![partial(
            "replica",
            Some("jdbc:postgresql://h/b"),
            None,
            None,
        )];
        let merged = merge_partials(&base, &profile);
        assert_eq!(
            merged.iter().map(|p| p.prefix.as_str()).collect::<Vec<_>>(),
            vec!["primary", "replica"]
        );
    }

    #[test]
    fn merge_profile_url_overrides_base_url() {
        let base = vec![partial(
            "ds",
            Some("jdbc:postgresql://dev/x"),
            Some("u"),
            None,
        )];
        let profile = vec![partial("ds", Some("jdbc:postgresql://prod/x"), None, None)];
        let merged = merge_partials(&base, &profile);
        assert_eq!(merged[0].url.as_deref(), Some("jdbc:postgresql://prod/x"));
        assert_eq!(merged[0].username.as_deref(), Some("u"));
    }

    #[test]
    fn split_config_name_separates_family_and_profile() {
        assert_eq!(
            split_config_name("application"),
            ("application".into(), None)
        );
        assert_eq!(
            split_config_name("application-prod"),
            ("application".into(), Some("prod".into()))
        );
        assert_eq!(
            split_config_name("bootstrap-dev"),
            ("bootstrap".into(), Some("dev".into()))
        );
        // Trailing dash → treated as no profile.
        assert_eq!(
            split_config_name("application-"),
            ("application-".into(), None)
        );
    }

    #[test]
    fn yaml_partials_overlay_end_to_end() {
        // Base supplies url + username; profile overlay supplies
        // only the password. The merge yields a complete block.
        let base_yaml =
            "spring:\n  datasource:\n    url: jdbc:postgresql://db/orders\n    username: app\n";
        let prof_yaml = "spring:\n  datasource:\n    password: prod-secret\n";
        let base = parse_yaml_partials(base_yaml);
        let prof = parse_yaml_partials(prof_yaml);
        let merged = merge_partials(&base, &prof);
        let ds = merged
            .iter()
            .find(|p| p.prefix == "spring.datasource")
            .expect("datasource prefix");
        assert_eq!(ds.url.as_deref(), Some("jdbc:postgresql://db/orders"));
        assert_eq!(ds.username.as_deref(), Some("app"));
        assert_eq!(ds.password.as_deref(), Some("prod-secret"));
    }
}
