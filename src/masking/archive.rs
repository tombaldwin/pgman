//! A masked dump on disk: pgman's own archive, a directory.
//!
//! ```text
//! production-ledger-20261028T140311Z.pgman-dump/
//!   header.json     what it is: format, pgman, dump id, source, when, masked
//!   manifest.json   each table's columns, rows, file and sha256; the schema's
//!   schema.dump     pg_dump's custom archive of everything but table rows
//!   data/0001.copy.gz …  each table's masked rows, COPY text, gzipped
//! ```
//!
//! Only `pgman restore` reads it. Files inside have generated names, so a
//! table's name never becomes a path; restore checks every checksum
//! before loading anything. Pure, but for [`sha256_file`].

use super::classify::{Origin, Plan, Rule};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const EXTENSION: &str = ".pgman-dump";
pub const FORMAT: u32 = 1;
pub const HEADER: &str = "header.json";
pub const MANIFEST: &str = "manifest.json";
pub const SCHEMA: &str = "schema.dump";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub format: u32,
    pub pgman: String,
    pub copy: String,
    pub label: String,
    pub env: Option<String>,
    pub database: String,
    pub created: u64,
    pub masked: bool,
    /// Taken from a protected target.
    #[serde(default)]
    pub protected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableEntry {
    /// `schema.name`.
    pub table: String,
    pub columns: Vec<String>,
    pub rows: u64,
    /// Under `data/`.
    pub file: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub header: Header,
    pub schema_sha256: String,
    pub tables: Vec<TableEntry>,
    pub large_objects: bool,
    /// The rules file's sha256, or `defaults`.
    pub rules: String,
    /// `fresh`: a key made for this copy and thrown away.
    pub key: String,
    pub words: u32,
    /// Each sequence: schema, name, last value, whether it was called.
    #[serde(default)]
    pub sequences: Vec<(String, String, i64, bool)>,
}

/// The generated name of the `n`th table's file. Pure.
pub fn data_file(n: usize) -> String {
    format!("{:04}.copy.gz", n + 1)
}

/// Whether `name` is one [`data_file`] makes: digits, `.copy.gz`, no
/// path. Pure.
pub fn safe_data_file(name: &str) -> bool {
    name.strip_suffix(".copy.gz")
        .is_some_and(|n| !n.is_empty() && n.len() <= 8 && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The hex sha256 of `bytes`. Pure.
pub fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The hex sha256 of a file, read in chunks. Reads it.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    Ok(ctx
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Why `m` isn't an archive this pgman restores, if it isn't: another
/// format, a file name that could be a path. Pure.
pub fn manifest_problem(m: &Manifest) -> Option<String> {
    if m.header.format != FORMAT {
        return Some(format!(
            "the archive is format {}, and this pgman reads format {FORMAT}: use a newer pgman",
            m.header.format
        ));
    }
    m.tables.iter().find(|t| !safe_data_file(&t.file)).map(|t| {
        format!(
            "the manifest names a data file {:?}: not one pgman writes",
            t.file
        )
    })
}

/// What a masked dump says it did: what each rule masked, every column
/// NULLed by name, what was left out. Pure.
pub fn summary(plan: &Plan) -> Vec<String> {
    let mut by_rule: Vec<(&'static str, Vec<String>)> = Vec::new();
    let mut nulled = Vec::new();
    let mut drops = Vec::new();
    for t in &plan.tables {
        if t.drop_rows {
            drops.push(format!(
                "{} ({})",
                t.table,
                t.drop_why.as_deref().unwrap_or("rows left out")
            ));
            continue;
        }
        for d in &t.columns {
            let at = format!("{}.{}", t.table, d.column);
            let at = match &d.origin {
                Origin::Follows(from) => format!("{at} (follows {from})"),
                _ => at,
            };
            match &d.rule {
                Rule::Keep => {}
                Rule::Null => nulled.push(at),
                r => match by_rule.iter_mut().find(|(n, _)| *n == r.name()) {
                    Some((_, list)) => list.push(at),
                    None => by_rule.push((r.name(), vec![at])),
                },
            }
        }
    }
    let masked: usize = by_rule.iter().map(|(_, l)| l.len()).sum::<usize>() + nulled.len();
    let mut out = vec![format!(
        "masked {masked} column{} in {} table{}",
        if masked == 1 { "" } else { "s" },
        plan.tables.len(),
        if plan.tables.len() == 1 { "" } else { "s" }
    )];
    for (rule, cols) in &by_rule {
        out.push(format!("  {rule:<10} {}", cols.join(", ")));
    }
    if !nulled.is_empty() {
        out.push(format!("  {:<10} {}", "NULL", nulled.join(", ")));
        out.push(
            "  a NULLed column the team needs: pgman masking show <target> gives the line that \
             keeps it"
                .to_string(),
        );
    }
    if !drops.is_empty() {
        out.push(format!("  no rows    {}", drops.join(", ")));
    }
    if !plan.large_objects {
        out.push(
            "  large objects left out (large_objects = \"keep\" in the rules keeps them)".into(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::classify::{Decision, TablePlan};
    use super::*;

    fn dec(column: &str, rule: Rule, origin: Origin) -> Decision {
        Decision {
            column: column.into(),
            rule,
            origin,
            fit: None,
            not_null: false,
            unique: false,
            base: "text".into(),
            enum_first: None,
            array: false,
        }
    }

    #[test]
    fn only_generated_names_are_data_files() {
        assert_eq!(data_file(0), "0001.copy.gz");
        assert!(safe_data_file("0001.copy.gz"));
        for bad in [
            "../x.copy.gz",
            "a/0001.copy.gz",
            ".copy.gz",
            "0001.copy",
            "0x01.copy.gz",
        ] {
            assert!(!safe_data_file(bad), "{bad}");
        }
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn the_summary_names_every_nulled_column() {
        let plan = Plan {
            tables: vec![
                TablePlan {
                    table: "public.customers".into(),
                    drop_rows: false,
                    drop_why: None,
                    columns: vec![
                        dec("id", Rule::Keep, Origin::Plain),
                        dec("email", Rule::Email, Origin::Default("x".into())),
                        dec("notes", Rule::Null, Origin::Default("x".into())),
                    ],
                    primary_key: vec!["id".into()],
                },
                TablePlan {
                    table: "public.orders".into(),
                    drop_rows: false,
                    drop_why: None,
                    columns: vec![dec(
                        "customer_email",
                        Rule::Email,
                        Origin::Follows("public.customers.email".into()),
                    )],
                    primary_key: vec![],
                },
                TablePlan {
                    table: "public.spring_session".into(),
                    drop_rows: true,
                    drop_why: Some("Spring Session: no rows".into()),
                    columns: vec![],
                    primary_key: vec![],
                },
            ],
            large_objects: false,
            problems: vec![],
        };
        let s = summary(&plan).join("\n");
        assert!(s.starts_with("masked 3 columns in 3 tables"), "{s}");
        assert!(s.contains("email      public.customers.email, public.orders.customer_email (follows public.customers.email)"), "{s}");
        assert!(s.contains("NULL       public.customers.notes"), "{s}");
        assert!(
            s.contains("no rows    public.spring_session (Spring Session: no rows)"),
            "{s}"
        );
        assert!(s.contains("large objects left out"), "{s}");
    }

    #[test]
    fn a_manifest_from_elsewhere_is_refused() {
        let m = Manifest {
            header: Header {
                format: 1,
                pgman: "0.4.0".into(),
                copy: "c".into(),
                label: "p".into(),
                env: None,
                database: "d".into(),
                created: 0,
                masked: true,
                protected: true,
            },
            schema_sha256: String::new(),
            tables: vec![TableEntry {
                table: "public.t".into(),
                columns: vec![],
                rows: 0,
                file: "../../etc/passwd".into(),
                sha256: String::new(),
            }],
            large_objects: false,
            rules: "defaults".into(),
            key: "fresh".into(),
            words: 1,
            sequences: vec![],
        };
        assert!(manifest_problem(&m)
            .unwrap()
            .contains("not one pgman writes"));
        let mut newer = m.clone();
        newer.tables.clear();
        newer.header.format = 2;
        assert!(manifest_problem(&newer).unwrap().contains("format 2"));
        newer.header.format = 1;
        assert_eq!(manifest_problem(&newer), None);
    }
}
