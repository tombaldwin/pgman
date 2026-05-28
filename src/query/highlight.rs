//! Pure SQL syntax highlighter for the editor.
//!
//! Two passes:
//!
//! 1. [`tokenize`] walks the buffer once and emits a `Vec<Span>` where
//!    every span has a [`TokenClass`]. Lexically self-contained — no
//!    schema lookups; identifies strings, comments, numbers, keywords,
//!    function-call-shaped identifiers (`COUNT(`), generic
//!    identifiers, and operators / punctuation. ASCII-fast for the
//!    common path; UTF-8-safe via `char_indices` on the edges.
//!
//! 2. [`classify`] takes those spans, walks the [`Identifier`] ones,
//!    and re-classes them as [`TokenClass::KnownIdent`] or
//!    [`TokenClass::UnknownIdent`] depending on whether the slice
//!    text resolves against the schema cache + in-scope tables /
//!    aliases / CTEs. The fallback is "Known if it appears anywhere
//!    in the cache" — loose resolution, matching the UX call made
//!    earlier (red = typo flag; green/default = it exists somewhere).
//!
//! Spans are byte-indexed against the input buffer so the renderer
//! can slice straight into `editor_buffer` to get the display text.

use crate::query::clause::CteDef;
use crate::query::from_parse::TableRefInQuery;
use crate::query::schema::SchemaCache;
use crate::query::vocabulary::{
    AGGREGATE_FUNCTIONS, JOIN_VARIANTS, PREDICATE_OPERATORS, SCALAR_FUNCTIONS, STATEMENT_KEYWORDS,
    TYPE_NAMES, WINDOW_FUNCTIONS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenClass {
    /// SQL reserved word — `SELECT`, `FROM`, `WHERE`, `AS`, etc.
    Keyword,
    /// Identifier directly followed by `(` AND matching a known
    /// function vocab entry — `COUNT(`, `SUM(`, `NOW(`.
    Function,
    /// Single-quoted string literal `'foo'`, or dollar-quoted
    /// `$tag$ … $tag$` (Postgres-specific).
    String,
    /// `--` line comment OR `/* … */` block comment.
    Comment,
    /// Numeric literal — `1`, `1.5`, `.5`, `1e9`.
    Number,
    /// Identifier-shaped run not matched as anything else.
    /// [`classify`] re-classes these to KnownIdent / UnknownIdent.
    Identifier,
    /// Resolved against the schema cache or in-scope.
    KnownIdent,
    /// Identifier-shaped but doesn't match anything in scope or the
    /// cache — flagged in red as a likely typo.
    UnknownIdent,
    /// Symbolic operators (`=`, `<>`, `+`, `||`, `::`, …) and
    /// punctuation (`,`, `;`, `(`, `.`). Rendered in the default
    /// text colour; carried in the span list mostly so the renderer
    /// can re-stitch the buffer with no gaps.
    Operator,
    /// Stretches of whitespace. Kept so the span list covers the
    /// whole buffer; rendered as plain text.
    Whitespace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub class: TokenClass,
}

impl Span {
    fn new(start: usize, end: usize, class: TokenClass) -> Self {
        Self { start, end, class }
    }
}

/// Pure lex pass. Produces non-overlapping spans covering every byte
/// of `buf` in order, so the renderer can iterate them and slice
/// straight from the buffer without gaps.
pub fn tokenize(buf: &str) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let bytes = buf.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];

        // Whitespace run
        if b.is_ascii_whitespace() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            out.push(Span::new(start, i, TokenClass::Whitespace));
            continue;
        }

        // Line comment: `-- … \n` (or EOF)
        if b == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            let start = i;
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            out.push(Span::new(start, i, TokenClass::Comment));
            continue;
        }

        // Block comment: `/* … */`. Nested per SQL standard — track depth.
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            let start = i;
            i += 2;
            let mut depth = 1;
            while i < bytes.len() && depth > 0 {
                if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            // Unterminated comment: still emit a Comment span to EOF
            // so the renderer doesn't fall back to default styling
            // mid-typing.
            out.push(Span::new(start, i, TokenClass::Comment));
            continue;
        }

        // Single-quoted string. `''` inside is a literal quote.
        if b == b'\'' {
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\'' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push(Span::new(start, i, TokenClass::String));
            continue;
        }

        // Dollar-quoted string: `$tag$ … $tag$` (tag optional). The
        // tag is `[A-Za-z_][A-Za-z0-9_]*` per Postgres docs.
        if b == b'$' {
            if let Some(end_tag) = scan_dollar_tag(bytes, i) {
                let tag = &bytes[i..=end_tag]; // includes both `$`
                let body_start = end_tag + 1;
                if let Some(close_at) = find_subslice(bytes, body_start, tag) {
                    let span_end = close_at + tag.len();
                    out.push(Span::new(i, span_end, TokenClass::String));
                    i = span_end;
                    continue;
                }
                // Unterminated — span the rest of the buffer so the
                // user sees the in-progress string highlighted.
                out.push(Span::new(i, bytes.len(), TokenClass::String));
                i = bytes.len();
                continue;
            }
            // Lone `$` — treat as operator (Postgres params like `$1`
            // are handled below via the digit-prefix check).
        }

        // Number: digit (or `.` followed by digit). Consume digits,
        // optional `.<digits>`, optional `e[+-]?<digits>`.
        if b.is_ascii_digit() || (b == b'.' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit())
        {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'.' {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
                let after_e = i + 1;
                let mut j = after_e;
                if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
                    j += 1;
                }
                if j < bytes.len() && bytes[j].is_ascii_digit() {
                    while j < bytes.len() && bytes[j].is_ascii_digit() {
                        j += 1;
                    }
                    i = j;
                }
            }
            out.push(Span::new(start, i, TokenClass::Number));
            continue;
        }

        // Identifier: ASCII letter or `_` start, alnum + `_` body.
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let text = &buf[start..i];
            let class = classify_word(text, bytes, i);
            out.push(Span::new(start, i, class));
            continue;
        }

        // Everything else: operator / punctuation. Coalesce runs of
        // non-identifier / non-whitespace / non-quote-or-comment
        // chars so we don't fragment.
        let start = i;
        while i < bytes.len() && is_operator_char(bytes[i]) {
            // Stop early if the next char would start a comment / string.
            if (bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-')
                || (bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*')
            {
                break;
            }
            i += 1;
        }
        if i == start {
            // Non-ASCII or otherwise unrecognised byte — step by one
            // codepoint to stay char-boundary safe.
            let step = utf8_char_len(bytes[start]);
            i = (start + step).min(bytes.len());
        }
        out.push(Span::new(start, i, TokenClass::Operator));
    }
    out
}

/// Re-class generic `Identifier` spans as `KnownIdent` or
/// `UnknownIdent` based on schema / in-scope resolution. Non-Identifier
/// spans pass through unchanged.
pub fn classify(
    spans: Vec<Span>,
    buf: &str,
    schema: &SchemaCache,
    in_scope: &[TableRefInQuery],
    ctes: &[CteDef],
) -> Vec<Span> {
    spans
        .into_iter()
        .map(|s| {
            if s.class != TokenClass::Identifier {
                return s;
            }
            let text = &buf[s.start..s.end];
            let class = if identifier_known(text, schema, in_scope, ctes) {
                TokenClass::KnownIdent
            } else {
                TokenClass::UnknownIdent
            };
            Span { class, ..s }
        })
        .collect()
}

fn identifier_known(
    name: &str,
    schema: &SchemaCache,
    in_scope: &[TableRefInQuery],
    ctes: &[CteDef],
) -> bool {
    // Tables (by name across any schema).
    if schema
        .tables
        .iter()
        .any(|t| t.name.eq_ignore_ascii_case(name))
    {
        return true;
    }
    // Sequences / indexes — same.
    if schema
        .sequences
        .iter()
        .any(|t| t.name.eq_ignore_ascii_case(name))
    {
        return true;
    }
    if schema
        .indexes
        .iter()
        .any(|t| t.name.eq_ignore_ascii_case(name))
    {
        return true;
    }
    // Schemas.
    if schema.schemas.iter().any(|s| s.eq_ignore_ascii_case(name)) {
        return true;
    }
    // Any column anywhere in the cache.
    if schema
        .columns_by_table
        .values()
        .any(|cols| cols.iter().any(|c| c.eq_ignore_ascii_case(name)))
    {
        return true;
    }
    // In-scope aliases (FROM `users u`).
    if in_scope.iter().any(|t| {
        t.alias
            .as_deref()
            .map(|a| a.eq_ignore_ascii_case(name))
            .unwrap_or(false)
    }) {
        return true;
    }
    // CTE names.
    if ctes.iter().any(|c| c.name.eq_ignore_ascii_case(name)) {
        return true;
    }
    // Virtual columns from CTE / subquery bodies.
    if in_scope.iter().any(|t| {
        t.virtual_columns
            .as_ref()
            .map(|cols| cols.iter().any(|c| c.eq_ignore_ascii_case(name)))
            .unwrap_or(false)
    }) {
        return true;
    }
    // `EXCLUDED` is a Postgres built-in pseudo-table inside
    // `ON CONFLICT DO UPDATE` clauses. The classifier registers it as
    // an in-scope alias when applicable; this is a belt-and-braces
    // catch for cases where the classifier hasn't (yet) flagged it.
    if name.eq_ignore_ascii_case("EXCLUDED") {
        return true;
    }
    false
}

/// Decide whether an alphanumeric word is a keyword, a known
/// function (when followed by `(`), or a generic identifier. The
/// `following_byte_idx` is where the word ends — we peek past
/// trailing whitespace to spot `<name>(` shapes.
fn classify_word(text: &str, bytes: &[u8], following_byte_idx: usize) -> TokenClass {
    let upper = text.to_ascii_uppercase();
    // Keyword? Match against the union of all keyword-shaped vocabs.
    if STATEMENT_KEYWORDS
        .iter()
        .any(|k| k.eq_ignore_ascii_case(&upper))
        || PREDICATE_OPERATORS
            .iter()
            .any(|k| k.eq_ignore_ascii_case(&upper))
        || JOIN_VARIANTS.iter().any(|k| k.eq_ignore_ascii_case(&upper))
        || TYPE_NAMES.iter().any(|k| k.eq_ignore_ascii_case(&upper))
        || matches_extra_keyword(&upper)
    {
        return TokenClass::Keyword;
    }
    // Function call? Look for `(` after optional whitespace.
    if function_call_shape(bytes, following_byte_idx) {
        if AGGREGATE_FUNCTIONS
            .iter()
            .any(|f| f.eq_ignore_ascii_case(&upper))
            || SCALAR_FUNCTIONS
                .iter()
                .any(|f| f.eq_ignore_ascii_case(&upper))
            || WINDOW_FUNCTIONS
                .iter()
                .any(|f| f.eq_ignore_ascii_case(&upper))
        {
            return TokenClass::Function;
        }
    }
    TokenClass::Identifier
}

/// Keywords that aren't in any of the keyword-shaped vocab lists but
/// are essential SQL grammar. Kept here so the highlighter doesn't
/// have to peek into clause-classifier internals.
fn matches_extra_keyword(upper: &str) -> bool {
    matches!(
        upper,
        "FROM"
            | "WHERE"
            | "GROUP"
            | "BY"
            | "ORDER"
            | "HAVING"
            | "LIMIT"
            | "OFFSET"
            | "DISTINCT"
            | "AS"
            | "ON"
            | "USING"
            | "AND"
            | "OR"
            | "NOT"
            | "IN"
            | "IS"
            | "NULL"
            | "TRUE"
            | "FALSE"
            | "CASE"
            | "WHEN"
            | "THEN"
            | "ELSE"
            | "END"
            | "INTO"
            | "VALUES"
            | "RETURNING"
            | "CONFLICT"
            | "DO"
            | "NOTHING"
            | "EXCLUDED"
            | "WITH"
            | "RECURSIVE"
            | "ASC"
            | "DESC"
            | "NULLS"
            | "FIRST"
            | "LAST"
            | "UNION"
            | "INTERSECT"
            | "EXCEPT"
            | "ALL"
            | "ANY"
            | "EXISTS"
            | "CAST"
            | "AT"
            | "TIME"
            | "ZONE"
            | "BETWEEN"
            | "LIKE"
            | "ILIKE"
            | "SIMILAR"
            | "TO"
            | "ESCAPE"
            | "OVER"
            | "PARTITION"
            | "BEGIN"
            | "COMMIT"
            | "ROLLBACK"
            | "SAVEPOINT"
            | "RELEASE"
            | "TRANSACTION"
            | "ISOLATION"
            | "LEVEL"
            | "READ"
            | "WRITE"
            | "ONLY"
            | "SHOW"
            | "SET"
            | "RESET"
            | "EXPLAIN"
            | "ANALYZE"
            | "VACUUM"
            | "REINDEX"
            | "CLUSTER"
            | "INDEX"
            | "TABLE"
            | "VIEW"
            | "MATERIALIZED"
            | "SEQUENCE"
            | "CREATE"
            | "ALTER"
            | "DROP"
            | "TRUNCATE"
            | "COMMENT"
            | "GRANT"
            | "REVOKE"
            | "PRIMARY"
            | "FOREIGN"
            | "KEY"
            | "REFERENCES"
            | "CHECK"
            | "UNIQUE"
            | "CONSTRAINT"
            | "DEFAULT"
            | "IF"
            | "CASCADE"
            | "RESTRICT"
            | "FOR"
            | "UPDATE"
            | "OF"
            | "FETCH"
            | "ROWS"
            | "ROW"
            | "WINDOW"
            | "FILTER"
            | "WITHIN"
    )
}

/// `<name>(` shape: skip whitespace from `idx` and check if the next
/// non-space byte is `(`.
fn function_call_shape(bytes: &[u8], idx: usize) -> bool {
    let mut j = idx;
    while j < bytes.len() && bytes[j] == b' ' {
        j += 1;
    }
    j < bytes.len() && bytes[j] == b'('
}

/// Scan a dollar-quote opening tag at `start` (assumes
/// `bytes[start] == b'$'`). Returns the index of the closing `$` of
/// the OPEN tag (so the body starts at `that + 1`). Tag chars are
/// `[A-Za-z_][A-Za-z0-9_]*`; empty-tag `$$` is also valid.
fn scan_dollar_tag(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    if i < bytes.len() && bytes[i] == b'$' {
        return Some(i);
    }
    if i >= bytes.len() || !(bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
        return None;
    }
    i += 1;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'$' {
        Some(i)
    } else {
        None
    }
}

/// Find a byte subslice starting at `from`. Returns the start index
/// of the match if any. Used for matching dollar-quote close tags.
fn find_subslice(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from + needle.len() > haystack.len() {
        return None;
    }
    let last = haystack.len() - needle.len();
    let mut i = from;
    while i <= last {
        if &haystack[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn is_operator_char(b: u8) -> bool {
    matches!(
        b,
        b'+' | b'-'
            | b'*'
            | b'/'
            | b'%'
            | b'='
            | b'<'
            | b'>'
            | b'!'
            | b'~'
            | b'|'
            | b'&'
            | b'^'
            | b'?'
            | b'@'
            | b':'
            | b';'
            | b','
            | b'('
            | b')'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'.'
            | b'#'
            | b'$'
    )
}

/// UTF-8 byte-length lookup: how many bytes the codepoint starting at
/// `b` occupies. Bytes that are continuation bytes return 1 so we
/// step over them defensively without panicking.
fn utf8_char_len(b: u8) -> usize {
    match b {
        b if b < 0x80 => 1,
        b if b < 0xC0 => 1,
        b if b < 0xE0 => 2,
        b if b < 0xF0 => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(buf: &str) -> Vec<(TokenClass, &str)> {
        tokenize(buf)
            .into_iter()
            .map(|s| (s.class, &buf[s.start..s.end]))
            .collect()
    }

    #[test]
    fn keyword_then_identifier() {
        let cs = classes("SELECT users");
        assert_eq!(cs[0].0, TokenClass::Keyword);
        assert_eq!(cs[0].1, "SELECT");
        // Whitespace span between.
        assert_eq!(cs[1].0, TokenClass::Whitespace);
        assert_eq!(cs[2].0, TokenClass::Identifier);
        assert_eq!(cs[2].1, "users");
    }

    #[test]
    fn function_name_followed_by_open_paren() {
        let cs = classes("COUNT(*)");
        assert_eq!(cs[0].0, TokenClass::Function);
        assert_eq!(cs[0].1, "COUNT");
        // `(*)` coalesces into one Operator span — rendering all
        // three glyphs with the same style is fine, and avoiding
        // micro-fragmentation keeps the renderer's span list short.
        assert_eq!(cs[1].0, TokenClass::Operator);
        assert_eq!(cs[1].1, "(*)");
    }

    #[test]
    fn function_name_without_paren_is_identifier() {
        let cs = classes("count AS n");
        assert_eq!(cs[0].0, TokenClass::Identifier);
        assert_eq!(cs[0].1, "count");
    }

    #[test]
    fn single_quoted_string_with_escaped_quote() {
        let cs = classes("'it''s'");
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].0, TokenClass::String);
        assert_eq!(cs[0].1, "'it''s'");
    }

    #[test]
    fn dollar_quoted_string_with_tag() {
        let cs = classes("$body$ raw 'stuff' $body$");
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].0, TokenClass::String);
    }

    #[test]
    fn unterminated_dollar_quote_spans_rest_of_buffer() {
        let cs = classes("$body$ raw");
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].0, TokenClass::String);
    }

    #[test]
    fn line_comment_to_end_of_line() {
        let cs = classes("-- todo\nSELECT 1");
        assert_eq!(cs[0].0, TokenClass::Comment);
        assert_eq!(cs[0].1, "-- todo");
        // The newline is its own whitespace span.
        assert_eq!(cs[1].0, TokenClass::Whitespace);
        assert_eq!(cs[2].0, TokenClass::Keyword);
        assert_eq!(cs[2].1, "SELECT");
    }

    #[test]
    fn block_comment_nests() {
        let cs = classes("/* a /* b */ c */ X");
        assert_eq!(cs[0].0, TokenClass::Comment);
        assert_eq!(cs[0].1, "/* a /* b */ c */");
    }

    #[test]
    fn number_with_decimal_and_exponent() {
        let cs = classes("1.5e-3");
        assert_eq!(cs[0].0, TokenClass::Number);
        assert_eq!(cs[0].1, "1.5e-3");
    }

    #[test]
    fn leading_dot_number() {
        let cs = classes(".5");
        assert_eq!(cs[0].0, TokenClass::Number);
    }

    #[test]
    fn operator_run_is_one_span() {
        // `<>` and `<=` should each be a single Operator span.
        let cs = classes("a <> b");
        assert_eq!(cs[2].0, TokenClass::Operator);
        assert_eq!(cs[2].1, "<>");
    }

    #[test]
    fn covers_every_byte_in_order() {
        // Important contract: spans don't overlap and together cover
        // the whole buffer end-to-end. The renderer relies on this
        // to slice without gaps.
        let buf = "SELECT a, b FROM t -- end";
        let spans = tokenize(buf);
        let mut next = 0;
        for s in &spans {
            assert_eq!(s.start, next, "gap before {s:?}");
            assert!(s.start <= s.end);
            next = s.end;
        }
        assert_eq!(next, buf.len());
    }

    #[test]
    fn multibyte_chars_dont_panic() {
        // The walker is byte-indexed but must not split a codepoint.
        // Step over the multi-byte `é` defensively.
        let buf = "SELECT 'café'";
        let spans = tokenize(buf);
        // First span = Keyword `SELECT`; later spans include the
        // string. No panic on the walker is the main thing here.
        assert!(!spans.is_empty());
        // Sum of span lengths = buffer length.
        let total: usize = spans.iter().map(|s| s.end - s.start).sum();
        assert_eq!(total, buf.len());
    }

    #[test]
    fn classify_promotes_known_identifier() {
        use crate::query::schema::{SchemaCache, TableMeta};
        let mut cache = SchemaCache::default();
        cache.tables.push(TableMeta {
            schema: "public".into(),
            name: "users".into(),
        });
        let buf = "SELECT users FROM t";
        let spans = tokenize(buf);
        let resolved = classify(spans, buf, &cache, &[], &[]);
        // `users` (the table) should be KnownIdent; `t` (no match)
        // should be UnknownIdent.
        let users = resolved
            .iter()
            .find(|s| &buf[s.start..s.end] == "users")
            .expect("`users` span");
        assert_eq!(users.class, TokenClass::KnownIdent);
        let t = resolved
            .iter()
            .find(|s| &buf[s.start..s.end] == "t")
            .expect("`t` span");
        assert_eq!(t.class, TokenClass::UnknownIdent);
    }

    #[test]
    fn classify_resolves_via_in_scope_alias() {
        use crate::query::schema::SchemaCache;
        let cache = SchemaCache::default();
        let in_scope = vec![TableRefInQuery {
            schema: None,
            name: "users".into(),
            alias: Some("u".into()),
            virtual_columns: None,
        }];
        let buf = "u";
        let spans = tokenize(buf);
        let resolved = classify(spans, buf, &cache, &in_scope, &[]);
        assert_eq!(resolved[0].class, TokenClass::KnownIdent);
    }

    #[test]
    fn classify_resolves_via_cte_name() {
        use crate::query::schema::SchemaCache;
        let cache = SchemaCache::default();
        let cte = CteDef {
            name: "recent".into(),
            columns: vec!["id".into()],
        };
        let buf = "recent";
        let spans = tokenize(buf);
        let resolved = classify(spans, buf, &cache, &[], &[cte]);
        assert_eq!(resolved[0].class, TokenClass::KnownIdent);
    }

    #[test]
    fn excluded_pseudo_table_always_known() {
        use crate::query::schema::SchemaCache;
        let cache = SchemaCache::default();
        let buf = "EXCLUDED";
        let spans = tokenize(buf);
        let resolved = classify(spans, buf, &cache, &[], &[]);
        // EXCLUDED is in the extra-keyword list, so it lexes as
        // Keyword and never reaches classify's Identifier branch.
        // Either way the renderer treats it as "expected".
        assert!(matches!(
            resolved[0].class,
            TokenClass::Keyword | TokenClass::KnownIdent
        ));
    }
}
