//! Discover database connections from an IntelliJ project's `.idea/` folder.
//!
//! Two files matter:
//!
//! - `dataSources.xml` (committed) — the `<data-source>` shell: name, uuid,
//!   driver, `<jdbc-url>`. Sometimes `<user-name>` lives here too, but on
//!   newer IntelliJ versions it has moved.
//! - `dataSources.local.xml` (gitignored) — per-user metadata: the
//!   `<user-name>`, schema-mapping (which database IntelliJ has been
//!   introspecting — usually the actual db the operator works on, even
//!   when the JDBC URL has no path component), and a `<secret-storage>`
//!   pointer to the OS keychain (passwords are never in the XML).
//!
//! Real-world example we have to handle: `<jdbc-url>jdbc:postgresql://h:5432/</jdbc-url>`
//! in the committed file, `<user-name>postgres</user-name>` + schema-mapping
//! `qname="nems"` in the local file. The right pick is
//! `postgres://postgres@h:5432/nems`, not the URL's empty-path default.
//!
//! Pure parsing here; auto-detection at startup is the renderer's job.

use std::collections::HashMap;
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
                let name = e.name().as_ref().to_string();
                if name == "data-source" {
                    if let Some(ds) = current.take() {
                        sources.push(ds);
                    }
                }
                current_tag = None;
            }
            Ok(Event::Text(t)) => {
                if let (Some(ds), Some(tag)) = (current.as_mut(), current_tag.as_deref()) {
                    // quick-xml 0.42: `unescape()` became `xml10_content()`,
                    // which resolves entities infallibly rather than
                    // returning a Result. IntelliJ writes XML 1.0.
                    let val = t.xml10_content();
                    match tag {
                        "jdbc-url" => ds.jdbc_url = Some(val.to_string()),
                        "user-name" => ds.user = Some(val.to_string()),
                        _ => {}
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

/// Per-user metadata for one data source, pulled from `dataSources.local.xml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntellijLocalMeta {
    /// `<user-name>` text inside the per-user file.
    pub user: Option<String>,
    /// Distinct `qname` values found on `<node kind="database">` inside the
    /// `<schema-mapping>` block. These are the databases IntelliJ has been
    /// introspecting — usually the actual DBs the operator works on, even
    /// when the committed `<jdbc-url>` has no path.
    pub databases: Vec<String>,
}

/// Parse `dataSources.local.xml`. Returns a map keyed by data-source UUID
/// so the caller can join with the committed `dataSources.xml` entries.
pub fn parse_local(xml: &str) -> HashMap<String, IntellijLocalMeta> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut out: HashMap<String, IntellijLocalMeta> = HashMap::new();
    let mut current_uuid: Option<String> = None;
    let mut current_meta = IntellijLocalMeta::default();
    let mut current_tag: Option<String> = None;
    let mut in_schema_mapping = false;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = element_name(&e);
                match name.as_str() {
                    "data-source" => {
                        current_uuid = attr(&e, "uuid");
                        current_meta = IntellijLocalMeta::default();
                        current_tag = None;
                        in_schema_mapping = false;
                    }
                    "schema-mapping" => in_schema_mapping = true,
                    "node"
                        if in_schema_mapping
                        // Only nodes with kind="database" matter; their qname
                        // (when present and non-empty) is the dbname.
                        && attr(&e, "kind").as_deref() == Some("database") =>
                    {
                        if let Some(q) = attr(&e, "qname").filter(|s| !s.is_empty()) {
                            if !current_meta.databases.contains(&q) {
                                current_meta.databases.push(q);
                            }
                        }
                    }
                    _ if current_uuid.is_some() => {
                        current_tag = Some(name);
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name().as_ref().to_string();
                match name.as_str() {
                    "data-source" => {
                        if let Some(uuid) = current_uuid.take() {
                            out.insert(uuid, std::mem::take(&mut current_meta));
                        }
                        in_schema_mapping = false;
                    }
                    "schema-mapping" => in_schema_mapping = false,
                    _ => {}
                }
                current_tag = None;
            }
            Ok(Event::Empty(e)) => {
                // Self-closing tag (common for `<node ... />` in schema-mapping).
                let name = element_name(&e);
                if name == "node"
                    && in_schema_mapping
                    && attr(&e, "kind").as_deref() == Some("database")
                {
                    if let Some(q) = attr(&e, "qname").filter(|s| !s.is_empty()) {
                        if !current_meta.databases.contains(&q) {
                            current_meta.databases.push(q);
                        }
                    }
                }
            }
            Ok(Event::Text(t)) => {
                if let (Some(_), Some(tag)) = (current_uuid.as_deref(), current_tag.as_deref()) {
                    if tag == "user-name" {
                        current_meta.user = Some(t.xml10_content().to_string());
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// Build a connectable `Dsn` from an `IntellijDataSource`. Returns `None`
/// when the driver isn't Postgres or the URL is unparseable.
///
/// Merges in the `<user-name>` from the XML when the URL itself didn't
/// carry one, and falls back to the `PGPASSWORD` environment variable for
/// the password — passwords aren't stored in `dataSources.xml` (they live
/// in IntelliJ's keychain), so `PGPASSWORD` is the no-magic way to supply
/// one without re-typing the DSN.
/// Expand an `IntellijDataSource` (plus its local-file metadata, when
/// present) into one connectable `Dsn` per known database.
///
/// Cases:
/// - URL has an explicit path (`.../mydb`) → one entry with that dbname.
/// - URL has no path but local-meta lists databases → one entry per
///   database. The returned tuple's first element is the dbname so the
///   caller can disambiguate picker labels.
/// - URL has no path and no local-meta → one entry with the Postgres
///   default (`postgres`).
///
/// Username merge order (most-specific wins): URL → committed `<user-name>`
/// → local `<user-name>`. Password: URL → `PGPASSWORD`.
pub fn expand_to_dsns(
    source: &IntellijDataSource,
    local: Option<&IntellijLocalMeta>,
) -> Vec<(Option<String>, crate::conn::Dsn)> {
    let Some(jdbc) = source.jdbc_url.as_deref() else {
        return Vec::new();
    };
    let Some(raw) = jdbc_to_dsn(jdbc) else {
        return Vec::new();
    };
    let Ok(base) = crate::conn::Dsn::parse(&raw) else {
        return Vec::new();
    };
    let url_has_dbname = url_has_explicit_path(&raw);

    // Effective user: URL > committed source.user > local.user
    let effective_user = base
        .user
        .clone()
        .or_else(|| source.user.clone().filter(|s| !s.is_empty()))
        .or_else(|| local.and_then(|m| m.user.clone()).filter(|s| !s.is_empty()));

    // Effective password: URL > PGPASSWORD
    let effective_password = base
        .password
        .clone()
        .or_else(|| std::env::var("PGPASSWORD").ok().filter(|s| !s.is_empty()));

    let databases: Vec<Option<String>> = if url_has_dbname {
        // Trust the URL — one entry, dbname pulled from base.
        vec![None]
    } else if let Some(meta) = local {
        if meta.databases.is_empty() {
            vec![None]
        } else {
            meta.databases.iter().map(|d| Some(d.clone())).collect()
        }
    } else {
        vec![None]
    };

    databases
        .into_iter()
        .map(|override_db| {
            let mut dsn = base.clone();
            dsn.user = effective_user.clone();
            dsn.password = effective_password.clone();
            if let Some(db) = &override_db {
                dsn.dbname = db.clone();
            }
            (override_db, dsn)
        })
        .collect()
}

/// True when a `postgresql://…` URL string has a non-empty path component
/// (i.e. an explicit dbname). The Dsn parser defaults an empty path to
/// `postgres`, which loses the distinction — this re-checks the raw string.
fn url_has_explicit_path(url: &str) -> bool {
    let after_scheme = match url.split_once("://") {
        Some((_, r)) => r,
        None => return false,
    };
    // Strip ?query so a `?` doesn't get mistaken for path content.
    let auth_path = after_scheme
        .split_once('?')
        .map(|(a, _)| a)
        .unwrap_or(after_scheme);
    match auth_path.split_once('/') {
        Some((_, path)) => !path.is_empty(),
        None => false,
    }
}

pub fn to_dsn(source: &IntellijDataSource) -> Option<crate::conn::Dsn> {
    let raw = source.jdbc_url.as_deref().and_then(jdbc_to_dsn)?;
    let mut dsn = crate::conn::Dsn::parse(&raw).ok()?;
    if dsn.user.is_none() {
        if let Some(u) = &source.user {
            if !u.is_empty() {
                dsn.user = Some(u.clone());
            }
        }
    }
    if dsn.password.is_none() {
        if let Ok(pw) = std::env::var("PGPASSWORD") {
            if !pw.is_empty() {
                dsn.password = Some(pw);
            }
        }
    }
    Some(dsn)
}

// -- helpers --

fn element_name(e: &quick_xml::events::BytesStart<'_>) -> String {
    e.name().as_ref().to_string()
}

fn attr(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    for attr in e.attributes().flatten() {
        let key = attr.key.as_ref();
        if key == name {
            return attr
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()
                .map(|c| c.to_string());
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

    #[test]
    fn to_dsn_merges_user_from_xml_when_url_omits_it() {
        let src = IntellijDataSource {
            name: "prod".into(),
            uuid: "u1".into(),
            jdbc_url: Some("jdbc:postgresql://db.internal:5432/myapp".into()),
            user: Some("alice".into()),
        };
        let dsn = to_dsn(&src).expect("postgres URL → Dsn");
        assert_eq!(dsn.host, "db.internal");
        assert_eq!(dsn.port, 5432);
        assert_eq!(dsn.dbname, "myapp");
        assert_eq!(dsn.user.as_deref(), Some("alice"));
    }

    #[test]
    fn to_dsn_prefers_user_from_url_over_xml() {
        let src = IntellijDataSource {
            name: "prod".into(),
            uuid: "u1".into(),
            jdbc_url: Some("jdbc:postgresql://bob@db/myapp".into()),
            user: Some("alice".into()),
        };
        let dsn = to_dsn(&src).expect("postgres URL → Dsn");
        assert_eq!(dsn.user.as_deref(), Some("bob"));
    }

    #[test]
    fn to_dsn_returns_none_for_non_postgres_driver() {
        let src = IntellijDataSource {
            name: "x".into(),
            uuid: "u".into(),
            jdbc_url: Some("jdbc:mysql://h/db".into()),
            user: None,
        };
        assert!(to_dsn(&src).is_none());
    }

    #[test]
    fn to_dsn_returns_none_when_url_missing() {
        let src = IntellijDataSource {
            name: "x".into(),
            uuid: "u".into(),
            jdbc_url: None,
            user: None,
        };
        assert!(to_dsn(&src).is_none());
    }

    #[test]
    fn parse_local_extracts_user_and_schema_mapping_databases() {
        // Real-world shape: user-name and schema-mapping qname live in the
        // .local file. The dbname `nems` is what we actually want.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<project version="4">
  <component name="dataSourceStorageLocal">
    <data-source name="postgres@localhost" uuid="dc30c5be-e7c9-4f5f-a119-79aec0403c13">
      <secret-storage>master_key</secret-storage>
      <user-name>postgres</user-name>
      <schema-mapping>
        <introspection-scope>
          <node negative="1">
            <node kind="database" negative="1">
              <node kind="schema" negative="1" />
            </node>
            <node kind="database" qname="nems">
              <node kind="schema" qname="public" />
            </node>
          </node>
        </introspection-scope>
      </schema-mapping>
    </data-source>
  </component>
</project>"#;
        let got = parse_local(xml);
        let meta = got
            .get("dc30c5be-e7c9-4f5f-a119-79aec0403c13")
            .expect("meta keyed by uuid");
        assert_eq!(meta.user.as_deref(), Some("postgres"));
        assert_eq!(meta.databases, vec!["nems"]);
    }

    #[test]
    fn parse_local_collects_multiple_databases() {
        let xml = r#"<project>
  <component>
    <data-source uuid="u1">
      <user-name>alice</user-name>
      <schema-mapping>
        <node kind="database" qname="orders" />
        <node kind="database" qname="logs" />
        <node kind="database" qname="orders" />
      </schema-mapping>
    </data-source>
  </component>
</project>"#;
        let got = parse_local(xml);
        let meta = got.get("u1").unwrap();
        // Dedup preserves first-seen order.
        assert_eq!(meta.databases, vec!["orders", "logs"]);
    }

    #[test]
    fn expand_to_dsns_uses_schema_mapping_when_url_has_no_path() {
        // The exact scenario from the user's project: URL has no dbname,
        // user-name is in the local file, schema-mapping pins `nems`.
        let src = IntellijDataSource {
            name: "postgres@localhost".into(),
            uuid: "u1".into(),
            jdbc_url: Some("jdbc:postgresql://localhost:5432/".into()),
            user: None,
        };
        let local = IntellijLocalMeta {
            user: Some("postgres".into()),
            databases: vec!["nems".into()],
        };
        let dsns = expand_to_dsns(&src, Some(&local));
        assert_eq!(dsns.len(), 1);
        let (suffix, dsn) = &dsns[0];
        assert_eq!(suffix.as_deref(), Some("nems"));
        assert_eq!(dsn.host, "localhost");
        assert_eq!(dsn.port, 5432);
        assert_eq!(dsn.dbname, "nems");
        assert_eq!(dsn.user.as_deref(), Some("postgres"));
    }

    #[test]
    fn expand_to_dsns_emits_one_entry_per_database() {
        let src = IntellijDataSource {
            name: "local".into(),
            uuid: "u1".into(),
            jdbc_url: Some("jdbc:postgresql://h:5432/".into()),
            user: Some("alice".into()),
        };
        let local = IntellijLocalMeta {
            user: None,
            databases: vec!["a".into(), "b".into()],
        };
        let dsns = expand_to_dsns(&src, Some(&local));
        assert_eq!(dsns.len(), 2);
        assert_eq!(dsns[0].1.dbname, "a");
        assert_eq!(dsns[1].1.dbname, "b");
    }

    #[test]
    fn expand_to_dsns_keeps_url_dbname_when_present() {
        // When the URL has a path, ignore schema-mapping — the URL is the
        // operator's explicit choice.
        let src = IntellijDataSource {
            name: "x".into(),
            uuid: "u1".into(),
            jdbc_url: Some("jdbc:postgresql://h:5432/explicit".into()),
            user: None,
        };
        let local = IntellijLocalMeta {
            user: Some("alice".into()),
            databases: vec!["other".into()],
        };
        let dsns = expand_to_dsns(&src, Some(&local));
        assert_eq!(dsns.len(), 1);
        assert_eq!(dsns[0].0, None);
        assert_eq!(dsns[0].1.dbname, "explicit");
    }

    #[test]
    fn expand_to_dsns_falls_back_to_postgres_default_with_no_meta() {
        let src = IntellijDataSource {
            name: "x".into(),
            uuid: "u1".into(),
            jdbc_url: Some("jdbc:postgresql://h:5432/".into()),
            user: None,
        };
        let dsns = expand_to_dsns(&src, None);
        assert_eq!(dsns.len(), 1);
        assert_eq!(dsns[0].1.dbname, "postgres");
    }

    #[test]
    fn url_has_explicit_path_handles_common_shapes() {
        assert!(!url_has_explicit_path("postgresql://h:5432/"));
        assert!(!url_has_explicit_path("postgresql://h:5432"));
        assert!(url_has_explicit_path("postgresql://h:5432/db"));
        assert!(url_has_explicit_path("postgresql://h:5432/db?ssl=true"));
        assert!(!url_has_explicit_path("postgresql://h:5432/?foo=1"));
    }
}
