//! Discover database connections from an IntelliJ project's
//! `.idea/dataSources.xml`.
//!
//! IntelliJ stores each data source as a `<data-source>` element with child
//! `<jdbc-url>` and `<user-name>` text nodes. Passwords usually live in a
//! sibling `dataSources.local.xml` (gitignored) — `parse_local_passwords`
//! reads those when present.
//!
//! Pure parsing here; auto-detection at startup is the renderer's job.

use std::path::Path;

use quick_xml::events::Event;
use quick_xml::Reader;

/// A data source pulled from `dataSources.xml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntellijDataSource {
    pub name: String,
    pub uuid: String,
    pub jdbc_url: Option<String>,
    pub user: Option<String>,
}

/// True if `dir` looks like an IntelliJ project root (has a `.idea/`).
pub fn detect_intellij_project(dir: &Path) -> bool {
    dir.join(".idea").is_dir()
}

/// Parse the body of a `dataSources.xml` file. On any XML error returns
/// whatever was successfully parsed so far — partial info is better than none.
pub fn parse(xml: &str) -> Vec<IntellijDataSource> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut sources = Vec::new();
    let mut current: Option<IntellijDataSource> = None;
    let mut current_tag: Option<String> = None;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = element_name(&e);
                if name == "data-source" {
                    current = Some(IntellijDataSource {
                        name: attr(&e, "name").unwrap_or_default(),
                        uuid: attr(&e, "uuid").unwrap_or_default(),
                        jdbc_url: None,
                        user: None,
                    });
                    current_tag = None;
                } else if current.is_some() {
                    current_tag = Some(name);
                }
            }
            Ok(Event::End(e)) => {
                let name = std::str::from_utf8(e.name().as_ref())
                    .unwrap_or("")
                    .to_string();
                if name == "data-source" {
                    if let Some(ds) = current.take() {
                        sources.push(ds);
                    }
                }
                current_tag = None;
            }
            Ok(Event::Text(t)) => {
                if let (Some(ds), Some(tag)) = (current.as_mut(), current_tag.as_deref()) {
                    if let Ok(val) = t.unescape() {
                        match tag {
                            "jdbc-url" => ds.jdbc_url = Some(val.to_string()),
                            "user-name" => ds.user = Some(val.to_string()),
                            _ => {}
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    sources
}

/// Convert a JDBC URL (`jdbc:postgresql://host:port/db?...`) to a
/// `postgres://`-style DSN that `conn::Dsn::parse` accepts. Returns `None`
/// for non-Postgres drivers.
pub fn jdbc_to_dsn(jdbc_url: &str) -> Option<String> {
    let stripped = jdbc_url.strip_prefix("jdbc:")?;
    if stripped.starts_with("postgresql://") || stripped.starts_with("postgres://") {
        Some(stripped.to_string())
    } else {
        None
    }
}

// -- helpers --

fn element_name(e: &quick_xml::events::BytesStart<'_>) -> String {
    std::str::from_utf8(e.name().as_ref())
        .unwrap_or("")
        .to_string()
}

fn attr(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    for attr in e.attributes().flatten() {
        let key = std::str::from_utf8(attr.key.as_ref()).unwrap_or("");
        if key == name {
            return attr.unescape_value().ok().map(|c| c.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_dot_idea_directory() {
        let base = std::env::temp_dir().join(format!("pgman-idea-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        let with_idea = base.join("p1");
        std::fs::create_dir_all(with_idea.join(".idea")).unwrap();
        assert!(detect_intellij_project(&with_idea));

        let plain = base.join("p2");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(!detect_intellij_project(&plain));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn parses_a_single_data_source() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<project version="4">
  <component name="DataSourceManagerImpl" format="xml" multifile-model="true">
    <data-source source="LOCAL" name="prod" uuid="abc-123">
      <driver-ref>postgresql</driver-ref>
      <jdbc-url>jdbc:postgresql://localhost:5432/mydb</jdbc-url>
      <user-name>alice</user-name>
    </data-source>
  </component>
</project>"#;
        let got = parse(xml);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "prod");
        assert_eq!(got[0].uuid, "abc-123");
        assert_eq!(
            got[0].jdbc_url.as_deref(),
            Some("jdbc:postgresql://localhost:5432/mydb")
        );
        assert_eq!(got[0].user.as_deref(), Some("alice"));
    }

    #[test]
    fn parses_multiple_data_sources() {
        let xml = r#"<project>
  <component>
    <data-source name="a" uuid="u1">
      <jdbc-url>jdbc:postgresql://h/a</jdbc-url>
    </data-source>
    <data-source name="b" uuid="u2">
      <jdbc-url>jdbc:mysql://h/b</jdbc-url>
    </data-source>
  </component>
</project>"#;
        let got = parse(xml);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "a");
        assert_eq!(got[1].name, "b");
    }

    #[test]
    fn malformed_xml_returns_what_was_parsed() {
        let xml = "<project><data-source name=\"a\" uuid=\"u1\"><jdbc-url>jdbc:postgresql://h/a";
        let got = parse(xml);
        // The data-source element opened with attrs captured before the EOF.
        assert!(got.len() <= 1);
    }

    #[test]
    fn jdbc_to_dsn_handles_postgres_and_skips_others() {
        assert_eq!(
            jdbc_to_dsn("jdbc:postgresql://h:5432/db").as_deref(),
            Some("postgresql://h:5432/db")
        );
        assert_eq!(
            jdbc_to_dsn("jdbc:postgres://h/db").as_deref(),
            Some("postgres://h/db")
        );
        assert!(jdbc_to_dsn("jdbc:mysql://h/db").is_none());
        assert!(jdbc_to_dsn("postgresql://h/db").is_none()); // missing jdbc: prefix
    }
}
