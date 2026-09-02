//! Best-effort extraction of the column names exposed by a SELECT
//! statement. Used by:
//!
//! - CTE column inference — `WITH foo AS (SELECT a, b FROM users)`
//!   exposes `(a, b)` to anything that later `FROM foo`s it.
//! - Subquery column inference — `FROM (SELECT a, b AS x FROM users) sub`
//!   exposes `(a, x)` to `sub.|` completion.
//!
//! Tolerant of partial input — when an expression doesn't have an
//! obvious name (`SELECT 1 + 1`, `SELECT COUNT(*)` without AS), the
//! item is skipped rather than producing a wrong guess.
//!
//! NOT a SELECT-list type-checker. We don't expand `SELECT *` against
//! the schema cache (the caller is welcome to layer that on); we don't
//! infer Postgres' default-column-name rules (`SELECT a + b` would
//! actually name the column `?column?` in Postgres). The goal is "if
//! a human looking at this SELECT could tell what the output column
//! is named, so can we" — nothing more.

use crate::query::from_parse::{parse_from_tables, tokenize, Tok};
use crate::query::schema::SchemaCache;

/// One item in a SELECT list. The structured form lets callers expand
/// `*` against the FROM clause + schema cache; `extract_select_columns`
/// is a convenience that drops the unnamed shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectItem {
    /// `col`, `tab.col`, `expr AS alias`, etc. — anything that yields
    /// a single output name.
    Named(String),
    /// Bare `*` — expand against all FROM tables.
    Star,
    /// `qualifier.*` (e.g. `u.*`) — expand against the matching
    /// in-scope table.
    StarOf(String),
}

/// Walk the FIRST top-level SELECT in `sql` and return each item's
/// output column name, in order. Items whose name can't be determined
/// are skipped.
///
/// Examples (input → output):
/// - `SELECT a, b FROM t`               → `["a", "b"]`
/// - `SELECT t.a, t.b FROM t`           → `["a", "b"]`
/// - `SELECT a AS x, b AS y FROM t`     → `["x", "y"]`
/// - `SELECT id, COUNT(*) AS n FROM t`  → `["id", "n"]`
/// - `SELECT 1 + 1 FROM t`              → `[]` (no name)
/// - `SELECT *`                         → `[]` (caller can expand)
///   Convenience: just the named items. `*` and `tab.*` shapes are
///   dropped. Use [`extract_select_items`] + [`resolve_select_columns`]
///   when you want `*` expansion against a schema cache.
pub fn extract_select_columns(sql: &str) -> Vec<String> {
    extract_select_items(sql)
        .into_iter()
        .filter_map(|item| match item {
            SelectItem::Named(n) => Some(n),
            SelectItem::Star | SelectItem::StarOf(_) => None,
        })
        .collect()
}

/// Structured variant — keeps `*` / `tab.*` so the caller can decide
/// how to expand them. Stops at the next clause keyword (`FROM`,
/// `WHERE`, …) so a UNION's later arm doesn't leak into the result.
pub fn extract_select_items(sql: &str) -> Vec<SelectItem> {
    let tokens = tokenize(sql);
    let Some(start) = first_select(&tokens) else {
        return Vec::new();
    };
    walk_select_list(&tokens, start)
}

/// Resolve a SELECT's output columns against a schema cache.
/// `Named` items pass through; `Star` expands against every table in
/// the SELECT's own FROM clause; `StarOf(qual)` expands against the
/// in-scope table matching `qual`. For subquery aliases / CTE refs in
/// the FROM clause, `virtual_columns` win over the catalog.
pub fn resolve_select_columns(sql: &str, schema: &SchemaCache) -> Vec<String> {
    let items = extract_select_items(sql);
    if items.is_empty() {
        return Vec::new();
    }
    let from = parse_from_tables(sql);
    let mut out: Vec<String> = Vec::new();
    for item in items {
        match item {
            SelectItem::Named(n) => out.push(n),
            SelectItem::Star => {
                for t in &from {
                    if let Some(v) = &t.virtual_columns {
                        out.extend(v.iter().cloned());
                    } else if let Some(cols) = schema.columns_for(t.schema.as_deref(), &t.name) {
                        out.extend(cols.iter().cloned());
                    }
                }
            }
            SelectItem::StarOf(qual) => {
                let q_lower = qual.to_ascii_lowercase();
                for t in &from {
                    if t.match_key() == q_lower {
                        if let Some(v) = &t.virtual_columns {
                            out.extend(v.iter().cloned());
                        } else if let Some(cols) = schema.columns_for(t.schema.as_deref(), &t.name)
                        {
                            out.extend(cols.iter().cloned());
                        }
                        break;
                    }
                }
            }
        }
    }
    out
}

fn first_select(tokens: &[Tok<'_>]) -> Option<usize> {
    for (i, t) in tokens.iter().enumerate() {
        if t.text.eq_ignore_ascii_case("SELECT") {
            return Some(i + 1);
        }
    }
    None
}

/// Walk the comma-separated SELECT list starting at `start`, stopping
/// at the next clause keyword (FROM / WHERE / ORDER / etc).
fn walk_select_list(tokens: &[Tok<'_>], start: usize) -> Vec<SelectItem> {
    let stop_words: &[&str] = &[
        "FROM",
        "WHERE",
        "GROUP",
        "ORDER",
        "HAVING",
        "LIMIT",
        "OFFSET",
        "FETCH",
        "RETURNING",
        "UNION",
        "INTERSECT",
        "EXCEPT",
        "INTO",
    ];
    let mut out: Vec<SelectItem> = Vec::new();
    let mut chunk: Vec<&Tok> = Vec::new();
    let mut paren_depth = 0i32;
    let mut i = start;
    while i < tokens.len() {
        let tok = &tokens[i];
        // DISTINCT / ALL / DISTINCT ON (...) modifiers come right after
        // SELECT — skip them so the first real item isn't mis-read as
        // an alias.
        if chunk.is_empty()
            && paren_depth == 0
            && (tok.text.eq_ignore_ascii_case("DISTINCT") || tok.text.eq_ignore_ascii_case("ALL"))
        {
            i += 1;
            // `DISTINCT ON ( … )` — skip the paren group too.
            if i < tokens.len() && tokens[i].text.eq_ignore_ascii_case("ON") {
                i += 1;
                if i < tokens.len() && tokens[i].text == "(" {
                    let mut d = 1;
                    i += 1;
                    while i < tokens.len() && d > 0 {
                        match tokens[i].text {
                            "(" => d += 1,
                            ")" => d -= 1,
                            _ => {}
                        }
                        i += 1;
                    }
                }
            }
            continue;
        }
        if paren_depth == 0 {
            let upper = tok.text.to_ascii_uppercase();
            if stop_words.contains(&upper.as_str()) {
                break;
            }
            if tok.text == "," {
                if let Some(item) = chunk_to_item(&chunk) {
                    out.push(item);
                }
                chunk.clear();
                i += 1;
                continue;
            }
            if tok.text == ";" {
                break;
            }
        }
        if tok.text == "(" {
            paren_depth += 1;
        } else if tok.text == ")" {
            paren_depth -= 1;
        }
        chunk.push(tok);
        i += 1;
    }
    if let Some(item) = chunk_to_item(&chunk) {
        out.push(item);
    }
    out
}

/// Distil one comma-separated SELECT-list chunk down to its output
/// column name, if discernible. Recognised shapes:
///
/// - `name`                                       → name
/// - `tab.name`                                   → name
/// - `schema.tab.name`                            → name
/// - `expr AS alias`                              → alias  (anywhere)
/// - `*`, `tab.*`, anonymous expressions          → None
fn chunk_to_item(chunk: &[&Tok<'_>]) -> Option<SelectItem> {
    if chunk.is_empty() {
        return None;
    }
    // Bare `*` — Star.
    if chunk.len() == 1 && chunk[0].text == "*" {
        return Some(SelectItem::Star);
    }
    // `qualifier.*` — StarOf(qualifier).
    if chunk.len() == 3
        && chunk[1].text == "."
        && chunk[2].text == "*"
        && is_identifier_like(chunk[0].text)
    {
        return Some(SelectItem::StarOf(chunk[0].text.to_string()));
    }
    // Strict `AS alias` first — it's the most authoritative signal,
    // and a SELECT-list `AS` is always at the top level (paren-depth 0
    // from the chunk's perspective). Scan tokens at the top level only.
    let mut depth = 0i32;
    for (idx, tok) in chunk.iter().enumerate() {
        match tok.text {
            "(" => depth += 1,
            ")" => depth -= 1,
            _ => {}
        }
        if depth == 0 && tok.text.eq_ignore_ascii_case("AS") {
            if let Some(next) = chunk.get(idx + 1) {
                if is_identifier_like(next.text) {
                    return Some(SelectItem::Named(next.text.to_string()));
                }
            }
        }
    }
    // Plain ident.
    if chunk.len() == 1 && is_identifier_like(chunk[0].text) {
        return Some(SelectItem::Named(chunk[0].text.to_string()));
    }
    // tab.col / schema.tab.col — last segment is the output name.
    if chunk.len() == 3
        && chunk[1].text == "."
        && is_identifier_like(chunk[0].text)
        && is_identifier_like(chunk[2].text)
    {
        return Some(SelectItem::Named(chunk[2].text.to_string()));
    }
    if chunk.len() == 5
        && chunk[1].text == "."
        && chunk[3].text == "."
        && is_identifier_like(chunk[0].text)
        && is_identifier_like(chunk[2].text)
        && is_identifier_like(chunk[4].text)
    {
        return Some(SelectItem::Named(chunk[4].text.to_string()));
    }
    None
}

fn is_identifier_like(s: &str) -> bool {
    let mut chars = s.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_idents() {
        assert_eq!(extract_select_columns("SELECT a, b FROM t"), vec!["a", "b"]);
    }

    #[test]
    fn qualified_idents_use_last_segment() {
        assert_eq!(
            extract_select_columns("SELECT t.a, t.b FROM t"),
            vec!["a", "b"]
        );
        assert_eq!(
            extract_select_columns("SELECT s.t.a, s.t.b FROM s.t"),
            vec!["a", "b"]
        );
    }

    #[test]
    fn as_alias_wins_over_underlying_expression() {
        assert_eq!(
            extract_select_columns("SELECT id, COUNT(*) AS n, email AS e FROM users"),
            vec!["id", "n", "e"]
        );
    }

    #[test]
    fn star_yields_no_names() {
        assert_eq!(
            extract_select_columns("SELECT * FROM users"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn unnameable_expressions_are_skipped() {
        // `1 + 1` has no derivable name; the `b` survives.
        assert_eq!(extract_select_columns("SELECT 1 + 1, b FROM t"), vec!["b"]);
    }

    #[test]
    fn stops_at_clause_keywords() {
        // Make sure WHERE / ORDER / etc don't accidentally get pulled
        // into the SELECT list.
        assert_eq!(
            extract_select_columns("SELECT a, b FROM users WHERE id = 1 ORDER BY a LIMIT 10"),
            vec!["a", "b"]
        );
    }

    #[test]
    fn distinct_modifier_is_skipped() {
        assert_eq!(
            extract_select_columns("SELECT DISTINCT a, b FROM t"),
            vec!["a", "b"]
        );
        assert_eq!(
            extract_select_columns("SELECT ALL a, b FROM t"),
            vec!["a", "b"]
        );
    }

    #[test]
    fn distinct_on_paren_group_is_skipped() {
        assert_eq!(
            extract_select_columns("SELECT DISTINCT ON (a, b) c, d FROM t"),
            vec!["c", "d"]
        );
    }

    #[test]
    fn partial_select_list_with_no_from_still_extracts() {
        // Operator mid-typing — no FROM yet.
        assert_eq!(
            extract_select_columns("SELECT id, email"),
            vec!["id", "email"]
        );
    }

    #[test]
    fn handles_function_calls_with_commas() {
        // The `,` inside COALESCE is at paren-depth 1 and must NOT
        // split items.
        assert_eq!(
            extract_select_columns("SELECT id, COALESCE(name, email, 'anon') AS who FROM users"),
            vec!["id", "who"]
        );
    }

    #[test]
    fn no_select_keyword_returns_empty() {
        assert!(extract_select_columns("FROM t").is_empty());
        assert!(extract_select_columns("").is_empty());
    }

    #[test]
    fn extract_items_recognises_star() {
        let items = extract_select_items("SELECT * FROM users");
        assert_eq!(items, vec![SelectItem::Star]);
    }

    #[test]
    fn extract_items_recognises_qualified_star() {
        let items = extract_select_items("SELECT u.*, o.id FROM users u, orders o");
        assert_eq!(
            items,
            vec![
                SelectItem::StarOf("u".into()),
                SelectItem::Named("id".into())
            ]
        );
    }

    #[test]
    fn extract_items_mixes_named_and_star() {
        let items = extract_select_items("SELECT id, *, name AS n FROM users");
        assert_eq!(
            items,
            vec![
                SelectItem::Named("id".into()),
                SelectItem::Star,
                SelectItem::Named("n".into()),
            ]
        );
    }

    #[test]
    fn resolve_select_columns_expands_bare_star_against_catalog() {
        let mut cache = SchemaCache::default();
        cache.tables.push(crate::query::schema::TableMeta {
            schema: "public".into(),
            name: "users".into(),
        });
        cache.columns_by_table.insert(
            ("public".into(), "users".into()),
            vec!["id".into(), "email".into()],
        );
        let cols = resolve_select_columns("SELECT * FROM users", &cache);
        assert_eq!(cols, vec!["id", "email"]);
    }

    #[test]
    fn resolve_select_columns_expands_qualified_star() {
        let mut cache = SchemaCache::default();
        cache.tables.push(crate::query::schema::TableMeta {
            schema: "public".into(),
            name: "users".into(),
        });
        cache.tables.push(crate::query::schema::TableMeta {
            schema: "public".into(),
            name: "orders".into(),
        });
        cache.columns_by_table.insert(
            ("public".into(), "users".into()),
            vec!["id".into(), "email".into()],
        );
        cache.columns_by_table.insert(
            ("public".into(), "orders".into()),
            vec!["order_id".into(), "total".into()],
        );
        let cols = resolve_select_columns("SELECT u.*, o.total FROM users u, orders o", &cache);
        assert_eq!(cols, vec!["id", "email", "total"]);
    }

    #[test]
    fn union_first_arm_wins_for_column_inference() {
        // The extractor stops at UNION, so only the first arm's
        // columns surface — matches SQL semantics (union result
        // columns come from the first arm).
        assert_eq!(
            extract_select_columns("SELECT a, b FROM x UNION SELECT c, d FROM y"),
            vec!["a", "b"]
        );
    }
}
