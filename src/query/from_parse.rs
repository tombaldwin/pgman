//! Best-effort extraction of `(schema?, table, alias?)` triples from the
//! `FROM` / `JOIN` clauses of a (possibly incomplete) SQL buffer.
//!
//! This is **not** a SQL parser. It scans tokens, finds `FROM` / `JOIN`
//! keywords, then for each one captures the next identifier (optionally
//! `schema.table`) and an optional alias (`AS x` or bare `x`). It stops
//! at `WHERE` / `GROUP` / `HAVING` / `ORDER` / `LIMIT` / `;` so a partly-
//! typed query like `SELECT u.| FROM users u JOIN orders` still works.
//!
//! Used by `query::complete` to scope qualified completion (`alias.col`)
//! and biased unqualified completion (prefer columns of tables that
//! appear in the current `FROM`).

/// One table reference pulled from a `FROM` / `JOIN`. `schema`, `name`
/// and `alias` are the names the server resolves: an unquoted
/// identifier folded to lower case, a quoted one verbatim (see
/// [`ident_value`]), so `FROM "Users"` and `FROM Users` differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRefInQuery {
    pub schema: Option<String>,
    pub name: String,
    pub alias: Option<String>,
    /// When this `TableRefInQuery` came from a subquery
    /// (`FROM (SELECT a, b FROM users) sub`), holds the column names
    /// inferred from the subquery's SELECT list. `None` for catalog
    /// tables (look those up in the schema cache).
    pub virtual_columns: Option<Vec<String>>,
}

impl TableRefInQuery {
    /// The name the operator is most likely to type as a qualifier —
    /// `alias` when present, otherwise `name`. Quoted/case-sensitive
    /// concerns are punted: we lowercase before matching.
    pub fn match_key(&self) -> String {
        self.alias
            .as_deref()
            .unwrap_or(self.name.as_str())
            .to_ascii_lowercase()
    }
}

/// Scan `sql` and return the list of table references in its FROM / JOIN
/// clauses. Tolerant: unterminated strings stop the scan; unknown tokens
/// are skipped; multiple FROM-clauses (subqueries) all contribute.
/// Like `parse_from_tables` but expands `SELECT *` inside subquery
/// bodies against the schema cache, so `FROM (SELECT * FROM users)
/// sub` lands with users' columns on the synthetic entry. Use this
/// from the completion engine; the pure `parse_from_tables` stays for
/// tests / callers without a cache.
pub fn parse_from_tables_resolved(
    sql: &str,
    schema: &crate::query::schema::SchemaCache,
) -> Vec<TableRefInQuery> {
    let mut out = parse_from_tables(sql);
    // Re-walk the buffer for any subquery synthetic entries whose
    // virtual_columns came back empty / star-only; re-extract using
    // resolve_select_columns. Cheapest correct approach without
    // re-architecting the parser: per-entry, find the corresponding
    // subquery body and re-run.
    let tokens = tokenize(sql);
    let mut entry_idx = 0;
    let mut i = 0;
    while i < tokens.len() && entry_idx < out.len() {
        let upper = tokens[i].text.to_ascii_uppercase();
        if upper == "FROM" || upper == "JOIN" {
            i += 1;
            loop {
                if let Some(close) = peek_subquery_close(&tokens, i) {
                    // Body tokens between i+1 and close-1.
                    let body_tokens = &tokens[i + 1..close - 1];
                    let body_text: String = body_tokens
                        .iter()
                        .map(|t| t.text)
                        .collect::<Vec<_>>()
                        .join(" ");
                    if let Some(entry) = out.get_mut(entry_idx) {
                        let cols =
                            crate::query::select_list::resolve_select_columns(&body_text, schema);
                        if !cols.is_empty() {
                            entry.virtual_columns = Some(cols);
                        }
                    }
                    entry_idx += 1;
                    // Skip past alias.
                    let alias_idx = if tokens
                        .get(close)
                        .map(|t| t.text.eq_ignore_ascii_case("AS"))
                        .unwrap_or(false)
                    {
                        close + 1
                    } else {
                        close
                    };
                    i = alias_idx + 1;
                } else {
                    // Plain table — already in `out`. Skip past it +
                    // optional alias.
                    if let Some((_, j)) = take_table_ref(&tokens, i) {
                        i = j;
                        let (_, j) = take_alias(&tokens, i);
                        i = j;
                        entry_idx += 1;
                    } else {
                        break;
                    }
                }
                if i < tokens.len() && tokens[i].text == "," {
                    i += 1;
                    continue;
                }
                break;
            }
        } else {
            i += 1;
        }
    }
    out
}

pub fn parse_from_tables(sql: &str) -> Vec<TableRefInQuery> {
    let tokens = tokenize(sql);
    let mut out: Vec<TableRefInQuery> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let tok = &tokens[i];
        let upper = tok.text.to_ascii_uppercase();
        if upper == "FROM" || upper == "JOIN" {
            i += 1;
            // Pull one table ref (or subquery), then zero-or-one alias.
            // Loop continues across `,`-separated lists after FROM.
            loop {
                // LATERAL is a marker that precedes a subquery (or a
                // function call). Skip past it so the next iteration
                // handles the following `(...)` as a normal subquery.
                // Without this skip, LATERAL would be consumed by
                // `take_table_ref` as a phantom table named "LATERAL".
                if tokens
                    .get(i)
                    .map(|t| t.text.eq_ignore_ascii_case("LATERAL"))
                    .unwrap_or(false)
                {
                    i += 1;
                    continue;
                }
                // Subquery: `( ... ) [AS] alias` — capture the alias as
                // a synthetic table entry so it shows up in_scope. We
                // don't type-check the subquery body (would need a
                // mini-engine), so columns of `sub` aren't known —
                // but typing `sub` itself completes via the alias path.
                if let Some(close) = peek_subquery_close(&tokens, i) {
                    let alias_idx = if tokens
                        .get(close)
                        .map(|t| t.text.eq_ignore_ascii_case("AS"))
                        .unwrap_or(false)
                    {
                        close + 1
                    } else {
                        close
                    };
                    let alias_text = tokens
                        .get(alias_idx)
                        .filter(|t| is_name_token(t))
                        .map(ident_value);
                    if let Some(name) = alias_text {
                        // Pull the subquery body's SELECT list so the
                        // outer query can complete `sub.col` against it.
                        let body_tokens = &tokens[i + 1..close - 1];
                        let body_text: String = body_tokens
                            .iter()
                            .map(|t| t.text)
                            .collect::<Vec<_>>()
                            .join(" ");
                        let cols = crate::query::select_list::extract_select_columns(&body_text);
                        let virtual_columns = if cols.is_empty() { None } else { Some(cols) };
                        out.push(TableRefInQuery {
                            schema: None,
                            name: name.clone(),
                            // alias = name so the synthetic entry
                            // surfaces in both the alias and the
                            // qualified-lookup paths of completion.
                            alias: Some(name),
                            virtual_columns,
                        });
                        i = alias_idx + 1;
                    } else {
                        // Anonymous subquery (rare, often a typo while
                        // typing). Skip past it; nothing to add to scope.
                        i = close;
                    }
                } else {
                    let Some((table, j)) = take_table_ref(&tokens, i) else {
                        break;
                    };
                    i = j;
                    let (alias, j) = take_alias(&tokens, i);
                    i = j;
                    out.push(TableRefInQuery {
                        schema: table.schema,
                        name: table.name,
                        alias,
                        virtual_columns: None,
                    });
                }
                // After a comma, another table ref. Anything else ends
                // the FROM list and we fall back to the outer scan.
                if i < tokens.len() && tokens[i].text == "," {
                    i += 1;
                    continue;
                }
                break;
            }
        } else {
            i += 1;
        }
    }
    out
}

/// If `tokens[i]` is `(`, return the index of the token AFTER its
/// matching `)`. Returns `None` if it's not a paren or the parens
/// don't close.
fn peek_subquery_close(tokens: &[Tok], i: usize) -> Option<usize> {
    if tokens.get(i).map(|t| t.text) != Some("(") {
        return None;
    }
    let mut depth = 1i32;
    let mut j = i + 1;
    while j < tokens.len() && depth > 0 {
        match tokens[j].text {
            "(" => depth += 1,
            ")" => depth -= 1,
            _ => {}
        }
        j += 1;
    }
    if depth == 0 {
        Some(j)
    } else {
        None
    }
}

// -- internals --

#[derive(Debug, Clone)]
pub(crate) struct Tok<'a> {
    pub(crate) text: &'a str,
    /// True for a `"…"` quoted identifier. `text` is then the raw body
    /// between the quotes, with any `""` escape still doubled — use
    /// [`ident_value`] for the identifier the server sees.
    pub(crate) quoted: bool,
}

/// The identifier a token names, as the server resolves it: an unquoted
/// identifier folds to lower case, a quoted one is kept verbatim (with
/// `""` unescaped to `"`). `FROM PgRevMixed` names `pgrevmixed`;
/// `FROM "PgRevMixed"` names `PgRevMixed` — two different tables.
pub(crate) fn ident_value(tok: &Tok) -> String {
    if tok.quoted {
        tok.text.replace("\"\"", "\"")
    } else {
        tok.text.to_ascii_lowercase()
    }
}

/// Can this token stand where a table / alias name goes? A quoted
/// identifier always can (it may hold spaces or punctuation); a bare one
/// must look like an identifier.
fn is_name_token(tok: &Tok) -> bool {
    if tok.quoted {
        !tok.text.is_empty()
    } else {
        is_identifier_like(tok.text)
    }
}

/// Lex a SQL fragment into identifier / punctuation / keyword tokens.
/// Strings, comments, and whitespace are skipped. Stops on unterminated
/// string / comment to keep the scan deterministic on partial input.
pub(crate) fn tokenize(sql: &str) -> Vec<Tok<'_>> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        // Whitespace
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // -- line comment
        if c == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // /* … */ block comment (no nesting)
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 >= bytes.len() {
                break;
            }
            i += 2;
            continue;
        }
        // '…' string literal
        if c == b'\'' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'\'' {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if i >= bytes.len() {
                break;
            }
            i += 1;
            continue;
        }
        // "…" quoted identifier — keep as one token (without the
        // quotes). A doubled `""` inside is an escaped quote, not the
        // end of the identifier.
        if c == b'"' {
            let start = i + 1;
            i += 1;
            loop {
                while i < bytes.len() && bytes[i] != b'"' {
                    i += 1;
                }
                if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                    i += 2;
                    continue;
                }
                break;
            }
            if i >= bytes.len() {
                break;
            }
            out.push(Tok {
                text: &sql[start..i],
                quoted: true,
            });
            i += 1;
            continue;
        }
        // Identifier / keyword
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            out.push(Tok {
                text: &sql[start..i],
                quoted: false,
            });
            continue;
        }
        // Number — gather digits + dot + exponent; treated as one token
        if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            out.push(Tok {
                text: &sql[start..i],
                quoted: false,
            });
            continue;
        }
        // Punctuation / unknown — emit one char as a token. Step to the
        // next UTF-8 char boundary rather than blindly `+1`, otherwise a
        // non-ASCII byte (e.g. user paste of `é` / `λ` / a non-breaking
        // space) would slice mid-codepoint and panic.
        let mut end = i + 1;
        while end < bytes.len() && !sql.is_char_boundary(end) {
            end += 1;
        }
        out.push(Tok {
            text: &sql[i..end],
            quoted: false,
        });
        i = end;
    }
    out
}

struct TableName {
    schema: Option<String>,
    name: String,
}

/// Pull a `schema.name` or `name` table reference starting at `i`.
/// Returns `(parsed, next_index)` or `None` if the next token isn't an
/// identifier.
fn take_table_ref(tokens: &[Tok], i: usize) -> Option<(TableName, usize)> {
    let head = tokens.get(i)?;
    if !is_name_token(head) {
        return None;
    }
    // Schema.name?
    if let (Some(dot), Some(tail)) = (tokens.get(i + 1), tokens.get(i + 2)) {
        if !dot.quoted && dot.text == "." && is_name_token(tail) {
            return Some((
                TableName {
                    schema: Some(ident_value(head)),
                    name: ident_value(tail),
                },
                i + 3,
            ));
        }
    }
    Some((
        TableName {
            schema: None,
            name: ident_value(head),
        },
        i + 1,
    ))
}

/// Pull an optional alias after a table ref. Honours `AS x` and bare `x`,
/// but rejects words that introduce the next clause / next FROM-item.
fn take_alias(tokens: &[Tok], i: usize) -> (Option<String>, usize) {
    let stop_words = [
        "ON",
        "WHERE",
        "GROUP",
        "HAVING",
        "ORDER",
        "LIMIT",
        "FETCH",
        "OFFSET",
        "UNION",
        "INTERSECT",
        "EXCEPT",
        "JOIN",
        "INNER",
        "LEFT",
        "RIGHT",
        "FULL",
        "CROSS",
        "OUTER",
        "USING",
        "AS",
        "RETURNING",
    ];
    // A quoted word is never a keyword: `"where"` is an alias.
    let is_stop = |t: &Tok| !t.quoted && stop_words.contains(&t.text.to_ascii_uppercase().as_str());
    if let Some(t) = tokens.get(i) {
        if !t.quoted && t.text.eq_ignore_ascii_case("AS") {
            if let Some(alias) = tokens.get(i + 1) {
                // Reject keywords after AS — `FROM users AS JOIN orders`
                // should not consume `JOIN` as the alias of `users`.
                if is_name_token(alias) && !is_stop(alias) {
                    return (Some(ident_value(alias)), i + 2);
                }
            }
            return (None, i + 1);
        }
        if is_name_token(t) && !is_stop(t) {
            return (Some(ident_value(t)), i + 1);
        }
    }
    (None, i)
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

    fn refs(sql: &str) -> Vec<TableRefInQuery> {
        parse_from_tables(sql)
    }

    #[test]
    fn picks_up_simple_from() {
        let got = refs("SELECT * FROM users");
        assert_eq!(
            got,
            vec![TableRefInQuery {
                schema: None,
                name: "users".into(),
                alias: None,
                virtual_columns: None,
            }]
        );
    }

    #[test]
    fn captures_alias_with_as_and_without() {
        let got = refs("SELECT u.id FROM users AS u JOIN orders o ON o.user_id = u.id");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "users");
        assert_eq!(got[0].alias.as_deref(), Some("u"));
        assert_eq!(got[1].name, "orders");
        assert_eq!(got[1].alias.as_deref(), Some("o"));
    }

    #[test]
    fn handles_schema_qualified_table() {
        let got = refs("SELECT * FROM public.users u");
        assert_eq!(got[0].schema.as_deref(), Some("public"));
        assert_eq!(got[0].name, "users");
        assert_eq!(got[0].alias.as_deref(), Some("u"));
    }

    #[test]
    fn comma_separated_from_list() {
        let got = refs("SELECT * FROM a, b AS bb, c");
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].name, "a");
        assert_eq!(got[1].name, "b");
        assert_eq!(got[1].alias.as_deref(), Some("bb"));
        assert_eq!(got[2].name, "c");
    }

    #[test]
    fn stops_alias_at_known_keywords() {
        // `WHERE` immediately after the table — must NOT be taken as alias.
        let got = refs("SELECT * FROM users WHERE id = 1");
        assert_eq!(got.len(), 1);
        assert!(got[0].alias.is_none());
    }

    #[test]
    fn tolerates_incomplete_select_under_cursor() {
        // User is mid-typing: the cursor is in the SELECT list after `u.`
        let got = refs("SELECT u. FROM users u JOIN orders o ON o.user_id = u.id");
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn skips_string_literals_and_comments() {
        let got = refs("SELECT 'FROM hidden' /* FROM also-hidden */ -- FROM also\nFROM users u");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "users");
    }

    #[test]
    fn quoted_identifier_keeps_case() {
        let got = refs(r#"SELECT * FROM "MyTable" AS t"#);
        assert_eq!(got[0].name, "MyTable");
        assert_eq!(got[0].alias.as_deref(), Some("t"));
    }

    /// The server folds a bare name to lower case and keeps a quoted one
    /// verbatim; the parse must too, or `"PgRevMixed"` and `PgRevMixed`
    /// (the table `pgrevmixed`) come out the same.
    #[test]
    fn bare_identifiers_fold_and_quoted_ones_do_not() {
        let got = refs(r#"SELECT * FROM Public.PgRevMixed AS Pr"#);
        assert_eq!(got[0].schema.as_deref(), Some("public"));
        assert_eq!(got[0].name, "pgrevmixed");
        assert_eq!(got[0].alias.as_deref(), Some("pr"));
        let got = refs(r#"SELECT * FROM "Public"."PgRevMixed" "Pr""#);
        assert_eq!(got[0].schema.as_deref(), Some("Public"));
        assert_eq!(got[0].name, "PgRevMixed");
        assert_eq!(got[0].alias.as_deref(), Some("Pr"));
    }

    /// A quoted name may hold spaces, punctuation and `""`-escaped
    /// quotes; a quoted keyword is a name, not a clause.
    #[test]
    fn quoted_names_with_spaces_and_escaped_quotes() {
        let got = refs(r#"SELECT * FROM "my table" "where" WHERE x = 1"#);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "my table");
        assert_eq!(got[0].alias.as_deref(), Some("where"));
        let got = refs(r#"SELECT * FROM "a ""b"" c" JOIN t2 ON true"#);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "a \"b\" c");
        assert_eq!(got[1].name, "t2");
        // An empty quoted name is not a table.
        assert!(refs(r#"SELECT * FROM """#).is_empty());
    }

    #[test]
    fn picks_up_multiple_joins() {
        let got = refs(
            "SELECT * FROM a \
             JOIN b ON a.id = b.a_id \
             LEFT JOIN c cc ON cc.b_id = b.id",
        );
        assert_eq!(got.len(), 3);
        assert_eq!(got[2].alias.as_deref(), Some("cc"));
    }

    #[test]
    fn empty_input_yields_empty_vec() {
        assert!(refs("").is_empty());
    }

    #[test]
    fn tokenizer_does_not_panic_on_non_ascii_punctuation() {
        // Used to panic at `&sql[i..i+1]` when `i` landed on a multi-byte
        // codepoint outside strings/comments. Now char-boundary safe.
        // (Two FROM clauses parse since the em-dash isn't a clause break;
        // that's fine — we only care the call returned without crashing.)
        let got = parse_from_tables("SELECT * FROM users — note FROM email");
        assert!(
            got.iter().any(|t| t.name == "users"),
            "first FROM should still appear: {got:?}"
        );
    }

    #[test]
    fn tokenizer_handles_emoji_and_accents() {
        // Smoke: just don't crash. The em-dash, accented letter, and
        // emoji are all non-ASCII bytes that previously hit the
        // unguarded slice.
        let _ = parse_from_tables("SELECT 'café 🐘' FROM users");
        let _ = parse_from_tables("λ FROM x");
    }

    #[test]
    fn as_does_not_swallow_a_following_keyword_as_alias() {
        // `FROM users AS JOIN orders ...` — `JOIN` is a clause keyword,
        // not an alias. The downstream JOIN must still be parsed.
        let got = parse_from_tables("SELECT * FROM users AS JOIN orders o ON o.x = users.x");
        assert!(
            got.iter().any(|t| t.name == "orders"),
            "orders JOIN should still appear in scope: {got:?}"
        );
    }

    #[test]
    fn subquery_alias_with_as_is_in_scope() {
        let got = refs("SELECT * FROM (SELECT a FROM users) AS sub WHERE 1=1");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "sub");
        assert_eq!(got[0].alias.as_deref(), Some("sub"));
    }

    #[test]
    fn subquery_alias_without_as_is_in_scope() {
        let got = refs("SELECT * FROM (SELECT a FROM users) sub JOIN orders o ON true");
        assert!(got
            .iter()
            .any(|t| t.name == "sub" && t.alias.as_deref() == Some("sub")));
        assert!(got.iter().any(|t| t.name == "orders"));
    }

    #[test]
    fn lateral_subquery_alias_in_scope_and_no_phantom_lateral_table() {
        // `FROM users u, LATERAL (SELECT ...) sub` — `sub` is in
        // scope, and `LATERAL` is NOT captured as a phantom table.
        let got = refs("SELECT * FROM users u, LATERAL (SELECT 1) sub WHERE 1=1");
        let names: Vec<&str> = got.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"users"));
        assert!(names.contains(&"sub"));
        assert!(
            !names.contains(&"LATERAL"),
            "LATERAL should not appear as a table: {names:?}"
        );
    }

    #[test]
    fn subquery_with_nested_parens_alias_still_captured() {
        // Aggregations / sub-sub queries inside don't confuse the
        // paren-depth scanner.
        let got = refs("SELECT * FROM (SELECT COUNT(*) FROM (SELECT 1) x) sub");
        assert!(got.iter().any(|t| t.name == "sub"));
    }

    #[test]
    fn match_key_prefers_alias() {
        let r = TableRefInQuery {
            schema: None,
            name: "Users".into(),
            alias: Some("u".into()),
            virtual_columns: None,
        };
        assert_eq!(r.match_key(), "u");
        let r2 = TableRefInQuery {
            schema: None,
            name: "Users".into(),
            alias: None,
            virtual_columns: None,
        };
        assert_eq!(r2.match_key(), "users");
    }
}
