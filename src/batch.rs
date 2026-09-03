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
use tokio_postgres::types::Type;

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
    /// The full safety config + the target database name, so the batch
    /// path enforces the SAME per-statement guard rails as the editor
    /// (not just `read_only` + `statement_timeout`).
    pub safety: crate::safety::SafetyConfig,
    pub db: String,
    /// Downgrade `Guard::Confirm` to "proceed" non-interactively (the
    /// `--yes` flag). `Guard::Block` stays blocked regardless.
    pub assume_yes: bool,
}

/// First line of a statement, trimmed and length-capped, for safety
/// messages — keeps a multi-line body from flooding the terminal.
fn stmt_summary(stmt: &str) -> String {
    let line = stmt.lines().next().unwrap_or("").trim();
    let mut s: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        s.push('…');
    }
    s
}

/// Check every statement in `sql` against the guard rails for `db`. Returns
/// `Err(message)` for the first statement a guard refuses: `Block` always
/// refuses; `Confirm` refuses unless `assume_yes` (the `--yes` flag). This is
/// the non-interactive analogue of the editor's classify→guard step, so a
/// `safety.toml` rule holds in CI exactly as it does in the TUI. Pure (no I/O)
/// so it's unit-tested.
///
/// On success it returns **the verified statements**, and the caller must run
/// those rather than the original string — otherwise the server executes text
/// the classifier never saw. A script `safety::split_verified` cannot account
/// for is refused outright: guards computed from the wrong statement
/// boundaries approve the wrong statements.
pub fn check_batch_safety(
    config: &crate::safety::SafetyConfig,
    db: &str,
    sql: &str,
    assume_yes: bool,
) -> Result<Vec<String>, String> {
    use crate::safety::Guard;
    let statements = crate::safety::split_verified(sql).map_err(|e| {
        tracing::warn!("batch: {e:?}");
        crate::safety::SPLIT_REFUSAL.to_string()
    })?;
    for stmt in &statements {
        let decision = crate::safety::evaluate(config, db, stmt);
        match decision.guard {
            Guard::Allow => {}
            Guard::Confirm if assume_yes => {}
            Guard::Confirm => {
                return Err(format!(
                    "blocked by safety: {} on '{}' would need confirmation \
                     — re-run with --yes to allow guarded writes in batch mode (statement: {})",
                    decision.kind.describe(),
                    db,
                    stmt_summary(stmt),
                ));
            }
            // Not a category guard: the profile asked for a read-only
            // session, and `--yes` does not buy a way out of it either.
            Guard::Block if decision.read_only_escape => {
                return Err(crate::safety::READ_ONLY_ESCAPE_REFUSAL.to_string());
            }
            Guard::Block => {
                return Err(format!(
                    "blocked by safety: {} on '{}' is set to block \
                     — change this guard to \"confirm\" in safety.toml to permit it (statement: {})",
                    decision.kind.describe(),
                    db,
                    stmt_summary(stmt),
                ));
            }
        }
    }
    Ok(statements)
}

/// Connect, run `opts.sql`, write the formatted result to `stdout`.
/// Returns `Ok(0)` on success and the formatted error / `Ok(1)` on
/// failure so `main` can map it to a process exit code.
pub async fn run(opts: Opts) -> Result<i32, String> {
    // Enforce the per-statement guard rails BEFORE connecting — a blocked
    // statement should never reach the server. `read_only` and
    // `statement_timeout` are applied server-side at connect; this adds the
    // category guards (drop / unqualified delete / …) the editor enforces.
    //
    // `checked` is what actually gets sent: the verified statements, re-joined.
    // Running `opts.sql` instead would let anything the splitter treated as one
    // statement — but the server treats as several — through unclassified.
    let checked = match check_batch_safety(&opts.safety, &opts.db, &opts.sql, opts.assume_yes) {
        Ok(statements) => statements,
        Err(msg) => {
            eprintln!("error: {msg}");
            return Ok(1);
        }
    };

    // Discard notices + notifications in batch mode — they'd
    // interleave with the result on stdout. Surface notices on
    // stderr; LISTEN/NOTIFY arrivals are silently dropped (a
    // one-shot batch isn't a sensible subscriber).
    let (notice_tx, mut notice_rx) = tokio::sync::mpsc::unbounded_channel::<conn::NoticeMsg>();
    // Keep the handle. A detached task here is a silent-loss bug: the
    // server sends NOTICE before CommandComplete, but nothing makes the
    // printer run before `run` returns and the process exits, so a
    // `RAISE WARNING` or a "relation already exists, skipping" can
    // vanish whenever the exit wins the race. Awaited below.
    let notice_task = tokio::spawn(async move {
        while let Some(n) = notice_rx.recv().await {
            eprintln!("[{}] {}", n.severity, n.message);
        }
    });
    let (notification_tx, mut notification_rx) =
        tokio::sync::mpsc::unbounded_channel::<conn::NotificationMsg>();
    tokio::spawn(async move {
        while notification_rx.recv().await.is_some() {
            // drop
        }
    });

    let (client, _tunnel) = conn::connect_only(
        opts.dsn,
        opts.read_only,
        opts.statement_timeout_ms,
        notice_tx,
        notification_tx,
    )
    .await?;

    // Multi-statement input (`pgman --batch --sql 'BEGIN; …; COMMIT'`)
    // goes through the simple-query / batch path — `run_statement`
    // uses `client.prepare` under the hood, which the extended-query
    // protocol rejects for multi-command strings. `safety::split_verified`
    // is the same splitter the interactive editor uses.
    let sql = checked.join(";\n");
    // `--format json` on a single statement gets the typed path
    // (`run_statement_typed_json`): SQL NULL, numbers, and booleans
    // stay distinct instead of collapsing through `Grid`'s
    // already-stringified cells (see that function's doc comment). A
    // multi-statement script has no single result set to type this
    // way, so it keeps going through the `Grid` + `format_json` path
    // below, same as every other format.
    let code = if opts.format == Format::Json && checked.len() == 1 {
        match run_statement_typed_json(&client, &sql).await {
            Ok(text) => {
                print!("{text}");
                0
            }
            Err(QueryErr { msg, .. }) => {
                eprintln!("error: {msg}");
                1
            }
        }
    } else {
        let result = if checked.len() > 1 {
            conn::run_batch(&client, &sql).await
        } else {
            conn::run_statement(&client, &sql).await
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
                0
            }
            Err(QueryErr { msg, .. }) => {
                eprintln!("error: {msg}");
                1
            }
        }
    };

    // Both exit paths come through here, because a notice matters just
    // as much on the failing one. Dropping the client and the tunnel
    // ends the connection task, which drops the last `notice_tx`, which
    // ends the printer loop above — so awaiting the handle drains
    // everything the server sent. Bounded, because a connection that
    // never finishes closing must not hang a batch run: on timeout we
    // return the exit code anyway and lose only what a detached task
    // would have lost regardless.
    drop(client);
    drop(_tunnel);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), notice_task).await;

    Ok(code)
}

/// RFC-4180-style CSV. Fields containing `,`, `"`, `\r`, or `\n` get
/// quoted; embedded `"` becomes `""`.
///
/// ```
/// use pgman::batch::format_csv;
/// use pgman::grid::Grid;
/// let g = Grid {
///     columns: vec!["id".into(), "name".into()],
///     rows: vec![vec!["1".into(), "has, comma".into()]],
///     truncated: false,
/// };
/// assert_eq!(format_csv(&g), "id,name\n1,\"has, comma\"\n");
/// ```
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
    let needs_quote = field.chars().any(|c| matches!(c, ',' | '"' | '\n' | '\r'));
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
///
/// ```
/// use pgman::batch::format_json;
/// use pgman::grid::Grid;
/// let g = Grid {
///     columns: vec!["id".into()],
///     rows: vec![vec!["1".into()], vec!["2".into()]],
///     truncated: false,
/// };
/// assert_eq!(format_json(&g), "[{\"id\":\"1\"},{\"id\":\"2\"}]\n");
/// ```
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

/// Run a single statement and render it as **typed** JSON — the
/// `Grid` path (`conn::run_statement` + [`format_json`]) always
/// stringifies every cell and, for TEXT columns, renders SQL NULL the
/// same way as an empty string, since `Grid` has already thrown the
/// distinction away by the time it reaches the formatter.
///
/// The actual database round trip (`Db` effect) is
/// [`conn::run_statement_typed`] — this function is the pure
/// rendering half, kept in `batch.rs` alongside the other format
/// writers so it gets the same unit-test coverage as
/// [`format_json`]/[`format_csv`]/etc.
async fn run_statement_typed_json(
    client: &tokio_postgres::Client,
    sql: &str,
) -> Result<String, QueryErr> {
    let typed = conn::run_statement_typed(client, sql).await?;
    Ok(render_typed_json(&typed))
}

/// The pure half of [`run_statement_typed_json`]: turn already-fetched
/// [`conn::TypedRows`] into a JSON array of objects, or (for a
/// non-row-returning statement) the same one-row "status" shape the
/// `Grid` path uses.
fn render_typed_json(typed: &conn::TypedRows) -> String {
    if let Some(affected) = typed.affected {
        let mut out = String::from("[{");
        push_json_string(&mut out, "status");
        out.push(':');
        push_json_string(&mut out, &format!("{affected} row(s) affected"));
        out.push_str("}]\n");
        return out;
    }
    let mut out = String::from("[");
    for (ri, row) in typed.rows.iter().enumerate() {
        if ri > 0 {
            out.push(',');
        }
        out.push('{');
        for (ci, (name, ty)) in typed.columns.iter().enumerate() {
            if ci > 0 {
                out.push(',');
            }
            push_json_string(&mut out, name);
            out.push(':');
            push_typed_json_cell(&mut out, ty, row.get(ci).and_then(|c| c.as_deref()));
        }
        out.push('}');
    }
    out.push_str("]\n");
    out
}

/// One cell of `run_statement_typed_json`'s output. `text` is the
/// Postgres text-wire rendering of the value, `None` for SQL NULL.
/// `ty` decides whether the cell becomes a bare JSON number/boolean
/// or a quoted string; everything not explicitly numeric or boolean
/// renders as a string, same as today's CSV/TSV/expanded formats.
fn push_typed_json_cell(buf: &mut String, ty: &Type, text: Option<&str>) {
    let Some(text) = text else {
        buf.push_str("null");
        return;
    };
    match *ty {
        Type::BOOL => match text {
            "t" => buf.push_str("true"),
            "f" => buf.push_str("false"),
            other => push_json_string(buf, other), // shouldn't happen; fail safe
        },
        Type::INT2 | Type::INT4 | Type::INT8 | Type::FLOAT4 | Type::FLOAT8 | Type::NUMERIC => {
            if is_json_number(text) {
                buf.push_str(text);
            } else {
                // NaN / Infinity / -Infinity — valid Postgres float
                // text, not a valid JSON number token.
                push_json_string(buf, text);
            }
        }
        _ => push_json_string(buf, text),
    }
}

/// `true` when `s` is a valid JSON `number` token (RFC 8259). Postgres's
/// text rendering of int2/int4/int8 and ordinary float4/float8/numeric
/// values already matches this grammar; the exceptions are the special
/// float spellings (`NaN`, `Infinity`, `-Infinity`), which this
/// correctly rejects so [`push_typed_json_cell`] falls back to a JSON
/// string instead of emitting invalid JSON.
fn is_json_number(s: &str) -> bool {
    let mut chars = s.chars().peekable();
    if chars.peek() == Some(&'-') {
        chars.next();
    }
    let mut saw_digit = false;
    while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
        chars.next();
        saw_digit = true;
    }
    if !saw_digit {
        return false;
    }
    if chars.peek() == Some(&'.') {
        chars.next();
        let mut saw_frac = false;
        while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
            chars.next();
            saw_frac = true;
        }
        if !saw_frac {
            return false;
        }
    }
    if matches!(chars.peek(), Some('e') | Some('E')) {
        chars.next();
        if matches!(chars.peek(), Some('+') | Some('-')) {
            chars.next();
        }
        let mut saw_exp = false;
        while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
            chars.next();
            saw_exp = true;
        }
        if !saw_exp {
            return false;
        }
    }
    chars.next().is_none()
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
            truncated: false,
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
        let g = grid(&["a", "b"], &[&["x\ty", "z\nw"], &["back\\slash", "ok"]]);
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

    // --- typed JSON cells (`run_statement_typed_json`'s pure half) ---

    #[test]
    fn typed_json_cell_null_is_bare_null_regardless_of_type() {
        for ty in [Type::TEXT, Type::INT4, Type::BOOL, Type::NUMERIC] {
            let mut out = String::new();
            push_typed_json_cell(&mut out, &ty, None);
            assert_eq!(out, "null", "type {ty:?}");
        }
    }

    #[test]
    fn typed_json_cell_empty_string_is_not_null() {
        // The exact conflation this fix is for: SQL NULL and '' must
        // render differently.
        let mut out = String::new();
        push_typed_json_cell(&mut out, &Type::TEXT, Some(""));
        assert_eq!(out, "\"\"");
    }

    #[test]
    fn typed_json_cell_renders_bool_as_json_boolean() {
        let mut out = String::new();
        push_typed_json_cell(&mut out, &Type::BOOL, Some("t"));
        assert_eq!(out, "true");

        let mut out = String::new();
        push_typed_json_cell(&mut out, &Type::BOOL, Some("f"));
        assert_eq!(out, "false");
    }

    #[test]
    fn typed_json_cell_renders_integers_and_floats_as_bare_numbers() {
        for (ty, text) in [
            (Type::INT2, "42"),
            (Type::INT4, "-7"),
            (Type::INT8, "9007199254740993"),
            (Type::FLOAT4, "1.5"),
            (Type::FLOAT8, "-3.25"),
            (Type::NUMERIC, "1.50"),
        ] {
            let mut out = String::new();
            push_typed_json_cell(&mut out, &ty, Some(text));
            assert_eq!(out, text, "type {ty:?}");
        }
    }

    #[test]
    fn typed_json_cell_falls_back_to_string_for_special_float_spellings() {
        // NaN / Infinity aren't valid JSON number tokens.
        for text in ["NaN", "Infinity", "-Infinity"] {
            let mut out = String::new();
            push_typed_json_cell(&mut out, &Type::FLOAT8, Some(text));
            assert_eq!(out, format!("\"{text}\""));
        }
    }

    #[test]
    fn typed_json_cell_renders_text_types_as_quoted_strings() {
        let mut out = String::new();
        push_typed_json_cell(&mut out, &Type::TEXT, Some("alice"));
        assert_eq!(out, "\"alice\"");
    }

    #[test]
    fn is_json_number_accepts_postgres_numeric_text() {
        for ok in [
            "0", "-1", "42", "1.5", "-3.14", "1e10", "1.5e-3", "0.0", "-0",
        ] {
            assert!(is_json_number(ok), "{ok} should be a JSON number");
        }
    }

    #[test]
    fn is_json_number_rejects_non_numbers_and_special_floats() {
        for bad in [
            "NaN",
            "Infinity",
            "-Infinity",
            "",
            "-",
            "1.",
            ".5",
            "abc",
            "1..0",
        ] {
            assert!(!is_json_number(bad), "{bad} should not be a JSON number");
        }
    }

    // --- render_typed_json: the pure half of run_statement_typed_json --

    #[test]
    fn render_typed_json_matches_the_worked_example() {
        // select null::text as a, '' as b, 42 as c, true as d, 1.5 as e
        let typed = conn::TypedRows {
            columns: vec![
                ("a".to_string(), Type::TEXT),
                ("b".to_string(), Type::TEXT),
                ("c".to_string(), Type::INT4),
                ("d".to_string(), Type::BOOL),
                ("e".to_string(), Type::NUMERIC),
            ],
            rows: vec![vec![
                None,
                Some("".to_string()),
                Some("42".to_string()),
                Some("t".to_string()),
                Some("1.5".to_string()),
            ]],
            affected: None,
        };
        assert_eq!(
            render_typed_json(&typed),
            "[{\"a\":null,\"b\":\"\",\"c\":42,\"d\":true,\"e\":1.5}]\n"
        );
    }

    #[test]
    fn render_typed_json_renders_a_non_row_statement_as_a_status_object() {
        let typed = conn::TypedRows {
            columns: Vec::new(),
            rows: Vec::new(),
            affected: Some(3),
        };
        assert_eq!(
            render_typed_json(&typed),
            "[{\"status\":\"3 row(s) affected\"}]\n"
        );
    }

    #[test]
    fn render_typed_json_multiple_rows() {
        let typed = conn::TypedRows {
            columns: vec![("id".to_string(), Type::INT4)],
            rows: vec![vec![Some("1".to_string())], vec![Some("2".to_string())]],
            affected: None,
        };
        assert_eq!(render_typed_json(&typed), "[{\"id\":1},{\"id\":2}]\n");
    }

    #[test]
    fn expanded_pads_column_names_per_record() {
        let g = grid(&["id", "much_longer_column"], &[&["1", "v1"], &["2", "v2"]]);
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
    fn batch_safety_allows_selects() {
        let cfg = crate::safety::SafetyConfig::default();
        assert!(check_batch_safety(&cfg, "db", "SELECT * FROM t", false).is_ok());
        // Multi-statement all-SELECT batch is fine too.
        assert!(check_batch_safety(&cfg, "db", "SELECT 1; SELECT 2", false).is_ok());
    }

    #[test]
    fn batch_safety_blocks_drop_even_with_yes() {
        // DROP defaults to Guard::Block — --yes must NOT override a block.
        let cfg = crate::safety::SafetyConfig::default();
        let err = check_batch_safety(&cfg, "db", "DROP TABLE legacy", true).unwrap_err();
        assert!(err.contains("block"), "got: {err}");
        assert!(err.contains("DROP"), "got: {err}");
    }

    #[test]
    fn batch_safety_confirm_requires_yes() {
        // INSERT defaults to Guard::Confirm: refused without --yes, allowed with.
        let cfg = crate::safety::SafetyConfig::default();
        let err = check_batch_safety(&cfg, "db", "INSERT INTO t VALUES (1)", false).unwrap_err();
        assert!(err.contains("--yes"), "got: {err}");
        assert!(check_batch_safety(&cfg, "db", "INSERT INTO t VALUES (1)", true).is_ok());
    }

    #[test]
    fn batch_safety_blocks_when_any_statement_is_blocked() {
        // A safe leading SELECT does not excuse a later DROP.
        let cfg = crate::safety::SafetyConfig::default();
        let err = check_batch_safety(&cfg, "db", "SELECT 1; DROP TABLE t", true).unwrap_err();
        assert!(err.contains("DROP"), "got: {err}");
    }

    #[test]
    fn batch_safety_dollar_quoted_function_is_one_statement() {
        // Regression for the split_statements dollar-quote fix: a CREATE
        // FUNCTION with a `;`-bearing body classifies as one DDL statement
        // (Confirm under default `ddl` guard), not a blocked DELETE fragment.
        let cfg = crate::safety::SafetyConfig::default();
        let sql =
            "CREATE FUNCTION f() RETURNS void AS $$ BEGIN DELETE FROM t; END; $$ LANGUAGE plpgsql";
        // Default ddl guard is Confirm → needs --yes, but is NOT a hard block.
        let err = check_batch_safety(&cfg, "db", sql, false).unwrap_err();
        assert!(err.contains("--yes"), "got: {err}");
        assert!(check_batch_safety(&cfg, "db", sql, true).is_ok());
    }

    #[test]
    fn batch_safety_sees_the_drop_hidden_by_a_dollar_in_an_identifier() {
        // The security-review reproduction, at the gate that let it through:
        // the splitter used to read `$b$` as a dollar-quote opener, so the
        // DROP arrived inside a fragment that classified as a SELECT.
        let cfg = crate::safety::SafetyConfig::default();
        let err = check_batch_safety(
            &cfg,
            "db",
            "SELECT 1; SELECT 1 AS a$b$c; DROP TABLE users",
            true,
        )
        .unwrap_err();
        assert!(err.contains("DROP"), "got: {err}");
    }

    #[test]
    fn batch_safety_sees_the_drop_hidden_by_a_quoted_identifier() {
        let cfg = crate::safety::SafetyConfig::default();
        let err = check_batch_safety(
            &cfg,
            "db",
            r#"SELECT 1; SELECT * FROM "a--b"; DROP TABLE users"#,
            true,
        )
        .unwrap_err();
        assert!(err.contains("DROP"), "got: {err}");
    }

    #[test]
    fn batch_safety_refuses_a_script_it_cannot_split() {
        // Fail closed: an unterminated literal means the boundaries are a
        // guess, so there is nothing to vouch for. --yes does not help.
        let cfg = crate::safety::SafetyConfig::default();
        for assume_yes in [false, true] {
            let err =
                check_batch_safety(&cfg, "db", "SELECT 1; SELECT 'oops", assume_yes).unwrap_err();
            assert_eq!(err, crate::safety::SPLIT_REFUSAL);
        }
    }

    #[test]
    fn batch_safety_returns_the_statements_it_checked() {
        // The caller runs these, not the original string — comments and the
        // original separators are gone, and what is left is exactly what was
        // classified.
        let cfg = crate::safety::SafetyConfig::default();
        let checked =
            check_batch_safety(&cfg, "db", "SELECT 1; -- note\nSELECT 2;", false).unwrap();
        assert_eq!(
            checked,
            vec!["SELECT 1".to_string(), "SELECT 2".to_string()]
        );
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
