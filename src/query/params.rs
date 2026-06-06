//! Named-placeholder (`:name`) handling for saved-query
//! load-time prompts.
//!
//! A saved query like `SELECT * FROM users WHERE id = :id` should
//! prompt the operator for `id` when loaded. This module finds the
//! placeholders and substitutes the entered values back in.
//!
//! The scanner is SQL-aware enough to avoid the obvious false
//! positives:
//! - **`::` casts** (`id::text`) are not placeholders.
//! - **`:=`** is not a placeholder (the `=` isn't an identifier).
//! - Placeholders inside **single-quoted strings** (`':id'`),
//!   **double-quoted identifiers** (`"a:b"`), **line comments**
//!   (`-- :id`), and **block comments** (`/* :id */`) are ignored.
//!
//! A placeholder is `:` immediately followed by an identifier
//! start (`A-Za-z_`) then identifier continuation (`A-Za-z0-9_`).
//! Substitution is **verbatim** — the operator types the literal
//! SQL text for each value (e.g. `42` or `'alice'`) and owns its
//! quoting, exactly as if they'd typed it into the editor (and it
//! still routes through `safety.rs` on run).
//!
//! Known limitation: dollar-quoted strings (`$$ … $$`) and `E'…'`
//! backslash escapes aren't tracked. A `:name` inside one of those
//! is rare; the worst case is an extra prompt, which the operator
//! can satisfy with the original text.

use std::collections::HashMap;

/// One detected placeholder occurrence: its byte span in the
/// source (`[start, end)`, covering the leading `:`) and the
/// bare name (without the colon).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Occurrence {
    start: usize,
    end: usize,
    name: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scan {
    Normal,
    Single,
    Double,
    LineComment,
    BlockComment,
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Walk `sql` once, returning every placeholder occurrence in
/// source order (duplicates included). The single source of truth
/// for both [`extract_params`] and [`substitute_params`] so the
/// two can never disagree about what counts as a placeholder.
fn scan(sql: &str) -> Vec<Occurrence> {
    let bytes_len = sql.len();
    let chars: Vec<(usize, char)> = sql.char_indices().collect();
    let mut out = Vec::new();
    let mut state = Scan::Normal;
    let mut i = 0;
    while i < chars.len() {
        let (pos, c) = chars[i];
        let next = chars.get(i + 1).map(|&(_, c)| c);
        match state {
            Scan::Normal => match c {
                '\'' => {
                    state = Scan::Single;
                    i += 1;
                }
                '"' => {
                    state = Scan::Double;
                    i += 1;
                }
                '-' if next == Some('-') => {
                    state = Scan::LineComment;
                    i += 2;
                }
                '/' if next == Some('*') => {
                    state = Scan::BlockComment;
                    i += 2;
                }
                ':' if next == Some(':') => {
                    // Cast operator — consume both colons so the
                    // char after `::` isn't misread as a start.
                    i += 2;
                }
                ':' if next.is_some_and(is_ident_start) => {
                    // Read the identifier following the colon.
                    let mut j = i + 1;
                    while j < chars.len() && is_ident_continue(chars[j].1) {
                        j += 1;
                    }
                    let name_start = chars[i + 1].0;
                    let end = chars.get(j).map(|&(p, _)| p).unwrap_or(bytes_len);
                    out.push(Occurrence {
                        start: pos,
                        end,
                        name: sql[name_start..end].to_string(),
                    });
                    i = j;
                }
                _ => i += 1,
            },
            Scan::Single => {
                if c == '\'' {
                    if next == Some('\'') {
                        // Escaped quote (`''`) stays in the string.
                        i += 2;
                    } else {
                        state = Scan::Normal;
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
            Scan::Double => {
                if c == '"' {
                    if next == Some('"') {
                        i += 2;
                    } else {
                        state = Scan::Normal;
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
            Scan::LineComment => {
                if c == '\n' {
                    state = Scan::Normal;
                }
                i += 1;
            }
            Scan::BlockComment => {
                if c == '*' && next == Some('/') {
                    state = Scan::Normal;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        }
    }
    out
}

/// Distinct placeholder names in first-appearance order. Empty
/// when the SQL has no named placeholders (the caller then loads
/// the query directly without prompting).
pub fn extract_params(sql: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut order = Vec::new();
    for occ in scan(sql) {
        if seen.insert(occ.name.clone()) {
            order.push(occ.name);
        }
    }
    order
}

/// Replace each `:name` placeholder with its mapped value. A name
/// missing from `values` is left untouched (so a partial map
/// degrades gracefully rather than blanking the placeholder).
/// Substitution is verbatim — see the module docs.
pub fn substitute_params(sql: &str, values: &HashMap<String, String>) -> String {
    let occ = scan(sql);
    if occ.is_empty() {
        return sql.to_string();
    }
    let mut out = String::with_capacity(sql.len());
    let mut cursor = 0;
    for o in occ {
        // Copy the gap before this placeholder verbatim.
        out.push_str(&sql[cursor..o.start]);
        match values.get(&o.name) {
            Some(v) => out.push_str(v),
            None => out.push_str(&sql[o.start..o.end]),
        }
        cursor = o.end;
    }
    out.push_str(&sql[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn extract_finds_named_placeholders_in_order() {
        let sql = "SELECT * FROM t WHERE id = :id AND org = :org";
        assert_eq!(extract_params(sql), vec!["id", "org"]);
    }

    #[test]
    fn extract_dedups_repeats_keeping_first_order() {
        let sql = "SELECT :a, :b WHERE x = :a";
        assert_eq!(extract_params(sql), vec!["a", "b"]);
    }

    #[test]
    fn extract_ignores_cast_operator() {
        let sql = "SELECT id::text, created::date FROM t";
        assert!(extract_params(sql).is_empty());
    }

    #[test]
    fn extract_cast_then_real_param() {
        let sql = "SELECT id::text FROM t WHERE id = :id";
        assert_eq!(extract_params(sql), vec!["id"]);
    }

    #[test]
    fn extract_ignores_placeholder_in_single_quoted_string() {
        let sql = "SELECT ':id' AS lit FROM t WHERE x = :real";
        assert_eq!(extract_params(sql), vec!["real"]);
    }

    #[test]
    fn extract_ignores_placeholder_in_double_quoted_ident() {
        let sql = r#"SELECT "weird:col" FROM t WHERE id = :id"#;
        assert_eq!(extract_params(sql), vec!["id"]);
    }

    #[test]
    fn extract_ignores_line_comment() {
        let sql = "SELECT 1 -- :nope\nWHERE id = :yes";
        assert_eq!(extract_params(sql), vec!["yes"]);
    }

    #[test]
    fn extract_ignores_block_comment() {
        let sql = "SELECT 1 /* :nope still :nope */ WHERE id = :yes";
        assert_eq!(extract_params(sql), vec!["yes"]);
    }

    #[test]
    fn extract_does_not_treat_colon_equals_as_param() {
        // `:=` (assignment-ish) — `=` isn't an identifier start.
        let sql = "x := 5";
        assert!(extract_params(sql).is_empty());
    }

    #[test]
    fn extract_requires_letter_after_colon() {
        // `:1` is not a named placeholder.
        let sql = "SELECT :1, :_ok, :2nd";
        // `:_ok` is valid; `:2nd` starts with a digit so the colon
        // isn't a placeholder (the `nd` after `2` is just text).
        assert_eq!(extract_params(sql), vec!["_ok"]);
    }

    #[test]
    fn extract_handles_escaped_quote_in_string() {
        // The `''` keeps us inside the string; `:no` stays ignored;
        // the real param is outside.
        let sql = "SELECT 'it''s :no' FROM t WHERE id = :id";
        assert_eq!(extract_params(sql), vec!["id"]);
    }

    #[test]
    fn extract_empty_for_no_params() {
        assert!(extract_params("SELECT 1").is_empty());
    }

    #[test]
    fn substitute_replaces_each_param_verbatim() {
        let sql = "SELECT * FROM t WHERE id = :id AND name = :name";
        let out = substitute_params(sql, &map(&[("id", "42"), ("name", "'alice'")]));
        assert_eq!(out, "SELECT * FROM t WHERE id = 42 AND name = 'alice'");
    }

    #[test]
    fn substitute_replaces_all_repeats() {
        let sql = "SELECT :a WHERE x = :a OR y = :a";
        let out = substitute_params(sql, &map(&[("a", "7")]));
        assert_eq!(out, "SELECT 7 WHERE x = 7 OR y = 7");
    }

    #[test]
    fn substitute_leaves_unmapped_placeholder_intact() {
        let sql = "WHERE id = :id AND org = :org";
        let out = substitute_params(sql, &map(&[("id", "1")]));
        assert_eq!(out, "WHERE id = 1 AND org = :org");
    }

    #[test]
    fn substitute_does_not_touch_casts_or_strings() {
        let sql = "SELECT id::text, ':id' FROM t WHERE id = :id";
        let out = substitute_params(sql, &map(&[("id", "9")]));
        assert_eq!(out, "SELECT id::text, ':id' FROM t WHERE id = 9");
    }

    #[test]
    fn substitute_no_params_returns_input() {
        let sql = "SELECT 1";
        assert_eq!(substitute_params(sql, &map(&[("x", "1")])), sql);
    }

    #[test]
    fn substitute_preserves_unicode_around_param() {
        // Non-ASCII text around the placeholder must survive the
        // byte-span splicing intact.
        let sql = "SELECT 'café' WHERE id = :id";
        let out = substitute_params(sql, &map(&[("id", "1")]));
        assert_eq!(out, "SELECT 'café' WHERE id = 1");
    }
}
