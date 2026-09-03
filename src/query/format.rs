//! Built-in SQL formatting for the editor's Ctrl-F.
//!
//! Wraps the `sqlformat` crate (the Rust port of sql-formatter that
//! sqlx uses) behind two knobs — indent width and keyword case — and a
//! hard rule: **formatting never changes what a statement means**.
//! String literals, quoted identifiers and comments must come out
//! byte-for-byte; a dollar-quoted body (`$$ … $$`, a function body)
//! is not handed to the formatter at all. Both are enforced here, not
//! trusted: the input is lexed with `safety::scan` before formatting
//! and the output re-lexed after, and any literal that moved makes
//! the whole format a refusal rather than a corruption.
//!
//! `pg_format`, when it is on `PATH`, is used instead of this module
//! (see `App::reformat_buffer`). Formatting only ever happens on
//! Ctrl-F — never on run, never on paste.

use serde::Deserialize;

use crate::safety::{scan, SpanKind};

/// What to do with reserved words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeywordCase {
    /// `select` → `SELECT` (the default).
    #[default]
    Upper,
    /// `SELECT` → `select`.
    Lower,
    /// Leave keywords as typed.
    Preserve,
}

/// The two knobs the `[editor]` table of `.pgman/pgman.toml` exposes.
/// `indent` is shared with auto-indent on Enter (`app::editor`), so
/// a formatted buffer and a hand-typed one line up the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatOptions {
    /// Spaces per indent level.
    pub indent: u8,
    pub keywords: KeywordCase,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            indent: 2,
            keywords: KeywordCase::Upper,
        }
    }
}

/// Why [`format_sql`] declined to touch the input. The `Display` text
/// is the footer status the operator sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatSkipped {
    /// The input carries a `$tag$ … $tag$` body. `sqlformat` has no
    /// notion of dollar quoting and reflows the body as SQL.
    DollarQuoted,
    /// The formatter's output is not the input token-for-token: a
    /// literal, quoted name or comment changed, or a code token was
    /// split or joined. This is the last-line check that turns a
    /// formatter quirk into a no-op instead of a changed statement —
    /// `e'a\'b'` → `e 'a\'b'` and `a$b` → `a $b` are two it catches.
    WouldAlter,
}

impl std::fmt::Display for FormatSkipped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            FormatSkipped::DollarQuoted => "formatting skipped: dollar-quoted body",
            FormatSkipped::WouldAlter => "formatting skipped: would alter the statement",
        })
    }
}

/// Format `sql` with the built-in formatter. Refuses — leaving the
/// input untouched is then the caller's job — when the input has a
/// dollar-quoted body, or when the output is not the input
/// token-for-token (see [`fingerprint`]).
pub fn format_sql(sql: &str, opts: &FormatOptions) -> Result<String, FormatSkipped> {
    let before = fingerprint(sql);
    if before
        .iter()
        .any(|(kind, _)| *kind == SpanKind::DollarQuoted)
    {
        return Err(FormatSkipped::DollarQuoted);
    }
    let options = sqlformat::FormatOptions {
        indent: sqlformat::Indent::Spaces(opts.indent),
        uppercase: match opts.keywords {
            KeywordCase::Upper => Some(true),
            KeywordCase::Lower => Some(false),
            KeywordCase::Preserve => None,
        },
        dialect: sqlformat::Dialect::PostgreSql,
        ..sqlformat::FormatOptions::default()
    };
    let out = sqlformat::format(sql, &sqlformat::QueryParams::None, &options);
    if fingerprint(&out) != before {
        return Err(FormatSkipped::WouldAlter);
    }
    Ok(out)
}

/// What a statement *says*, with its layout removed: every literal,
/// quoted identifier and comment verbatim, and every code token —
/// a run of identifier characters (`$` included: `a$b` is one name
/// to Postgres, `$1` one parameter) lower-cased, or a single
/// punctuation character. Two inputs with equal fingerprints differ
/// only in whitespace and keyword case, which is all a formatter
/// may change.
fn fingerprint(sql: &str) -> Vec<(SpanKind, String)> {
    let mut out = Vec::new();
    for span in scan(sql) {
        let text = &sql[span.start..span.end];
        if span.kind != SpanKind::Code {
            out.push((span.kind, text.to_string()));
            continue;
        }
        let mut word = String::new();
        for c in text.chars() {
            if c.is_alphanumeric() || c == '_' || c == '$' {
                word.extend(c.to_lowercase());
                continue;
            }
            if !word.is_empty() {
                out.push((SpanKind::Code, std::mem::take(&mut word)));
            }
            if !c.is_whitespace() {
                out.push((SpanKind::Code, c.to_string()));
            }
        }
        if !word.is_empty() {
            out.push((SpanKind::Code, word));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(indent: u8, keywords: KeywordCase) -> FormatOptions {
        FormatOptions { indent, keywords }
    }

    #[test]
    fn defaults_are_two_spaces_and_upper() {
        assert_eq!(FormatOptions::default(), opts(2, KeywordCase::Upper));
    }

    #[test]
    fn indent_knob_sets_the_width() {
        let two = format_sql("select a from t", &opts(2, KeywordCase::Upper)).unwrap();
        let four = format_sql("select a from t", &opts(4, KeywordCase::Upper)).unwrap();
        assert_eq!(two, "SELECT\n  a\nFROM\n  t");
        assert_eq!(four, "SELECT\n    a\nFROM\n    t");
    }

    #[test]
    fn keyword_knob_upper_lower_preserve() {
        let sql = "Select a From t";
        assert_eq!(
            format_sql(sql, &opts(2, KeywordCase::Upper)).unwrap(),
            "SELECT\n  a\nFROM\n  t"
        );
        assert_eq!(
            format_sql(sql, &opts(2, KeywordCase::Lower)).unwrap(),
            "select\n  a\nfrom\n  t"
        );
        assert_eq!(
            format_sql(sql, &opts(2, KeywordCase::Preserve)).unwrap(),
            "Select\n  a\nFrom\n  t"
        );
    }

    /// A string literal that happens to contain SQL is data: no
    /// reflow, no case change, byte-for-byte.
    #[test]
    fn string_literal_containing_sql_survives_byte_for_byte() {
        let sql = "select a from t where x = 'select  b from  U' and y = 'it''s'";
        let out = format_sql(sql, &FormatOptions::default()).unwrap();
        assert!(out.contains("'select  b from  U'"), "got: {out}");
        assert!(out.contains("'it''s'"), "got: {out}");
        assert_eq!(fingerprint(&out), fingerprint(sql));
    }

    #[test]
    fn quoted_identifier_keeps_its_case_and_spacing() {
        let sql = "select \"Quoted  Name\", \"from\" from \"My Table\"";
        let out = format_sql(sql, &opts(2, KeywordCase::Lower)).unwrap();
        assert!(out.contains("\"Quoted  Name\""), "got: {out}");
        assert!(out.contains("\"from\""), "got: {out}");
        assert!(out.contains("\"My Table\""), "got: {out}");
        assert_eq!(fingerprint(&out), fingerprint(sql));
    }

    #[test]
    fn line_comment_survives_verbatim() {
        let sql = "select a -- the SELECT list, kept as-is\nfrom t";
        let out = format_sql(sql, &FormatOptions::default()).unwrap();
        assert!(out.contains("-- the SELECT list, kept as-is"), "got: {out}");
        assert_eq!(fingerprint(&out), fingerprint(sql));
    }

    /// `sqlformat` has no notion of dollar quoting: it reflowed
    /// `$$\n  select   1;\n$$` into `$$\nSELECT\n  1;\n$$` when probed.
    /// A function body is code in another language as far as the
    /// outer statement is concerned, so the whole buffer is refused.
    #[test]
    fn dollar_quoted_body_is_never_formatted() {
        let body = "$$\n  select   1;\n  return  x;\n$$";
        let sql = format!("create function f() returns int as {body} language plpgsql");
        assert_eq!(
            format_sql(&sql, &FormatOptions::default()),
            Err(FormatSkipped::DollarQuoted)
        );
        // Tagged form too.
        let sql = "do $fn$ begin  perform 1; end $fn$";
        assert_eq!(
            format_sql(sql, &FormatOptions::default()),
            Err(FormatSkipped::DollarQuoted)
        );
        assert_eq!(
            FormatSkipped::DollarQuoted.to_string(),
            "formatting skipped: dollar-quoted body"
        );
    }

    /// A `$` that is not a dollar-quote — a positional parameter —
    /// does not trip the refusal.
    #[test]
    fn positional_parameters_are_not_dollar_quotes() {
        let out = format_sql("select $1 from t where c = $2", &FormatOptions::default()).unwrap();
        assert_eq!(out, "SELECT\n  $1\nFROM\n  t\nWHERE\n  c = $2");
    }

    /// `sqlformat` splits `a$b` into `a $b` — one identifier becomes
    /// two tokens. The fingerprint check refuses rather than emit it.
    #[test]
    fn identifier_with_dollar_is_refused_not_split() {
        assert_eq!(
            format_sql("select a$b from t", &FormatOptions::default()),
            Err(FormatSkipped::WouldAlter)
        );
    }

    /// `sqlformat` splits `e'a\'b'` into `e 'a\'b'` — an escape-string
    /// literal becomes an identifier plus a plain string, and the
    /// plain string now ends at the `\'`. (Upper-case `E'…'` survives;
    /// the operator should not have to know which.)
    #[test]
    fn escape_string_mangling_is_refused_not_emitted() {
        assert_eq!(
            format_sql("select e'a\\'b' from t", &FormatOptions::default()),
            Err(FormatSkipped::WouldAlter)
        );
        let out = format_sql("select E'a\\'b' from t", &FormatOptions::default()).unwrap();
        assert!(out.contains("E'a\\'b'"), "got: {out}");
        assert_eq!(
            FormatSkipped::WouldAlter.to_string(),
            "formatting skipped: would alter the statement"
        );
    }

    #[test]
    fn fingerprint_keeps_literals_and_splits_code_into_tokens() {
        let fp = fingerprint("Select 'a', t.x$y -- c\nfrom \"b\" where x=$1");
        let want: Vec<(SpanKind, String)> = vec![
            (SpanKind::Code, "select".into()),
            (SpanKind::String, "'a'".into()),
            (SpanKind::Code, ",".into()),
            (SpanKind::Code, "t".into()),
            (SpanKind::Code, ".".into()),
            (SpanKind::Code, "x$y".into()),
            (SpanKind::LineComment, "-- c".into()),
            (SpanKind::Code, "from".into()),
            (SpanKind::Ident, "\"b\"".into()),
            (SpanKind::Code, "where".into()),
            (SpanKind::Code, "x".into()),
            (SpanKind::Code, "=".into()),
            (SpanKind::Code, "$1".into()),
        ];
        assert_eq!(fp, want);
        // Whitespace and keyword case are the only free variables.
        assert_eq!(
            fp,
            fingerprint("SELECT\n  'a',\n  t.x$y -- c\nFROM\n  \"b\"\nWHERE\n  x = $1")
        );
    }
}
