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
}
