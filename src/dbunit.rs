//! DBUnit fixture support.
//!
//! Parse a DBUnit FlatXmlDataSet (the most common format) and generate a SQL
//! script that:
//!   - cleans the tables involved (`TRUNCATE … RESTART IDENTITY CASCADE`, or
//!     `DELETE FROM`) — in reverse insertion order so FKs don't bite, then
//!   - inserts the rows in the order they appear in the fixture.
//!
//! A FlatXmlDataSet looks like:
//!
//! ```xml
//! <?xml version="1.0" encoding="UTF-8"?>
//! <dataset>
//!   <users id="1" name="Alice"/>
//!   <users id="2" name="Bob"/>
//!   <orders id="1" user_id="1" total="100.00"/>
//! </dataset>
//! ```
//!
//! Each non-root element is one row in the table named by the element;
//! attributes are columns. A missing attribute is treated as a missing column
//! (the INSERT just doesn't mention it, so the DB default applies).
//!
//! Names in the XML are read the way DBUnit reads them — case-insensitively:
//! `users`, `USERS` and `Users` all mean `users`. Only a name that can't be
//! a bare identifier (`order items`) is taken exactly. See
//! [`sql_ident_name`].

use quick_xml::events::Event;
use quick_xml::Reader;

/// One row, in the order columns appeared in the XML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub table: String,
    pub columns: Vec<(String, String)>,
}

/// A parsed fixture — just an ordered list of rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fixture {
    pub rows: Vec<Row>,
}

impl Fixture {
    /// Unique table names, in the order they first appear in the fixture.
    pub fn tables(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for r in &self.rows {
            if !seen.contains(&r.table) {
                seen.push(r.table.clone());
            }
        }
        seen
    }
}

/// How `generate_clean` empties the involved tables. Serialised
/// in the per-database safety profile (`safety.toml` /
/// `pgman.toml`) as `clean_mode = "truncate"` / `"delete_from"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanMode {
    /// `TRUNCATE TABLE t RESTART IDENTITY CASCADE` — fast, resets sequences,
    /// cascades through FKs. Hits Postgres permissions and locks tables.
    /// The default — matches DBUnit's own `CLEAN_INSERT` behaviour.
    #[default]
    Truncate,
    /// `DELETE FROM t` — row-by-row, slower, doesn't reset sequences, but
    /// works without TRUNCATE privilege and respects triggers.
    DeleteFrom,
}

/// Parse a FlatXmlDataSet body into a [`Fixture`].
pub fn parse_flat_xml(xml: &str) -> Result<Fixture, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut fixture = Fixture::default();
    let mut buf = Vec::new();
    let mut inside_dataset = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = element_name(&e);
                if name == "dataset" {
                    inside_dataset = true;
                } else if inside_dataset {
                    fixture.rows.push(Row {
                        table: name,
                        columns: row_columns(&e),
                    });
                }
            }
            Ok(Event::Empty(e)) => {
                let name = element_name(&e);
                if inside_dataset && name != "dataset" {
                    fixture.rows.push(Row {
                        table: name,
                        columns: row_columns(&e),
                    });
                }
            }
            Ok(Event::End(_)) => {}
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("xml parse error: {e}")),
            _ => {}
        }
        buf.clear();
    }
    Ok(fixture)
}

/// SQL `INSERT` for each row, in fixture order.
pub fn generate_inserts(fixture: &Fixture) -> Vec<String> {
    fixture.rows.iter().map(insert_sql).collect()
}

/// Cleanup SQL — one statement per involved table, in **reverse** insertion
/// order so FK-child tables are emptied before parents.
pub fn generate_clean(fixture: &Fixture, mode: CleanMode) -> Vec<String> {
    let mut tables = fixture.tables();
    tables.reverse();
    tables
        .iter()
        .map(|t| {
            let t = sql_table_name(t);
            match mode {
                CleanMode::Truncate => format!("TRUNCATE TABLE {t} RESTART IDENTITY CASCADE"),
                CleanMode::DeleteFrom => format!("DELETE FROM {t}"),
            }
        })
        .collect()
}

/// A complete script: comments, cleanup, then inserts. Each statement is
/// terminated with `;\n` so the result can be split or sent to
/// `client.batch_execute` directly.
pub fn generate_apply_script(fixture: &Fixture, mode: CleanMode) -> String {
    let mut out = String::new();
    out.push_str("-- clean (reverse-order, so FK children go first)\n");
    for stmt in generate_clean(fixture, mode) {
        out.push_str(&stmt);
        out.push_str(";\n");
    }
    out.push_str("\n-- insert fixture rows\n");
    for stmt in generate_inserts(fixture) {
        out.push_str(&stmt);
        out.push_str(";\n");
    }
    out
}

/// Render a [`Fixture`] back into FlatXmlDataSet form — the
/// inverse of [`parse_flat_xml`]. Each row becomes a
/// `<table col="val" .../>` element; attribute values are
/// XML-escaped (including newline / tab / CR as numeric entities)
/// so a round-trip back through [`parse_flat_xml`] reproduces the
/// fixture exactly. Per-row column order is preserved.
///
/// SQL NULL is an omitted attribute, flat XML's own NULL (a captured
/// row's NULL cells are left out by [`fixture_from_rows`]); an empty
/// string is `col=""`.
pub fn generate_flat_xml(fixture: &Fixture) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<dataset>\n");
    for row in &fixture.rows {
        out.push_str("  <");
        out.push_str(&xml_escape_name(&row.table));
        for (k, v) in &row.columns {
            out.push(' ');
            out.push_str(&xml_escape_name(k));
            out.push_str("=\"");
            out.push_str(&xml_escape_attr(v));
            out.push('"');
        }
        out.push_str("/>\n");
    }
    out.push_str("</dataset>\n");
    out
}

/// Build a single-table [`Fixture`] from a result grid — the
/// "capture current state" path. `columns` align with each row's
/// cells by position, and any cells beyond `columns` are ignored. A
/// NULL cell — and a short row's missing one — is left out of the
/// row, flat XML's NULL; an empty string stays, as `""`. Applying the
/// fixture with pgman then leaves an omitted column to its default:
/// NULL, unless the table gives it a `DEFAULT`. `table` is the
/// server's own name and is used verbatim as the element name (DBUnit
/// flat-XML convention — schema-qualify by hand if your setup needs
/// it).
///
/// Refused, rather than renamed, when the table name or a column
/// name would not read back as itself on apply — see
/// [`fixture_name_problem`]. A renamed element applies to some other
/// table, or none; a renamed attribute to a column the table lacks.
pub fn fixture_from_rows(
    table: &str,
    columns: &[String],
    rows: &[Vec<crate::grid::Cell>],
) -> Result<Fixture, String> {
    if let Some(why) = fixture_name_problem(table, true) {
        return Err(format!("table {table:?} can't be a fixture element: {why}"));
    }
    for column in columns {
        if let Some(why) = fixture_name_problem(column, false) {
            return Err(format!(
                "column {column:?} can't be a fixture attribute: {why}"
            ));
        }
    }
    let rows = rows
        .iter()
        .map(|cells| Row {
            table: table.to_string(),
            columns: columns
                .iter()
                .enumerate()
                .filter_map(|(i, name)| {
                    let text = cells.get(i)?.as_ref()?;
                    Some((name.clone(), text.clone()))
                })
                .collect(),
        })
        .collect();
    Ok(Fixture { rows })
}

/// How a fixture name (an element or attribute name) reads as a SQL
/// identifier on apply, with DBUnit's semantics: names are
/// case-insensitive. Any name shaped like a bare identifier folds to
/// lower case the way the server folds an unquoted one — `USERS`,
/// `Users` and `users` all mean `users`, and `<UserAccount Id="1">`
/// means table `useraccount`, column `id`, as Java projects write it.
/// Only a name that can't be a bare identifier (`order items`,
/// `Café`) is taken exactly, quoted. A reserved word is quoted after
/// folding (`user` → `"user"`): that is syntax, not case.
pub fn sql_ident_name(name: &str) -> String {
    if is_bare_identifier_shape(name) {
        crate::app::quote_ident(&name.to_ascii_lowercase())
    } else {
        crate::app::quote_ident(name)
    }
}

/// `[A-Za-z_][A-Za-z0-9_]*` — a name that, unquoted, the server reads
/// as an identifier (folded to lower case).
fn is_bare_identifier_shape(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

/// [`sql_ident_name`] for an element name, which may be
/// schema-qualified (`audit.events`, DBUnit's qualified-table-names
/// spelling): each side is read on its own.
pub fn sql_table_name(name: &str) -> String {
    match name.split_once('.') {
        Some((schema, table)) if !schema.is_empty() && !table.is_empty() => {
            format!("{}.{}", sql_ident_name(schema), sql_ident_name(table))
        }
        _ => sql_ident_name(name),
    }
}

/// Why `name` — a catalog name captured from a result, a table's
/// (`table`) or a column's — can't be written as a fixture name and
/// read back as itself, or `None` when it can. XML can't carry every
/// identifier (`?column?`, `a b`, `a"b`), a name with an upper-case
/// letter (`"PgRevMixed"`, `"createdAt"`) reads back folded — DBUnit
/// names are case-insensitive — and a table name with a dot reads back
/// as schema.table.
pub fn fixture_name_problem(name: &str, table: bool) -> Option<&'static str> {
    if !is_xml_name(name) {
        return Some("not a valid XML name");
    }
    if table && name.contains('.') {
        return Some("a dot reads back as schema.table");
    }
    if sql_ident_name(name) != crate::app::quote_ident(name) {
        return Some(
            "DBUnit fixtures name tables and columns case-insensitively, \
             so a name with upper-case letters can't round-trip",
        );
    }
    None
}

/// Is `s` an XML `Name` as `quick_xml` will parse it back? Letters
/// (any script), `_` and `:` may start it; digits, `-` and `.` may
/// follow. Approximates the XML 1.0 production with Rust's Unicode
/// classes — enough to keep every ordinary SQL identifier and refuse
/// the punctuation that breaks the markup.
fn is_xml_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if is_xml_name_start(c) => chars.all(is_xml_name_char),
        _ => false,
    }
}

fn is_xml_name_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == ':'
}

fn is_xml_name_char(c: char) -> bool {
    is_xml_name_start(c) || c.is_numeric() || c == '-' || c == '.'
}

// -- helpers --

/// Sanitise a string into a valid XML `Name` for use as an element or
/// attribute name, so [`generate_flat_xml`] never writes malformed
/// XML. Every disallowed character becomes `_`; a name that can't
/// *start* an XML name is prefixed with `_`. The identity on every
/// name [`is_xml_name`] accepts — which is every name a capture
/// ([`fixture_from_rows`]) lets through, since it refuses the rest
/// rather than have them renamed here — and on every name a parsed
/// fixture holds.
fn xml_escape_name(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| if is_xml_name_char(c) { c } else { '_' })
        .collect();
    let needs_prefix = out
        .chars()
        .next()
        .map(|c| !is_xml_name_start(c))
        .unwrap_or(true);
    if needs_prefix {
        out.insert(0, '_');
    }
    out
}

/// Escape a string for use inside a double-quoted XML attribute.
/// Whitespace controls become numeric entities so they survive
/// XML attribute-value normalisation on re-parse.
fn xml_escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            _ => out.push(c),
        }
    }
    out
}

fn insert_sql(row: &Row) -> String {
    // Every attribute omitted (`<t/>`, or a captured row of NULLs):
    // `INSERT INTO t () VALUES ()` is not SQL.
    if row.columns.is_empty() {
        return format!("INSERT INTO {} DEFAULT VALUES", sql_table_name(&row.table));
    }
    let cols: Vec<String> = row.columns.iter().map(|(k, _)| sql_ident_name(k)).collect();
    let vals: Vec<String> = row
        .columns
        .iter()
        .map(|(_, v)| crate::app::format_sql_literal(Some(v)))
        .collect();
    format!(
        "INSERT INTO {} ({}) VALUES ({})",
        sql_table_name(&row.table),
        cols.join(", "),
        vals.join(", ")
    )
}

fn element_name(e: &quick_xml::events::BytesStart<'_>) -> String {
    e.name().as_ref().to_string()
}

fn row_columns(e: &quick_xml::events::BytesStart<'_>) -> Vec<(String, String)> {
    let mut cols = Vec::new();
    for attr in e.attributes().flatten() {
        let key = attr.key.as_ref().to_string();
        if key.is_empty() {
            continue;
        }
        let val = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .ok()
            .unwrap_or_default()
            .to_string();
        cols.push((key, val));
    }
    cols
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<dataset>
  <users id="1" name="Alice"/>
  <users id="2" name="Bob"/>
  <orders id="1" user_id="1" total="100.00"/>
</dataset>"#
    }

    #[test]
    fn generate_then_parse_round_trips() {
        let original = parse_flat_xml(sample()).unwrap();
        let xml = generate_flat_xml(&original);
        let reparsed = parse_flat_xml(&xml).unwrap();
        assert_eq!(reparsed, original);
    }

    #[test]
    fn generate_escapes_special_chars_and_round_trips() {
        let f = Fixture {
            rows: vec![Row {
                table: "t".into(),
                columns: vec![
                    ("note".into(), "a&b<c>d\"e".into()),
                    ("multi".into(), "line1\nline2\tcol".into()),
                ],
            }],
        };
        let xml = generate_flat_xml(&f);
        // Entities present, no raw special chars leaked into the attr.
        assert!(xml.contains("&amp;") && xml.contains("&lt;") && xml.contains("&quot;"));
        assert!(xml.contains("&#10;") && xml.contains("&#9;"));
        // Exact round-trip including whitespace controls.
        assert_eq!(parse_flat_xml(&xml).unwrap(), f);
    }

    #[test]
    fn fixture_from_rows_aligns_columns_and_leaves_out_nulls() {
        let cols = vec!["id".to_string(), "name".to_string()];
        let rows = vec![
            vec![Some("1".to_string()), Some("alice".to_string())],
            vec![Some("2".to_string())], // short row → name is NULL
            vec![Some("3".to_string()), None],
            vec![Some("4".to_string()), Some(String::new())],
        ];
        let f = fixture_from_rows("users", &cols, &rows).unwrap();
        assert_eq!(f.rows.len(), 4);
        assert_eq!(f.rows[0].table, "users");
        assert_eq!(
            f.rows[0].columns,
            vec![("id".into(), "1".into()), ("name".into(), "alice".into())]
        );
        assert_eq!(f.rows[1].columns, vec![("id".into(), "2".into())]);
        assert_eq!(f.rows[2].columns, vec![("id".into(), "3".into())]);
        // The empty string is a value: it stays.
        assert_eq!(
            f.rows[3].columns,
            vec![("id".into(), "4".into()), ("name".into(), String::new())]
        );
        let xml = generate_flat_xml(&f);
        assert!(xml.contains("<users id=\"3\"/>"), "{xml}");
        assert!(xml.contains("<users id=\"4\" name=\"\"/>"), "{xml}");
        assert_eq!(parse_flat_xml(&xml).unwrap(), f);
    }

    /// A row of nothing but NULLs captures as `<t/>`; applying it must
    /// still be SQL.
    #[test]
    fn a_row_with_no_attributes_inserts_default_values() {
        let f = fixture_from_rows("t", &["a".to_string()], &[vec![None]]).unwrap();
        assert_eq!(
            generate_flat_xml(&f),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<dataset>\n  <t/>\n</dataset>\n"
        );
        assert_eq!(generate_inserts(&f), vec!["INSERT INTO t DEFAULT VALUES"]);
    }

    /// DBUnit names are case-insensitive: any bare-identifier-shaped
    /// element folds, whatever its case — `<UserAccount Id="1">` is table
    /// `useraccount` — in the INSERT and in both clean modes. A reserved
    /// word is still quoted; a name that can't be bare is taken exactly.
    #[test]
    fn apply_reads_names_case_insensitively_as_dbunit_does() {
        let f = parse_flat_xml(r#"<dataset><UserAccount Id="1" FullName="x"/></dataset>"#).unwrap();
        assert_eq!(
            generate_inserts(&f),
            vec!["INSERT INTO useraccount (id, fullname) VALUES ('1', 'x')"]
        );
        assert_eq!(
            generate_clean(&f, CleanMode::DeleteFrom),
            vec!["DELETE FROM useraccount"]
        );
        assert_eq!(
            generate_clean(&f, CleanMode::Truncate),
            vec!["TRUNCATE TABLE useraccount RESTART IDENTITY CASCADE"]
        );
        assert_eq!(sql_table_name("USERS"), "users");
        assert_eq!(sql_table_name("users"), "users");
        assert_eq!(sql_table_name("PgRevMixed"), "pgrevmixed");
        assert_eq!(sql_table_name("user"), "\"user\"");
        assert_eq!(sql_table_name("audit.events"), "audit.events");
        assert_eq!(sql_table_name("AUDIT.PgEvents"), "audit.pgevents");
        assert_eq!(sql_table_name("Café"), "\"Café\"");
        let f = parse_flat_xml("<dataset><USER ID=\"1\"/></dataset>").unwrap();
        assert_eq!(
            generate_inserts(&f)[0],
            "INSERT INTO \"user\" (id) VALUES ('1')"
        );
    }

    /// A table name that would not read back as itself is refused, not
    /// renamed into some other table.
    #[test]
    fn capture_refuses_a_table_name_that_does_not_round_trip() {
        for bad in ["weird table", "USERS", "PgRevMixed", "a.b", "?column?", ""] {
            assert!(
                fixture_from_rows(bad, &["id".to_string()], &[]).is_err(),
                "{bad:?} must be refused"
            );
        }
        let err = fixture_from_rows("PgRevMixed", &["id".to_string()], &[]).unwrap_err();
        assert!(err.contains("case-insensitively"), "{err}");
        // A reserved lowercase word is not a case problem: it is quoted
        // on apply and round-trips.
        assert!(fixture_from_rows("user", &["id".to_string()], &[]).is_ok());
        // Accepted means written as is: not renamed to `Caf__1` on the
        // way out, which applied to a table that isn't there.
        let f = fixture_from_rows("Café_1", &["prénom".to_string()], &[vec![Some("x".into())]])
            .unwrap();
        let xml = generate_flat_xml(&f);
        assert!(xml.contains("<Café_1 prénom=\"x\"/>"), "{xml}");
        assert_eq!(parse_flat_xml(&xml).unwrap(), f);
    }

    /// Reserved and dotted column names round-trip: capture, XML, parse,
    /// apply names the same columns, quoted where they must be. Bare,
    /// `user` was a syntax error.
    #[test]
    fn column_names_round_trip_through_capture_and_apply() {
        let cols = ["id", "user", "a.b"].map(String::from);
        let row = vec![
            Some("1".to_string()),
            Some("u".to_string()),
            Some("d".to_string()),
        ];
        let f = fixture_from_rows("t", &cols, &[row]).unwrap();
        let reparsed = parse_flat_xml(&generate_flat_xml(&f)).unwrap();
        assert_eq!(reparsed, f);
        assert_eq!(
            generate_inserts(&reparsed),
            vec!["INSERT INTO t (id, \"user\", \"a.b\") VALUES ('1', 'u', 'd')"]
        );
        // A hand-written uppercase fixture still means the lowercase columns.
        let f = parse_flat_xml("<dataset><USERS ID=\"1\" FULL_NAME=\"x\"/></dataset>").unwrap();
        assert_eq!(
            generate_inserts(&f),
            vec!["INSERT INTO users (id, full_name) VALUES ('1', 'x')"]
        );
    }

    /// The apply script writes values as the yank does: a backslash in an
    /// `E'…'` string, doubled, so `standard_conforming_strings` can't
    /// change what it inserts.
    #[test]
    fn apply_writes_a_backslash_as_an_escape_string() {
        let f = parse_flat_xml(r#"<dataset><t b="\x0102" s="it's"/></dataset>"#).unwrap();
        assert_eq!(
            generate_inserts(&f),
            vec!["INSERT INTO t (b, s) VALUES (E'\\\\x0102', 'it''s')"]
        );
    }

    /// A column XML can't carry, or one that would read back as another
    /// column, refuses the capture and names the column — no `_` rename.
    #[test]
    fn capture_refuses_a_column_name_that_does_not_round_trip() {
        for bad in ["a\"b", "with space", "?column?", "ID", "createdAt", "1st"] {
            let err =
                fixture_from_rows("t", &["id".to_string(), bad.to_string()], &[]).expect_err(bad);
            assert!(err.contains(&format!("column {bad:?}")), "{err}");
        }
    }

    #[test]
    fn fixture_from_rows_then_generate_is_parseable() {
        let cols = vec!["id".to_string(), "name".to_string()];
        let rows = vec![vec![Some("1".to_string()), Some("O'Brien".to_string())]];
        let f = fixture_from_rows("users", &cols, &rows).unwrap();
        let xml = generate_flat_xml(&f);
        let parsed = parse_flat_xml(&xml).unwrap();
        assert_eq!(parsed, f);
        // Apostrophe needs no escaping inside a double-quoted attr.
        assert!(xml.contains("name=\"O'Brien\""));
    }

    #[test]
    fn special_chars_in_names_are_sanitised_to_valid_xml() {
        // Regression: column/table names were emitted raw, so an
        // unaliased expression header (`?column?`) or a spaced/`&` alias
        // produced malformed XML. Names must sanitise to valid XML names
        // and still parse back.
        let f = Fixture {
            rows: vec![Row {
                table: "weird table".into(),
                columns: vec![
                    ("?column?".into(), "2".into()),
                    ("a&b".into(), "x".into()),
                    ("1leading".into(), "y".into()),
                ],
            }],
        };
        let xml = generate_flat_xml(&f);
        // No raw illegal name chars leaked into element/attribute names.
        assert!(!xml.contains("?column?"));
        assert!(!xml.contains("weird table"));
        assert!(!xml.contains("a&b"));
        // Valid XML now round-trips without a parse error.
        let parsed = parse_flat_xml(&xml).expect("sanitised names must parse");
        assert_eq!(parsed.rows.len(), 1);
        assert_eq!(parsed.rows[0].table, "weird_table");
        let names: Vec<&str> = parsed.rows[0]
            .columns
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        assert_eq!(names, vec!["_column_", "a_b", "_1leading"]);
    }

    #[test]
    fn ordinary_identifiers_pass_through_name_sanitiser() {
        assert_eq!(xml_escape_name("user_id"), "user_id");
        assert_eq!(xml_escape_name("created_at"), "created_at");
        assert_eq!(xml_escape_name(""), "_");
    }

    #[test]
    fn generate_empty_fixture_is_just_dataset_wrapper() {
        let xml = generate_flat_xml(&Fixture::default());
        assert!(xml.contains("<dataset>"));
        assert!(xml.contains("</dataset>"));
        assert_eq!(parse_flat_xml(&xml).unwrap(), Fixture::default());
    }

    #[test]
    fn parse_picks_up_each_row() {
        let f = parse_flat_xml(sample()).unwrap();
        assert_eq!(f.rows.len(), 3);
        assert_eq!(f.rows[0].table, "users");
        assert_eq!(f.rows[2].table, "orders");
        assert_eq!(f.rows[0].columns.len(), 2);
        assert_eq!(f.rows[2].columns.len(), 3);
    }

    #[test]
    fn tables_preserves_first_appearance_order() {
        let f = parse_flat_xml(sample()).unwrap();
        assert_eq!(f.tables(), vec!["users".to_string(), "orders".to_string()]);
    }

    #[test]
    fn generate_inserts_produces_one_per_row_with_quoted_values() {
        let f = parse_flat_xml(sample()).unwrap();
        let inserts = generate_inserts(&f);
        assert_eq!(inserts.len(), 3);
        assert_eq!(
            inserts[0],
            "INSERT INTO users (id, name) VALUES ('1', 'Alice')"
        );
        assert_eq!(
            inserts[2],
            "INSERT INTO orders (id, user_id, total) VALUES ('1', '1', '100.00')"
        );
    }

    #[test]
    fn generate_clean_reverses_table_order() {
        let f = parse_flat_xml(sample()).unwrap();
        // Insertion order: users, orders. Cleanup must do orders first.
        let truncate = generate_clean(&f, CleanMode::Truncate);
        assert_eq!(truncate.len(), 2);
        assert!(truncate[0].starts_with("TRUNCATE TABLE orders"));
        assert!(truncate[1].starts_with("TRUNCATE TABLE users"));

        let delete = generate_clean(&f, CleanMode::DeleteFrom);
        assert_eq!(delete[0], "DELETE FROM orders");
        assert_eq!(delete[1], "DELETE FROM users");
    }

    #[test]
    fn apply_script_combines_clean_and_inserts_with_terminators() {
        let f = parse_flat_xml(sample()).unwrap();
        let script = generate_apply_script(&f, CleanMode::Truncate);
        assert!(script.contains("TRUNCATE TABLE orders"));
        assert!(script.contains("TRUNCATE TABLE users"));
        assert!(script.contains("INSERT INTO users"));
        assert!(script.contains("INSERT INTO orders"));
        // every line ends with `;\n` (modulo the comment lines and trailing blank)
        assert!(script.matches(";\n").count() >= 5);
    }

    #[test]
    fn values_with_single_quotes_are_doubled() {
        let xml = r#"<dataset><users id="1" name="O'Brien"/></dataset>"#;
        let f = parse_flat_xml(xml).unwrap();
        let inserts = generate_inserts(&f);
        assert_eq!(
            inserts[0],
            "INSERT INTO users (id, name) VALUES ('1', 'O''Brien')"
        );
    }

    #[test]
    fn xml_parse_error_surfaces() {
        // An invalid `<` makes the parser barf.
        let xml = "<dataset><users id=<></dataset>";
        assert!(parse_flat_xml(xml).is_err());
    }

    /// The 0.36 -> 0.42 quick-xml bump changed how attribute values
    /// decode. 0.36 only resolved entities; 0.42 also performs XML
    /// attribute-value normalisation, under which a *literal* newline
    /// or tab inside an attribute collapses to a single space.
    ///
    /// Note this is a property of the *version*, not of which method
    /// you call: 0.42's `unescape_value()` is a deprecated shim that
    /// delegates to `normalized_value(Implicit1_0)` and normalises too.
    /// Worth stating because it makes the obvious mutation — swap the
    /// call back — semantically inert, and an inert mutation looks
    /// exactly like a test that cannot fail.
    ///
    /// It does not affect our own files, which is why the existing
    /// round-trip test still passes: `generate_flat_xml` writes
    /// whitespace as the character references `&#10;` / `&#9;`, and the
    /// spec explicitly exempts character references from that collapse.
    ///
    /// It does change how we read a fixture some *other* tool wrote
    /// with a raw newline in an attribute. That is a behaviour change
    /// worth pinning rather than shipping silently — and the new
    /// behaviour is the spec-conformant one, so this pins the fix, not
    /// a regression.
    #[test]
    fn literal_whitespace_in_an_attribute_normalises_but_char_refs_survive() {
        // A raw newline, as an external tool might emit.
        let external = "<dataset>\n  <t note=\"line1\nline2\" />\n</dataset>";
        let got = parse_flat_xml(external).expect("parse");
        assert_eq!(
            got.rows[0].columns[0].1, "line1 line2",
            "a LITERAL newline in an attribute must normalise to a space (XML spec)"
        );

        // The same content written by us, via character references.
        let ours = "<dataset>\n  <t note=\"line1&#10;line2\" />\n</dataset>";
        let got = parse_flat_xml(ours).expect("parse");
        assert_eq!(
            got.rows[0].columns[0].1, "line1\nline2",
            "a CHARACTER REFERENCE must survive normalisation intact — this is \
             what keeps our own fixtures round-tripping"
        );
    }
}
