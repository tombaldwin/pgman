//! The project's masking rules: `.pgman/masking/<database>.toml`,
//! committed beside the schema, holding only where the team differs from
//! the defaults. Found by walking up from the current directory, or named
//! with `--masking FILE`. Pure parsing; [`find`] is the thin reader.
//!
//! ```toml
//! version = 1
//! large_objects = "drop"          # or "keep"
//!
//! [classify]
//! personal = ["handle"]           # more words that mean personal
//! not_personal = ["notes"]        # words that don't, here
//! reason = "notes hold no personal data in this schema"
//!
//! [tables.customers]              # public.customers
//! notes = { rule = "keep", reason = "reviewed: support codes only" }
//! contact = "email"
//! signup_on = { rule = "date.shift", days = 90 }
//!
//! [tables."audit.events"]
//! rows = "drop"
//! ```

use super::catalog::{Catalog, Table};
use super::classify::{NamePart, Rule};
use std::path::{Path, PathBuf};

/// `rows = "drop"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rows {
    Drop,
}

/// One column's rule, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnRule {
    pub column: String,
    pub rule: Rule,
    pub reason: Option<String>,
}

/// One `[tables.X]` block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableRules {
    /// As written: `customers` (in `public`) or `audit.events`.
    pub key: String,
    pub rows: Option<Rows>,
    /// `"*" = …`: every column the block doesn't name.
    pub every: Option<ColumnRule>,
    pub columns: Vec<ColumnRule>,
}

impl TableRules {
    /// The rule for `column`: its own, else `"*"`'s.
    pub fn column(&self, column: &str) -> Option<&Rule> {
        self.rule_for(column).map(|r| &r.rule)
    }

    pub fn rule_for(&self, column: &str) -> Option<&ColumnRule> {
        self.columns
            .iter()
            .find(|c| c.column == column)
            .or(self.every.as_ref())
    }

    fn qualified(&self) -> String {
        if self.key.contains('.') {
            self.key.clone()
        } else {
            format!("public.{}", self.key)
        }
    }
}

/// A rules file, parsed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleSet {
    pub large_objects_kept: bool,
    pub personal: Vec<String>,
    pub not_personal: Vec<String>,
    pub classify_reason: Option<String>,
    pub tables: Vec<TableRules>,
    /// Where it was read from, for messages.
    pub file: Option<PathBuf>,
}

impl RuleSet {
    /// The block for `t`, by `schema.name`, or by bare name in `public`.
    pub fn table(&self, t: &Table) -> Option<&TableRules> {
        let q = t.qualified();
        self.tables.iter().find(|r| r.qualified() == q)
    }

    /// What the rules name that the catalog doesn't have: a typo, or a
    /// table or column since renamed. Pure.
    pub fn unknown(&self, catalog: &Catalog) -> Vec<String> {
        let mut out = Vec::new();
        for r in &self.tables {
            match catalog.table(&r.qualified()) {
                None => out.push(format!(
                    "the rules name table {}, which this database doesn't have",
                    r.key
                )),
                Some(t) => {
                    for c in &r.columns {
                        if t.column(&c.column).is_none() {
                            out.push(format!(
                                "the rules name {}.{}, which this database doesn't have",
                                r.key, c.column
                            ));
                        }
                    }
                }
            }
        }
        out
    }
}

fn rule_named(name: &str, value: Option<&toml::Value>, days: Option<i64>) -> Result<Rule, String> {
    let text = || {
        value
            .and_then(toml::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("{name} needs value = \"…\""))
    };
    Ok(match name {
        "keep" => Rule::Keep,
        "null" => Rule::Null,
        "fixed" => Rule::Fixed(text()?),
        "password" => Rule::Password(
            value
                .and_then(toml::Value::as_str)
                .unwrap_or("")
                .to_string(),
        ),
        "hash" => Rule::Hash,
        "email" => Rule::Email,
        "name" => Rule::Name(NamePart::Full),
        "first_name" => Rule::Name(NamePart::First),
        "last_name" => Rule::Name(NamePart::Last),
        "phone" => Rule::Phone,
        "ip" => Rule::Ip,
        "postcode" => Rule::Postcode,
        "year" => Rule::Year,
        "date.shift" => {
            let d = days.unwrap_or(180);
            if !(1..=3650).contains(&d) {
                return Err(format!("date.shift days = {d}: give 1 to 3650"));
            }
            Rule::DateShift(d as u32)
        }
        other => {
            return Err(format!(
                "unknown rule {other:?}: keep, null, fixed, password, hash, email, name, \
                 first_name, last_name, phone, ip, postcode, year or date.shift"
            ))
        }
    })
}

fn column_rule(column: &str, v: &toml::Value) -> Result<ColumnRule, String> {
    let at = |e: String| format!("{column}: {e}");
    match v {
        toml::Value::String(s) => Ok(ColumnRule {
            column: column.to_string(),
            rule: rule_named(s, None, None).map_err(at)?,
            reason: None,
        }),
        toml::Value::Table(t) => {
            for k in t.keys() {
                if !matches!(k.as_str(), "rule" | "value" | "days" | "reason") {
                    return Err(at(format!("unknown key {k:?}: rule, value, days, reason")));
                }
            }
            let name = t
                .get("rule")
                .and_then(toml::Value::as_str)
                .ok_or_else(|| at("needs rule = \"…\"".into()))?;
            let days = match t.get("days") {
                Some(d) => Some(
                    d.as_integer()
                        .ok_or_else(|| at("days is a number".into()))?,
                ),
                None => None,
            };
            let reason = match t.get("reason") {
                Some(r) => Some(
                    r.as_str()
                        .filter(|s| !s.trim().is_empty())
                        .ok_or_else(|| at("reason is some words".into()))?
                        .to_string(),
                ),
                None => None,
            };
            Ok(ColumnRule {
                column: column.to_string(),
                rule: rule_named(name, t.get("value"), days).map_err(at)?,
                reason,
            })
        }
        _ => Err(at(
            "a rule is a name (\"email\") or a table ({ rule = … })".into()
        )),
    }
}

fn strings(v: Option<&toml::Value>, what: &str) -> Result<Vec<String>, String> {
    match v {
        None => Ok(Vec::new()),
        Some(toml::Value::Array(a)) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("{what} is a list of words"))
            })
            .collect(),
        Some(_) => Err(format!("{what} is a list of words")),
    }
}

/// Parse a rules file. Pure.
pub fn parse(text: &str) -> Result<RuleSet, String> {
    let doc: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut set = RuleSet::default();
    for (k, v) in &doc {
        match k.as_str() {
            "version" => {
                if v.as_integer() != Some(1) {
                    return Err("version = 1 is the only version there is".into());
                }
            }
            "large_objects" => {
                set.large_objects_kept = match v.as_str() {
                    Some("keep") => true,
                    Some("drop") => false,
                    _ => return Err("large_objects is \"keep\" or \"drop\"".into()),
                }
            }
            "classify" => {
                let t = v.as_table().ok_or("[classify] is a table")?;
                for key in t.keys() {
                    if !matches!(key.as_str(), "personal" | "not_personal" | "reason") {
                        return Err(format!("[classify]: unknown key {key:?}"));
                    }
                }
                set.personal = strings(t.get("personal"), "classify.personal")?;
                set.not_personal = strings(t.get("not_personal"), "classify.not_personal")?;
                set.classify_reason = t
                    .get("reason")
                    .and_then(toml::Value::as_str)
                    .map(str::to_string);
                if !set.not_personal.is_empty() && set.classify_reason.is_none() {
                    return Err(
                        "[classify] not_personal keeps columns the defaults would mask: give \
                         reason = \"…\" for the team that reads it"
                            .into(),
                    );
                }
            }
            "tables" => {
                let tables = v.as_table().ok_or("[tables] holds one table per name")?;
                for (name, body) in tables {
                    let body = body
                        .as_table()
                        .ok_or_else(|| format!("[tables.{name}] is a table of column rules"))?;
                    let mut tr = TableRules {
                        key: name.clone(),
                        ..TableRules::default()
                    };
                    for (col, rule) in body {
                        match col.as_str() {
                            "rows" => {
                                if rule.as_str() != Some("drop") {
                                    return Err(format!(
                                        "[tables.{name}] rows = \"drop\" is the only setting"
                                    ));
                                }
                                tr.rows = Some(Rows::Drop);
                            }
                            "*" => {
                                tr.every = Some(
                                    column_rule("*", rule)
                                        .map_err(|e| format!("[tables.{name}] {e}"))?,
                                )
                            }
                            _ => tr.columns.push(
                                column_rule(col, rule)
                                    .map_err(|e| format!("[tables.{name}] {e}"))?,
                            ),
                        }
                    }
                    set.tables.push(tr);
                }
            }
            other => return Err(format!("unknown key {other:?} in the rules file")),
        }
    }
    Ok(set)
}

/// `.pgman/masking/<database>.toml` above `start`, if there is one.
/// Reads the filesystem.
pub fn locate(start: &Path, database: &str) -> Option<PathBuf> {
    let root = crate::project::find_root(start)?;
    let p = root.join(".pgman/masking").join(format!("{database}.toml"));
    p.is_file().then_some(p)
}

/// The rules for `database`: `file` when given, else [`locate`]d, else
/// none (the defaults alone). A file that is there and can't be read is
/// an error. Reads files.
pub fn find(start: &Path, database: &str, file: Option<&Path>) -> Result<RuleSet, String> {
    let path = match file {
        Some(f) => Some(f.to_path_buf()),
        None => locate(start, database),
    };
    let Some(path) = path else {
        return Ok(RuleSet::default());
    };
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut set = parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    set.file = Some(path);
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rules_file_parses() {
        let set = parse(
            "version = 1\nlarge_objects = \"keep\"\n\
             [classify]\npersonal = [\"handle\"]\nnot_personal = [\"notes\"]\nreason = \"none here\"\n\
             [tables.customers]\nnotes = { rule = \"keep\", reason = \"codes\" }\ncontact = \"email\"\n\
             signup_on = { rule = \"date.shift\", days = 90 }\nsecret = { rule = \"fixed\", value = \"x\" }\n\
             \"*\" = \"null\"\n\
             [tables.\"audit.events\"]\nrows = \"drop\"\n",
        )
        .unwrap();
        assert!(set.large_objects_kept);
        assert_eq!(set.personal, ["handle"]);
        let c = set.tables.iter().find(|t| t.key == "customers").unwrap();
        let audit = set.tables.iter().find(|t| t.key == "audit.events").unwrap();
        assert_eq!(c.column("contact"), Some(&Rule::Email));
        assert_eq!(c.column("signup_on"), Some(&Rule::DateShift(90)));
        assert_eq!(c.column("secret"), Some(&Rule::Fixed("x".into())));
        assert_eq!(
            c.rule_for("notes").unwrap().reason.as_deref(),
            Some("codes")
        );
        assert_eq!(c.column("anything"), Some(&Rule::Null), "the * rule");
        assert_eq!(audit.rows, Some(Rows::Drop));
        assert_eq!(audit.qualified(), "audit.events");
        assert_eq!(c.qualified(), "public.customers");
    }

    #[test]
    fn mistakes_are_named() {
        for (text, says) in [
            ("version = 2\n", "version = 1"),
            ("colour = 1\n", "unknown key \"colour\""),
            (
                "[tables.t]\nc = \"scramble\"\n",
                "unknown rule \"scramble\"",
            ),
            ("[tables.t]\nc = { rule = \"fixed\" }\n", "needs value"),
            (
                "[tables.t]\nc = { rule = \"keep\", why = \"x\" }\n",
                "unknown key \"why\"",
            ),
            (
                "[tables.t]\nc = { rule = \"date.shift\", days = 0 }\n",
                "1 to 3650",
            ),
            ("[tables.t]\nrows = \"keep\"\n", "rows = \"drop\""),
            ("[classify]\nnot_personal = [\"x\"]\n", "give reason"),
            ("large_objects = \"maybe\"\n", "keep\" or \"drop"),
        ] {
            let e = parse(text).unwrap_err();
            assert!(e.contains(says), "{text}: {e}");
        }
    }

    #[test]
    fn rules_for_what_isnt_there_are_named() {
        use super::super::catalog::{Column, ColumnType};
        let cat = Catalog {
            tables: vec![Table {
                schema: "public".into(),
                name: "customers".into(),
                kind: "r".into(),
                columns: vec![Column {
                    num: 1,
                    name: "email".into(),
                    ty: ColumnType {
                        display: "text".into(),
                        base: "text".into(),
                        base_schema: "pg_catalog".into(),
                        category: "S".into(),
                        enum_first: None,
                        array_of: None,
                        max_len: None,
                        not_null: false,
                    },
                    generated: false,
                    unique: false,
                }],
                primary_key: vec![],
                foreign_keys: vec![],
                partition_key: vec![],
            }],
            large_objects: 0,
        };
        let set = parse("[tables.customers]\nemial = \"email\"\n[tables.custmers]\nx = \"keep\"\n")
            .unwrap();
        let u = set.unknown(&cat).join("\n");
        assert!(u.contains("customers.emial"), "{u}");
        assert!(u.contains("table custmers"), "{u}");
    }
}
