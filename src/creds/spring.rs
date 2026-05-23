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

/// Parse every `<prefix>.url` / `<prefix>.username` / `<prefix>.password`
/// triple from a `.properties` body. Order in the output preserves the
/// order in which each prefix's `.url` line was encountered.
///
/// Filters: a prefix is only emitted when its `.url` starts with `jdbc:`
/// — otherwise it's almost certainly not a datasource (e.g. `service.url`,
/// `swagger.url`).
pub fn parse_properties_all(text: &str) -> Vec<SpringDatasourceEntry> {
    use std::collections::HashMap;

    let mut order: Vec<String> = Vec::new();
    let mut entries: HashMap<String, SpringDatasourceEntry> = HashMap::new();

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

        let entry = entries
            .entry(prefix.clone())
            .or_insert_with(|| SpringDatasourceEntry {
                prefix: prefix.clone(),
                url: String::new(),
                username: None,
                password: None,
            });
        match field {
            "url" => {
                if entry.url.is_empty() && !order.contains(&prefix) {
                    order.push(prefix.clone());
                }
                entry.url = value.to_string();
            }
            "username" => entry.username = Some(value.to_string()),
            "password" => entry.password = Some(value.to_string()),
            _ => {}
        }
    }

    // Reassemble in first-seen order, dropping prefixes that never got a
    // jdbc:* URL (or got nothing at all).
    order
        .into_iter()
        .filter_map(|p| entries.remove(&p))
        .filter(|e| e.url.starts_with("jdbc:"))
        .collect()
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

/// Walk a YAML body line-by-line, tracking indent-based nesting, and
/// emit one `path.to.key=value` line per scalar leaf. Pure / testable.
pub fn flatten_yaml(text: &str) -> String {
    // Stack of (indent_cols, key) pairs we've descended into.
    let mut path: Vec<(usize, String)> = Vec::new();
    let mut out = String::new();
    for raw in text.lines() {
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
            // Opens a child map. Push and move on.
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
    for (i, &b) in bytes.iter().enumerate() {
        match b {
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
        assert_eq!(
            ds.url.as_deref(),
            Some("jdbc:postgresql://db:5432/orders")
        );
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
dataSource.url=jdbc:postgresql://localhost:5432/nems?escapeSyntaxCallMode=callIfNoReturn
dataSource.username=nems
dataSource.password=Exp!0de3atGravy

logDataSource.url=jdbc:postgresql://localhost:5432/nemslog
logDataSource.username=nems
logDataSource.password=Exp!0de3atGravy
";
        let entries = parse_properties_all(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].prefix, "dataSource");
        assert_eq!(entries[0].username.as_deref(), Some("nems"));
        assert!(entries[0].url.contains("nems"));
        assert_eq!(entries[1].prefix, "logDataSource");
        assert!(entries[1].url.contains("nemslog"));
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
    fn parse_yaml_all_finds_non_spring_prefix() {
        // Some Spring apps put the connection straight under top-level
        // `dataSource:` (mirroring the .properties shape).
        let yaml = "\
dataSource:
  url: jdbc:postgresql://localhost:5432/nems
  username: nems
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
}
