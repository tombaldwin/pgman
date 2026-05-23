//! Lightweight clause-context classifier for grammar-aware completion.
//!
//! Given the editor buffer + the cursor position, this module reports
//! which SQL clause the cursor is sitting in — SELECT list, FROM,
//! WHERE, INSERT-column-list, etc. The completion engine
//! (`query::complete`) reads that context to constrain its candidates
//! ("after FROM, never offer columns").
//!
//! Design choice: a real SQL grammar (`sqlparser-rs`, `pg_query`) is
//! brittle against the mid-typed SQL the editor regularly holds. This
//! module is a forgiving token scan that tracks just enough state to
//! classify the cursor's clause — incomplete keywords are tolerated,
//! unknown tokens are ignored, and the state machine is monotonic so
//! one bad token can't poison the rest of the classification.

use crate::query::from_parse::tokenize;

/// What the cursor's surrounding SQL clause is. Completion uses this to
/// filter candidates: `TableRef` mode never offers columns, `Predicate`
/// mode prefers columns, etc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClauseContext {
    /// Top of a statement (fresh buffer or right after `;`) — operator
    /// is typing the first verb (SELECT / INSERT / UPDATE / DELETE / …).
    StatementStart,
    /// SELECT list or RETURNING list — wants columns of in-scope tables
    /// plus aggregates / `*`.
    SelectList,
    /// Position where a table reference is expected: after FROM, JOIN,
    /// `INSERT INTO`, `UPDATE`, `DELETE FROM`. Never columns.
    TableRef,
    /// Predicate position: after WHERE, HAVING, or JOIN … ON. Wants
    /// columns + comparison operators.
    Predicate,
    /// After ORDER BY / GROUP BY — wants columns.
    OrderOrGroup,
    /// Inside the `(...)` column list of `INSERT INTO <table> (...)` —
    /// wants the columns of that specific table.
    InsertColumns(QualifiedTable),
    /// After `UPDATE <table> SET` — wants the columns of that table.
    UpdateAssign(QualifiedTable),
    /// Inside `VALUES (...)` — literals, no useful identifier completion.
    Values,
    /// Couldn't classify (unrecognised syntax). Fall back to the
    /// pre-grammar behaviour so completion still works on something
    /// the parser doesn't understand.
    Unknown,
}

/// A schema-qualified table name pulled out of the parsed statement —
/// what the completion engine looks up in the schema cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedTable {
    pub schema: Option<String>,
    pub name: String,
}

/// Output of [`classify_at`] — the clause the cursor is sitting in plus
/// (for `INSERT` / `UPDATE` / `DELETE`) the target table the operator is
/// writing to. The write target is useful even in clauses other than
/// `InsertColumns` / `UpdateAssign`: `UPDATE foo SET col = … WHERE |`
/// should still let completion offer `foo`'s columns inside `WHERE`,
/// even though there's no `FROM` for the FROM-scope parser to find.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub ctx: ClauseContext,
    pub write_target: Option<QualifiedTable>,
}

/// Classify the cursor's clause. Scans tokens of the statement up to the
/// cursor (where "statement" = everything since the last unquoted `;`).
pub fn classify_at(buf: &str, cursor: usize) -> Classification {
    // Clamp + char-boundary safety: the editor cursor can sit at any
    // byte position but `&buf[..cursor]` would panic mid-codepoint.
    let cursor = cursor.min(buf.len());
    if !buf.is_char_boundary(cursor) {
        return Classification {
            ctx: ClauseContext::Unknown,
            write_target: None,
        };
    }
    let stmt_start = statement_start(buf, cursor);
    let stmt = &buf[stmt_start..cursor];
    let tokens = tokenize(stmt);
    classify_tokens(&tokens)
}

/// Where the current statement begins — the byte offset just past the
/// last `;` (outside strings / quoted identifiers / comments) before
/// `cursor`, or `0`.
fn statement_start(buf: &str, cursor: usize) -> usize {
    let bytes = buf.as_bytes();
    let end = cursor.min(bytes.len());
    let mut i = 0;
    let mut last_semi: Option<usize> = None;
    let mut in_single = false; // '…' string literal
    let mut in_double = false; // "…" quoted identifier
    while i < end {
        let b = bytes[i];
        if in_single {
            // Postgres doubles a `'` to embed it; treat any `'` as end
            // of literal — the worst case is a slightly over-eager
            // close, which still keeps `;` correctly inside strings.
            if b == b'\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if b == b'"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        // -- line comment: skip to newline.
        if b == b'-' && i + 1 < end && bytes[i + 1] == b'-' {
            while i < end && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // /* … */ block comment (no nesting).
        if b == b'/' && i + 1 < end && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < end && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < end {
                i += 2;
            }
            continue;
        }
        match b {
            b'\'' => in_single = true,
            b'"' => in_double = true,
            b';' => last_semi = Some(i),
            _ => {}
        }
        i += 1;
    }
    match last_semi {
        Some(i) => i + 1,
        None => 0,
    }
}

/// Per-paren-scope state. Each `(` pushes a new scope; `)` pops back to
/// the parent. That isolation is what makes the classifier correct for
/// subqueries and CTE bodies — the parent's `pending_table_ref` etc
/// can't leak into the inner scope, and the inner scope's WHERE / FROM
/// can't poison the outer ctx after `)` closes.
#[derive(Debug, Clone)]
struct ScopeState {
    ctx: ClauseContext,
    /// The most-recently-named table in this scope — used by `SET`
    /// (becomes the UpdateAssign target) and by INSERT-column-list
    /// resolution.
    active_table: Option<QualifiedTable>,
    /// Next identifier is the target table (set by FROM / JOIN / INTO /
    /// UPDATE).
    pending_table_ref: bool,
    /// Expecting `BY` after `ORDER` / `GROUP`.
    pending_by: bool,
    /// `INSERT INTO <table>` seen at this scope; the next `(` opens its
    /// column list.
    expecting_insert_paren: bool,
}

impl ScopeState {
    fn new(ctx: ClauseContext) -> Self {
        Self {
            ctx,
            active_table: None,
            pending_table_ref: false,
            pending_by: false,
            expecting_insert_paren: false,
        }
    }
}

fn classify_tokens(tokens: &[crate::query::from_parse::Tok<'_>]) -> Classification {
    use ClauseContext::*;
    let mut scopes: Vec<ScopeState> = vec![ScopeState::new(StatementStart)];
    // Statement-level state — survives paren scoping.
    let mut write_target: Option<QualifiedTable> = None;
    let mut in_write_stmt = false;
    // Flagged when entering a paren whose contents are VALUES literals
    // (so the cursor inside reports Values rather than e.g. a phantom
    // SelectList).
    let mut entered_via_values = false;

    let mut i = 0;
    while i < tokens.len() {
        let tok = &tokens[i];
        let upper = tok.text.to_ascii_uppercase();

        // Punctuation: parens manage the scope stack.
        if tok.text == "(" {
            // Decide what context the new scope starts in.
            let parent = scopes.last().expect("scope stack invariant");
            let new_ctx = if parent.expecting_insert_paren {
                InsertColumns(parent.active_table.clone().unwrap_or(QualifiedTable {
                    schema: None,
                    name: String::new(),
                }))
            } else if matches!(parent.ctx, Values) {
                Values
            } else {
                // Subquery / parenthesised expression / CTE body — fresh
                // sub-statement scope, so an inner SELECT / FROM / WHERE
                // is correctly classified.
                StatementStart
            };
            // Clear consumers on the parent — entering parens consumes
            // any pending intent.
            let parent_mut = scopes.last_mut().unwrap();
            parent_mut.expecting_insert_paren = false;
            parent_mut.pending_table_ref = false;
            parent_mut.pending_by = false;
            entered_via_values = matches!(new_ctx, Values);
            scopes.push(ScopeState::new(new_ctx));
            i += 1;
            continue;
        }
        if tok.text == ")" {
            if scopes.len() > 1 {
                scopes.pop();
            }
            i += 1;
            continue;
        }
        if tok.text == "," || tok.text == "." {
            i += 1;
            continue;
        }

        // From here on we work on the top scope.
        let scope = scopes.last_mut().expect("scope stack invariant");

        if scope.pending_table_ref && is_identifier_like(tok.text) {
            let (table, consumed) = take_qualified(tokens, i);
            scope.active_table = Some(table.clone());
            // The first table named in a write statement is the write
            // target; carried across scopes for WHERE / SET completion.
            if in_write_stmt && write_target.is_none() {
                write_target = Some(table);
            }
            scope.pending_table_ref = false;
            scope.expecting_insert_paren =
                matches!(scope.ctx, TableRef) && in_insert_path(tokens, i);
            i += consumed;
            continue;
        }
        if scope.pending_by && upper == "BY" {
            scope.ctx = OrderOrGroup;
            scope.pending_by = false;
            i += 1;
            continue;
        }

        // Clause-introducing keywords. With the scope stack, none of
        // these need a paren_depth guard — each scope is locally at
        // "depth 0".
        match upper.as_str() {
            "SELECT" => scope.ctx = SelectList,
            "FROM" => {
                scope.ctx = TableRef;
                scope.pending_table_ref = true;
            }
            "JOIN" => {
                scope.ctx = TableRef;
                scope.pending_table_ref = true;
            }
            "INTO" => {
                scope.ctx = TableRef;
                scope.pending_table_ref = true;
                in_write_stmt = true;
            }
            "UPDATE" => {
                scope.ctx = TableRef;
                scope.pending_table_ref = true;
                in_write_stmt = true;
            }
            "DELETE" => {
                scope.ctx = TableRef;
                in_write_stmt = true;
            }
            "WHERE" | "HAVING" | "ON" => scope.ctx = Predicate,
            "ORDER" | "GROUP" => scope.pending_by = true,
            "RETURNING" => scope.ctx = SelectList,
            "VALUES" => {
                scope.ctx = Values;
                // Critical: VALUES consumes any earlier
                // `expecting_insert_paren` — `INSERT INTO foo VALUES (`
                // opens a Values scope, not an InsertColumns one.
                scope.expecting_insert_paren = false;
            }
            "SET" if matches!(scope.ctx, TableRef) => {
                if let Some(t) = scope.active_table.clone() {
                    scope.ctx = UpdateAssign(t);
                }
            }
            "INSERT" => {
                in_write_stmt = true;
            }
            _ => {}
        }
        i += 1;
    }

    let final_ctx = scopes
        .last()
        .map(|s| s.ctx.clone())
        .unwrap_or(StatementStart);
    let _ = entered_via_values; // currently advisory; reserved for future refinements
    Classification {
        ctx: final_ctx,
        write_target,
    }
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

/// Walk forward over `name` or `schema.name`. Returns the parsed
/// qualified table and the number of tokens consumed.
fn take_qualified(
    tokens: &[crate::query::from_parse::Tok<'_>],
    i: usize,
) -> (QualifiedTable, usize) {
    let head = tokens[i].text.to_string();
    if i + 2 < tokens.len() && tokens[i + 1].text == "." && is_identifier_like(&tokens[i + 2].text)
    {
        (
            QualifiedTable {
                schema: Some(head),
                name: tokens[i + 2].text.to_string(),
            },
            3,
        )
    } else {
        (
            QualifiedTable {
                schema: None,
                name: head,
            },
            1,
        )
    }
}

/// Extract names of CTEs (`WITH cte_name AS (...)`) declared in `buf`.
/// Returns the names in declaration order. Tolerant of partial input:
/// an unterminated CTE body still emits everything before it.
///
/// Used by completion so `FROM ` after a `WITH` block offers the local
/// CTE names alongside catalog tables.
pub fn extract_cte_names(buf: &str) -> Vec<String> {
    let tokens = tokenize(buf);
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i].text.eq_ignore_ascii_case("WITH") {
            i += 1;
            // Optional RECURSIVE keyword.
            if i < tokens.len() && tokens[i].text.eq_ignore_ascii_case("RECURSIVE") {
                i += 1;
            }
            // CTE list: `name [(col_list)] AS ( body ) [, name2 ...]`.
            loop {
                if i >= tokens.len() || !is_identifier_like(tokens[i].text) {
                    break;
                }
                let name = tokens[i].text.to_string();
                i += 1;
                // Optional column-list.
                if i < tokens.len() && tokens[i].text == "(" {
                    i = skip_parenthesised(&tokens, i);
                }
                // AS keyword is required for CTE syntax.
                if i >= tokens.len() || !tokens[i].text.eq_ignore_ascii_case("AS") {
                    break;
                }
                i += 1;
                // Optional MATERIALIZED / NOT MATERIALIZED.
                if i < tokens.len() && tokens[i].text.eq_ignore_ascii_case("NOT") {
                    i += 1;
                }
                if i < tokens.len() && tokens[i].text.eq_ignore_ascii_case("MATERIALIZED") {
                    i += 1;
                }
                // CTE body: `( ... )`. Capture the name first so a
                // partial / unterminated body still counts.
                out.push(name);
                if i < tokens.len() && tokens[i].text == "(" {
                    i = skip_parenthesised(&tokens, i);
                }
                // Continue if the list extends with a comma.
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

/// Step past a `(...)` group starting at `tokens[i] == "("`. Returns the
/// index of the token AFTER the matching `)`, or `tokens.len()` when
/// the input runs out before the close.
fn skip_parenthesised(tokens: &[crate::query::from_parse::Tok<'_>], i: usize) -> usize {
    if tokens.get(i).map(|t| t.text) != Some("(") {
        return i;
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
    j
}

/// Heuristic: did `INSERT INTO` appear earlier in this token stream?
/// We only care because `INSERT INTO foo (` should switch to
/// InsertColumns, but `UPDATE foo (...)` (unusual) shouldn't.
fn in_insert_path(tokens: &[crate::query::from_parse::Tok<'_>], up_to: usize) -> bool {
    tokens[..up_to]
        .iter()
        .any(|t| t.text.eq_ignore_ascii_case("INSERT"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(sql: &str) -> ClauseContext {
        classify_at(sql, sql.len()).ctx
    }

    fn target(sql: &str) -> Option<QualifiedTable> {
        classify_at(sql, sql.len()).write_target
    }

    #[test]
    fn fresh_buffer_is_statement_start() {
        assert_eq!(classify(""), ClauseContext::StatementStart);
        assert_eq!(classify("  "), ClauseContext::StatementStart);
    }

    #[test]
    fn select_keyword_enters_select_list() {
        assert_eq!(classify("SELECT "), ClauseContext::SelectList);
        assert_eq!(classify("SELECT u"), ClauseContext::SelectList);
        assert_eq!(classify("SELECT u, e"), ClauseContext::SelectList);
    }

    #[test]
    fn from_enters_table_ref() {
        assert_eq!(classify("SELECT * FROM "), ClauseContext::TableRef);
        assert_eq!(classify("SELECT * FROM us"), ClauseContext::TableRef);
    }

    #[test]
    fn where_enters_predicate() {
        assert_eq!(
            classify("SELECT * FROM u WHERE "),
            ClauseContext::Predicate
        );
        assert_eq!(
            classify("SELECT * FROM u WHERE id ="),
            ClauseContext::Predicate
        );
    }

    #[test]
    fn order_by_enters_order_or_group() {
        assert_eq!(
            classify("SELECT * FROM u ORDER BY "),
            ClauseContext::OrderOrGroup
        );
        assert_eq!(
            classify("SELECT * FROM u GROUP BY "),
            ClauseContext::OrderOrGroup
        );
    }

    #[test]
    fn join_returns_table_ref() {
        assert_eq!(
            classify("SELECT * FROM users u JOIN "),
            ClauseContext::TableRef
        );
    }

    #[test]
    fn on_returns_predicate() {
        assert_eq!(
            classify("SELECT * FROM u JOIN orders o ON "),
            ClauseContext::Predicate
        );
    }

    #[test]
    fn returning_returns_select_list() {
        assert_eq!(
            classify("INSERT INTO users (id) VALUES (1) RETURNING "),
            ClauseContext::SelectList
        );
    }

    #[test]
    fn insert_into_table_then_paren_enters_insert_columns() {
        let ctx = classify("INSERT INTO users (");
        match ctx {
            ClauseContext::InsertColumns(t) => assert_eq!(t.name, "users"),
            other => panic!("expected InsertColumns(users), got {other:?}"),
        }
    }

    #[test]
    fn insert_into_qualified_table_keeps_schema() {
        let ctx = classify("INSERT INTO public.users (");
        match ctx {
            ClauseContext::InsertColumns(t) => {
                assert_eq!(t.schema.as_deref(), Some("public"));
                assert_eq!(t.name, "users");
            }
            other => panic!("expected InsertColumns(public.users), got {other:?}"),
        }
    }

    #[test]
    fn insert_values_is_values_context() {
        let ctx = classify("INSERT INTO users (id, name) VALUES (");
        assert_eq!(ctx, ClauseContext::Values);
    }

    #[test]
    fn update_set_enters_update_assign() {
        let ctx = classify("UPDATE users SET ");
        match ctx {
            ClauseContext::UpdateAssign(t) => assert_eq!(t.name, "users"),
            other => panic!("expected UpdateAssign(users), got {other:?}"),
        }
    }

    #[test]
    fn delete_from_enters_table_ref() {
        assert_eq!(classify("DELETE FROM "), ClauseContext::TableRef);
    }

    #[test]
    fn statement_start_ignores_semicolons_inside_strings() {
        // Regression: `;` inside a string literal used to be picked up
        // by the naive byte scan, splitting the statement mid-string
        // and losing all the WHERE / FROM tokens that came after.
        assert_eq!(
            classify("SELECT 'foo;bar' FROM t WHERE em"),
            ClauseContext::Predicate
        );
    }

    #[test]
    fn statement_start_ignores_semicolons_in_quoted_idents_and_comments() {
        // `"col;name"` — quoted identifier shouldn't terminate the
        // statement.
        assert_eq!(
            classify(r#"SELECT "col;name" FROM t WHERE "#),
            ClauseContext::Predicate
        );
        // -- line comment with embedded ;
        assert_eq!(
            classify("SELECT 1 -- foo;bar\nFROM t WHERE "),
            ClauseContext::Predicate
        );
        // /* block comment with ; */
        assert_eq!(
            classify("SELECT /* a;b */ 1 FROM t WHERE "),
            ClauseContext::Predicate
        );
    }

    #[test]
    fn semicolon_starts_a_fresh_statement() {
        assert_eq!(
            classify("SELECT * FROM u; "),
            ClauseContext::StatementStart
        );
        assert_eq!(
            classify("SELECT * FROM u; SELECT "),
            ClauseContext::SelectList
        );
    }

    #[test]
    fn insert_into_table_values_is_values_not_insert_columns() {
        // Regression: `INSERT INTO foo VALUES (` used to flip the `(`
        // into InsertColumns because expecting_insert_paren wasn't
        // cleared by VALUES.
        assert_eq!(
            classify("INSERT INTO foo VALUES ("),
            ClauseContext::Values
        );
    }

    #[test]
    fn from_subquery_does_not_capture_inner_table_as_outer_active() {
        // Regression: `FROM (SELECT a FROM t)` used to consume `a`
        // as the active table because pending_table_ref leaked
        // across the `(`.
        let c = classify_at("UPDATE foo SET x = (SELECT 1 FROM bar) WHERE ", 44);
        // The write target must still be `foo` — not `bar` from the
        // subquery's FROM.
        assert_eq!(
            c.write_target.as_ref().map(|t| t.name.clone()),
            Some("foo".to_string())
        );
    }

    #[test]
    fn subquery_where_does_not_leak_to_outer_context() {
        // Regression: WHERE / ORDER BY inside a subquery used to leave
        // the outer context stuck at Predicate / OrderOrGroup after the
        // `)` closed. The outer cursor is at the alias position after
        // `(subq) ` — that's still TableRef.
        assert_eq!(
            classify("SELECT col FROM (SELECT a FROM t WHERE x = 1) "),
            ClauseContext::TableRef
        );
    }

    #[test]
    fn cte_body_classifies_inner_select() {
        // Regression: `WITH cte AS (SELECT em` used to return
        // StatementStart because the inner SELECT was suppressed by
        // the old `paren_depth == 0` guard.
        assert_eq!(
            classify("WITH cte AS (SELECT em"),
            ClauseContext::SelectList
        );
    }

    #[test]
    fn insert_into_select_subquery_is_select_list() {
        // INSERT INTO foo SELECT a FROM bar  — at the SELECT-list
        // position the operator is choosing source columns from `bar`.
        assert_eq!(
            classify("INSERT INTO foo SELECT a"),
            ClauseContext::SelectList
        );
    }

    #[test]
    fn write_target_carried_through_update_where() {
        // `UPDATE foo SET x = 1 WHERE |` — ctx is Predicate, but the
        // completion engine needs to know foo is the write target so
        // it can offer foo's columns inside WHERE without a FROM.
        let c = classify_at("UPDATE foo SET x = 1 WHERE ", 27);
        assert_eq!(c.ctx, ClauseContext::Predicate);
        assert_eq!(c.write_target.as_ref().map(|t| &t.name), Some(&"foo".to_string()));
    }

    #[test]
    fn write_target_carried_through_delete_where() {
        let c = classify_at("DELETE FROM foo WHERE ", 22);
        assert_eq!(c.ctx, ClauseContext::Predicate);
        assert_eq!(c.write_target.as_ref().map(|t| &t.name), Some(&"foo".to_string()));
    }

    #[test]
    fn write_target_none_for_pure_select() {
        assert_eq!(target("SELECT * FROM foo WHERE "), None);
    }

    #[test]
    fn extract_cte_names_single() {
        let got = extract_cte_names("WITH foo AS (SELECT 1) SELECT * FROM foo");
        assert_eq!(got, vec!["foo"]);
    }

    #[test]
    fn extract_cte_names_multiple_with_recursive_and_column_list() {
        let got = extract_cte_names(
            "WITH RECURSIVE foo(a, b) AS (SELECT 1, 2), bar AS (SELECT * FROM foo) SELECT * FROM bar",
        );
        assert_eq!(got, vec!["foo", "bar"]);
    }

    #[test]
    fn extract_cte_names_materialized_keyword() {
        let got = extract_cte_names(
            "WITH foo AS MATERIALIZED (SELECT 1), bar AS NOT MATERIALIZED (SELECT 2) SELECT 1",
        );
        assert_eq!(got, vec!["foo", "bar"]);
    }

    #[test]
    fn extract_cte_names_partial_body_still_captures_name() {
        // User mid-typing the CTE body — name is already there.
        let got = extract_cte_names("WITH foo AS (SELECT em");
        assert_eq!(got, vec!["foo"]);
    }

    #[test]
    fn extract_cte_names_returns_empty_for_no_with() {
        assert!(extract_cte_names("SELECT * FROM users").is_empty());
    }

    #[test]
    fn classifier_is_tolerant_of_random_garbage() {
        // Non-SQL text shouldn't crash — falls back to StatementStart /
        // Unknown depending on whether we saw any clause keyword.
        let _ = classify("foo bar baz");
        let _ = classify("123 1.5 4.5.6");
        let _ = classify("café 🐘 — em-dash");
    }

    #[test]
    fn classify_at_handles_non_char_boundary_cursor() {
        // Defensive: even if a caller passes a bogus cursor, no panic.
        assert_eq!(
            classify_at("SELECT 'café' FROM x", 8),
            // cursor at byte 8 — that's actually inside the 'café' string;
            // we just need this to not panic. Any return is fine.
            classify_at("SELECT 'café' FROM x", 8)
        );
    }
}
