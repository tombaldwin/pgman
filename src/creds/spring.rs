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
    fn parse_properties_all_handles_spring_canonical_prefix() {
        let text = "spring.datasource.url=jdbc:postgresql://h/x\n\
                    spring.datasource.username=svc";
        let entries = parse_properties_all(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].prefix, "spring.datasource");
    }
}
