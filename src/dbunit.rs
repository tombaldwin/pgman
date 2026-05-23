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

/// How `generate_clean` empties the involved tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanMode {
    /// `TRUNCATE TABLE t RESTART IDENTITY CASCADE` — fast, resets sequences,
    /// cascades through FKs. Hits Postgres permissions and locks tables.
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
        .map(|t| match mode {
            CleanMode::Truncate => format!("TRUNCATE TABLE {t} RESTART IDENTITY CASCADE"),
            CleanMode::DeleteFrom => format!("DELETE FROM {t}"),
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

// -- helpers --

fn insert_sql(row: &Row) -> String {
    let cols: Vec<&str> = row.columns.iter().map(|(k, _)| k.as_str()).collect();
    let vals: Vec<String> = row
        .columns
        .iter()
        .map(|(_, v)| format!("'{}'", v.replace('\'', "''")))
        .collect();
    format!(
        "INSERT INTO {} ({}) VALUES ({})",
        row.table,
        cols.join(", "),
        vals.join(", ")
    )
}

fn element_name(e: &quick_xml::events::BytesStart<'_>) -> String {
    std::str::from_utf8(e.name().as_ref())
        .unwrap_or("")
        .to_string()
}

fn row_columns(e: &quick_xml::events::BytesStart<'_>) -> Vec<(String, String)> {
    let mut cols = Vec::new();
    for attr in e.attributes().flatten() {
        let key = std::str::from_utf8(attr.key.as_ref()).unwrap_or("").to_string();
        if key.is_empty() {
            continue;
        }
        let val = attr.unescape_value().ok().unwrap_or_default().to_string();
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
}
