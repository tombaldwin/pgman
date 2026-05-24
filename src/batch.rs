//! Batch / pipe mode — `pgman --batch --sql "…"` (or SQL via stdin)
//! runs the statement and emits the result to stdout in a chosen
//! format, then exits. No TUI; suitable for shell scripts and CI.
//!
//! The formatter functions ([`format_csv`], [`format_tsv`],
//! [`format_json`], [`format_expanded`]) are pure on [`Grid`] so they
//! get full unit-test coverage. The async I/O wrapper [`run`] is
//! kept thin.

use crate::conn::{self, Dsn, QueryErr};
use crate::grid::Grid;

/// Output formats. Mirrors the names psql exposes for `\pset format`
/// but pared to the four most useful for scripted output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Csv,
    Tsv,
    Json,
    Expanded,
}

impl Format {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "csv" => Ok(Format::Csv),
            "tsv" => Ok(Format::Tsv),
            "json" => Ok(Format::Json),
            "expanded" | "x" => Ok(Format::Expanded),
            other => Err(format!(
                "unknown --format {other:?}; expected csv | tsv | json | expanded"
            )),
        }
    }
}

/// CLI-shaped options for batch mode. Parsed in `main.rs`.
pub struct Opts {
    pub dsn: Dsn,
    pub sql: String,
    pub format: Format,
    pub read_only: bool,
    pub statement_timeout_ms: u64,
}

/// Connect, run `opts.sql`, write the formatted result to `stdout`.
/// Returns `Ok(0)` on success and the formatted error / `Ok(1)` on
/// failure so `main` can map it to a process exit code.
pub async fn run(opts: Opts) -> Result<i32, String> {
    // Discard notices in batch mode — they'd interleave with the
    // result on stdout. Surface them on stderr via tracing instead.
    let (notice_tx, mut notice_rx) =
        tokio::sync::mpsc::unbounded_channel::<conn::NoticeMsg>();
    tokio::spawn(async move {
        while let Some(n) = notice_rx.recv().await {
            eprintln!("[{}] {}", n.severity, n.message);
        }
    });

    let (client, _tunnel) = conn::connect_only(
        opts.dsn,
        opts.read_only,
        opts.statement_timeout_ms,
        notice_tx,
    )
    .await?;

    // Multi-statement input (`pgman --batch --sql 'BEGIN; …; COMMIT'`)
    // goes through the simple-query / batch path — `run_statement`
    // uses `client.prepare` under the hood, which the extended-query
    // protocol rejects for multi-command strings. `safety::split_statements`
    // is the same splitter the interactive editor uses.
    let statements = crate::safety::split_statements(&opts.sql);
    let result = if statements.len() > 1 {
        conn::run_batch(&client, &opts.sql).await
    } else {
        conn::run_statement(&client, &opts.sql).await
    };
    match result {
        Ok(grid) => {
            let text = match opts.format {
                Format::Csv => format_csv(&grid),
                Format::Tsv => format_tsv(&grid),
                Format::Json => format_json(&grid),
                Format::Expanded => format_expanded(&grid),
            };
            print!("{text}");
            // Ensure the output ends in a newline for shell pipelines
            // (some formats include trailing newlines already; only
            // add one when the format doesn't).
            if !text.ends_with('\n') {
                println!();
            }
            Ok(0)
        }
        Err(QueryErr { msg, .. }) => {
            eprintln!("error: {msg}");
            Ok(1)
        }
    }
}

/// RFC-4180-style CSV. Fields containing `,`, `"`, `\r`, or `\n` get
/// quoted; embedded `"` becomes `""`.
pub fn format_csv(grid: &Grid) -> String {
    let mut out = String::new();
    push_delim_row(&mut out, &grid.columns, ',', true);
    for row in &grid.rows {
        push_delim_row(&mut out, row, ',', true);
    }
    out
}

/// Tab-separated. No quoting; tabs and newlines inside values are
/// escaped (`\t`, `\n`) so each record stays a single line.
pub fn format_tsv(grid: &Grid) -> String {
    let mut out = String::new();
    push_delim_row(&mut out, &grid.columns, '\t', false);
    for row in &grid.rows {
        push_delim_row(&mut out, row, '\t', false);
    }
    out
}

fn push_delim_row(buf: &mut String, fields: &[String], delim: char, csv_quote: bool) {
    for (i, field) in fields.iter().enumerate() {
        if i > 0 {
            buf.push(delim);
        }
        if csv_quote {
            push_csv_field(buf, field);
        } else {
            push_tsv_field(buf, field);
        }
    }
    buf.push('\n');
}

fn push_csv_field(buf: &mut String, field: &str) {
    let needs_quote = field
        .chars()
        .any(|c| matches!(c, ',' | '"' | '\n' | '\r'));
    if needs_quote {
        buf.push('"');
        for c in field.chars() {
            if c == '"' {
                buf.push_str("\"\"");
            } else {
                buf.push(c);
            }
        }
        buf.push('"');
    } else {
        buf.push_str(field);
    }
}

fn push_tsv_field(buf: &mut String, field: &str) {
    for c in field.chars() {
        match c {
            '\t' => buf.push_str("\\t"),
            '\n' => buf.push_str("\\n"),
            '\r' => buf.push_str("\\r"),
            '\\' => buf.push_str("\\\\"),
            _ => buf.push(c),
        }
    }
}

/// JSON array of objects. Each row → `{ "col": "value", … }`. Values
/// are always strings (we don't have a type-aware path; grids carry
/// the rendered string form). Keys / strings are escaped per RFC 8259.
pub fn format_json(grid: &Grid) -> String {
    let mut out = String::from("[");
    for (ri, row) in grid.rows.iter().enumerate() {
        if ri > 0 {
            out.push(',');
        }
        out.push('{');
        for (ci, col) in grid.columns.iter().enumerate() {
            if ci > 0 {
                out.push(',');
            }
            push_json_string(&mut out, col);
            out.push(':');
            let value = row.get(ci).map(String::as_str).unwrap_or("");
            push_json_string(&mut out, value);
        }
        out.push('}');
    }
    out.push(']');
    out.push('\n');
    out
}

fn push_json_string(buf: &mut String, s: &str) {
    buf.push('"');
    for c in s.chars() {
        match c {
            '"' => buf.push_str("\\\""),
            '\\' => buf.push_str("\\\\"),
            '\n' => buf.push_str("\\n"),
            '\r' => buf.push_str("\\r"),
            '\t' => buf.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(buf, "\\u{:04x}", c as u32);
            }
            c => buf.push(c),
        }
    }
    buf.push('"');
}

/// psql `\x` expanded — one record per group, `col | value` lines
/// separated by a divider. Useful for rows with many columns or long
/// values, where a wide CSV/TSV becomes unreadable.
pub fn format_expanded(grid: &Grid) -> String {
    let mut out = String::new();
    // Column-name padding so the `|` separators line up within each
    // record. The widest column name wins.
    let col_w = grid
        .columns
        .iter()
        .map(|c| c.chars().count())
        .max()
        .unwrap_or(0);
    for (i, row) in grid.rows.iter().enumerate() {
        let header = format!("-[ RECORD {} ]", i + 1);
        out.push_str(&header);
        out.push('\n');
        for (col, value) in grid.columns.iter().zip(row.iter()) {
            let pad = col_w.saturating_sub(col.chars().count());
            out.push_str(col);
            for _ in 0..pad {
                out.push(' ');
            }
            out.push_str(" | ");
            out.push_str(value);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(columns: &[&str], rows: &[&[&str]]) -> Grid {
        Grid {
            columns: columns.iter().map(|s| (*s).to_string()).collect(),
            rows: rows
                .iter()
                .map(|r| r.iter().map(|s| (*s).to_string()).collect())
                .collect(),
        }
    }

    #[test]
    fn csv_quotes_fields_with_comma_or_quote_or_newline() {
        let g = grid(
            &["id", "note"],
            &[
                &["1", "plain"],
                &["2", "has, comma"],
                &["3", "has \"quote\""],
                &["4", "has\nnewline"],
            ],
        );
        let out = format_csv(&g);
        assert_eq!(
            out,
            "id,note\n1,plain\n2,\"has, comma\"\n3,\"has \"\"quote\"\"\"\n4,\"has\nnewline\"\n"
        );
    }

    #[test]
    fn csv_empty_grid_emits_header_only() {
        let g = grid(&["a", "b"], &[]);
        assert_eq!(format_csv(&g), "a,b\n");
    }

    #[test]
    fn tsv_escapes_control_chars_no_quoting() {
        let g = grid(
            &["a", "b"],
            &[&["x\ty", "z\nw"], &["back\\slash", "ok"]],
        );
        let out = format_tsv(&g);
        assert_eq!(out, "a\tb\nx\\ty\tz\\nw\nback\\\\slash\tok\n");
    }

    #[test]
    fn json_array_of_objects_keys_in_column_order() {
        let g = grid(
            &["id", "email"],
            &[&["1", "a@b.com"], &["2", "needs \"quote\""]],
        );
        let out = format_json(&g);
        assert_eq!(
            out,
            "[{\"id\":\"1\",\"email\":\"a@b.com\"},{\"id\":\"2\",\"email\":\"needs \\\"quote\\\"\"}]\n"
        );
    }

    #[test]
    fn json_escapes_control_chars() {
        let g = grid(&["x"], &[&["line\nwith\ttabs"]]);
        let out = format_json(&g);
        assert!(out.contains(r#""line\nwith\ttabs""#));
    }

    #[test]
    fn expanded_pads_column_names_per_record() {
        let g = grid(
            &["id", "much_longer_column"],
            &[&["1", "v1"], &["2", "v2"]],
        );
        let out = format_expanded(&g);
        // The shorter column name should be padded so that the `|`
        // separators line up.
        let first_record: Vec<&str> = out.lines().take(3).collect();
        assert_eq!(first_record[0], "-[ RECORD 1 ]");
        // `id` is 2 chars, `much_longer_column` is 18 → pad `id` to
        // 18 then ` | ` then value.
        assert_eq!(first_record[1], "id                 | 1");
        assert_eq!(first_record[2], "much_longer_column | v1");
    }

    #[test]
    fn format_parse_rejects_unknown_with_helpful_message() {
        let e = Format::parse("xml").unwrap_err();
        assert!(e.contains("xml"));
        assert!(e.contains("csv"));
    }

    #[test]
    fn format_parse_accepts_aliases() {
        assert_eq!(Format::parse("X").unwrap(), Format::Expanded);
        assert_eq!(Format::parse("Json").unwrap(), Format::Json);
    }

    #[test]
    fn run_routes_multistatement_through_batch_path() {
        // `safety::split_statements` is what `run` consults to
        // decide single-vs-batch. A guard: more than one statement
        // yields >= 2 entries so the multi-stmt branch fires.
        let many = "BEGIN; SELECT 1; COMMIT";
        assert!(crate::safety::split_statements(many).len() >= 2);
        let one = "SELECT 1";
        assert_eq!(crate::safety::split_statements(one).len(), 1);
    }
}
