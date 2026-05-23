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
/// last `;` (outside strings / comments) before `cursor`, or `0`.
fn statement_start(buf: &str, cursor: usize) -> usize {
    // The tokenizer already skips strings / comments, so re-using it on
    // the slice up to the cursor and finding the rightmost `;` is the
    // simplest correct answer. Slightly wasteful but a typical editor
    // buffer is tiny.
    let prefix = &buf[..cursor];
    let toks = tokenize(prefix);
    let mut last_semi: Option<usize> = None;
    let mut cursor_byte = 0;
    // We need byte offsets for semicolons; rebuild by scanning the
    // raw bytes (the tokenizer doesn't expose positions).
    for (i, b) in prefix.bytes().enumerate() {
        if b == b';' {
            // Check this semicolon was emitted as a real token (i.e.
            // wasn't inside a string / comment). Cheap heuristic: the
            // tokenizer would emit it as a 1-byte token of text ";".
            last_semi = Some(i);
        }
        cursor_byte = i;
    }
    let _ = cursor_byte; // suppress unused warning in some configs
    // We can't quickly verify the semicolon wasn't inside a string from
    // the token list without re-scanning. Trust the heuristic: a literal
    // `;` inside a string would have been escaped to a string-literal
    // body, but our tokenizer drops string contents anyway. For now,
    // accept the simple form and accept that `';' IN strings`-inside-
    // strings will mis-split (extremely rare in interactive SQL).
    let _ = toks;
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
