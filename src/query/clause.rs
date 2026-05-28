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
    /// Predicate position after WHERE / JOIN … ON. Wants columns +
    /// comparison operators. (HAVING is its own variant because
    /// Postgres lets it reference SELECT-list aliases that WHERE can't.)
    Predicate,
    /// HAVING predicate — like `Predicate` plus access to the SELECT
    /// list's output column aliases.
    HavingPredicate,
    /// After ORDER BY / GROUP BY — wants columns.
    OrderOrGroup,
    /// Inside the `(...)` column list of `INSERT INTO <table> (...)` —
    /// wants the columns of that specific table.
    InsertColumns(QualifiedTable),
    /// Inside `EXPLAIN (...)` — option-name position. Wants tokens like
    /// `ANALYZE`, `BUFFERS`, etc.
    ExplainOptions,
    /// Inside `VACUUM (...)` or `ANALYZE (...)` — option-name position.
    /// Wants entries from `vocabulary::VACUUM_OPTIONS`.
    VacuumOptions,
    /// `SHOW |` or `SET |` — the operator is naming a GUC parameter.
    /// We don't distinguish SHOW vs SET because the candidate set is
    /// the same (parameter names); the value side flips to `GucValue`
    /// once `=` appears.
    GucParameter,
    /// `SET <param> = |` — the operator is typing the value. Wants
    /// `on` / `off` / `true` / `false` / `default`. String-typed GUCs
    /// (timezone, search_path) accept any string literal — we only
    /// offer the universal values here.
    GucValue,
    /// Inside `CAST(expr AS |)` — the operator is naming a SQL type.
    /// Wants entries from `vocabulary::TYPE_NAMES`.
    TypeName,
    /// `CREATE TABLE t (|` — column-name position (just after `(` or
    /// `,`). No completion: the operator is naming a fresh column and
    /// we have nothing useful to offer.
    CreateTableColumns,
    /// `CREATE TABLE t (id |` — type-name position (just after a
    /// column name). Wants entries from `vocabulary::TYPE_NAMES`.
    CreateTableColumnType,
    /// `ON CONFLICT ON CONSTRAINT | …` — the operator is naming a
    /// unique / primary-key constraint on the INSERT target. The
    /// completion engine filters `SchemaCache::constraints` by the
    /// write target's table name.
    ConstraintName,
    /// `DROP <kind> | …` — the operator is naming the relation to drop.
    /// `kind` selects which catalog set to suggest (tables / sequences
    /// / indexes / …). All variants get `IF EXISTS` / `CASCADE` /
    /// `RESTRICT` continuations alongside.
    DropTarget(DropKind),
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

/// Which kind of catalog object a `DROP` statement is naming. Determines
/// which `SchemaCache` field the completion engine pulls candidates
/// from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropKind {
    /// `DROP TABLE` / `DROP VIEW` / `DROP MATERIALIZED VIEW` — all
    /// resolve to `cache.tables`.
    Table,
    Index,
    Sequence,
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
    /// `INSERT INTO <table>` (or `COPY <table>`) seen at this scope;
    /// the next `(` opens the column list.
    expecting_insert_paren: bool,
    /// `EXPLAIN` seen; the next `(` opens its option list.
    expecting_explain_paren: bool,
    /// `VACUUM` or `ANALYZE` seen; the next `(` opens its option list
    /// (same shape as EXPLAIN's option list).
    expecting_vacuum_paren: bool,
    /// `CAST` seen; the next `(` opens its argument paren. The child
    /// scope inherits `is_cast_scope = true` so a subsequent `AS`
    /// inside flips ctx to `TypeName` instead of being read as an
    /// alias introducer.
    expecting_cast_paren: bool,
    /// This scope sits inside the parens of a `CAST(...)`. Set by the
    /// `(` handler when the parent was `expecting_cast_paren`.
    is_cast_scope: bool,
    /// We've passed an `ON CONFLICT` in this scope. Set on `CONFLICT`
    /// so that a subsequent `DO UPDATE` doesn't reset the classifier
    /// state as if it were the start of an UPDATE statement.
    in_on_conflict: bool,
    /// `DROP` or `REINDEX` seen; the next keyword (TABLE / VIEW /
    /// MATERIALIZED VIEW / INDEX / SEQUENCE / SCHEMA / ...) selects
    /// which kind of object the operator is naming. INDEX → fetches
    /// from cache.indexes; SEQUENCE → cache.sequences; TABLE/VIEW →
    /// cache.tables. Other kinds (FUNCTION / TYPE / DATABASE / ROLE
    /// etc.) leave the ctx unchanged — no useful candidates from the
    /// current cache.
    pending_drop_kind: bool,
    /// `CREATE` seen at this scope. The next `TABLE` token (possibly
    /// after `TEMP` / `UNLOGGED` / `IF NOT EXISTS`) selects the
    /// CREATE TABLE path.
    pending_create_kind: bool,
    /// `CREATE TABLE <name>` seen; the next `(` opens the column
    /// definition list.
    expecting_create_table_paren: bool,
}

impl ScopeState {
    fn new(ctx: ClauseContext) -> Self {
        Self {
            ctx,
            active_table: None,
            pending_table_ref: false,
            pending_by: false,
            expecting_insert_paren: false,
            expecting_explain_paren: false,
            expecting_vacuum_paren: false,
            expecting_cast_paren: false,
            is_cast_scope: false,
            in_on_conflict: false,
            pending_drop_kind: false,
            pending_create_kind: false,
            expecting_create_table_paren: false,
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
            let inherits_cast = parent.expecting_cast_paren;
            let new_ctx = if parent.expecting_explain_paren {
                ExplainOptions
            } else if parent.expecting_vacuum_paren {
                VacuumOptions
            } else if parent.expecting_create_table_paren {
                CreateTableColumns
            } else if parent.expecting_insert_paren {
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
            parent_mut.expecting_explain_paren = false;
            parent_mut.expecting_vacuum_paren = false;
            parent_mut.expecting_cast_paren = false;
            parent_mut.expecting_create_table_paren = false;
            parent_mut.pending_table_ref = false;
            parent_mut.pending_by = false;
            entered_via_values = matches!(new_ctx, Values);
            let mut child = ScopeState::new(new_ctx);
            child.is_cast_scope = inherits_cast;
            scopes.push(child);
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
        if tok.text == "," {
            // Inside a CREATE TABLE column list, `,` returns us to the
            // column-name slot from a column-type slot. Other clauses
            // don't toggle on commas.
            let scope = scopes.last_mut().expect("scope stack invariant");
            if matches!(scope.ctx, CreateTableColumnType) {
                scope.ctx = CreateTableColumns;
            }
            i += 1;
            continue;
        }
        if tok.text == "." {
            i += 1;
            continue;
        }
        // `=` inside `SET <param> = …` flips ctx to the value side so
        // completion offers `on` / `off` / `default` rather than more
        // parameter names. `SHOW timezone` has no `=` so SHOW
        // completion is unaffected.
        if tok.text == "=" {
            let scope = scopes.last_mut().expect("scope stack invariant");
            if matches!(scope.ctx, GucParameter) {
                scope.ctx = GucValue;
            }
            i += 1;
            continue;
        }

        // From here on we work on the top scope.
        let scope = scopes.last_mut().expect("scope stack invariant");
        // Once we're in GucValue (`SET timezone = |`), the operator is
        // typing the value — `ON` (the SQL keyword) is also `on` (the
        // boolean GUC value), and we don't want it to flip the ctx
        // back to Predicate. Stay in GucValue until the statement ends
        // (next `;`).
        if matches!(scope.ctx, GucValue) {
            i += 1;
            continue;
        }

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

        // `expecting_create_table_paren` is a one-shot expectation
        // that the NEXT token is the `(` of a CREATE TABLE column
        // list. The `(` handler at the top of the loop consumes it.
        // If we reach the keyword match instead, the next token is
        // something other than `(` — clear the flag so a later
        // subquery `(` (e.g. `CREATE TABLE t AS SELECT … FROM (…)`)
        // doesn't get treated as a column-definition paren. Keep
        // the flag while `pending_table_ref` is still active (the
        // window between `TABLE` and the table name; intermediate
        // modifiers like `IF NOT EXISTS` sit there).
        if !scope.pending_table_ref {
            scope.expecting_create_table_paren = false;
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
            // `UPDATE` normally starts a statement. Inside an
            // `ON CONFLICT DO UPDATE` clause it's a continuation —
            // don't reset state (especially pending_table_ref, which
            // would consume the next keyword `SET` as a table name).
            "UPDATE" if !scope.in_on_conflict => {
                scope.ctx = TableRef;
                scope.pending_table_ref = true;
                in_write_stmt = true;
            }
            "UPDATE" => {
                // ON CONFLICT DO UPDATE — no-op; SET will rebind ctx.
            }
            "DELETE" => {
                scope.ctx = TableRef;
                in_write_stmt = true;
            }
            "WHERE" => scope.ctx = Predicate,
            "ON" => {
                // Peek-ahead: `ON CONFLICT` opens an upsert clause
                // (handled by the CONFLICT keyword arm), not a
                // predicate. And inside an existing ON CONFLICT, any
                // ON belongs to the conflict spec (`ON CONSTRAINT`
                // is the canonical use). Either way, skip the
                // Predicate flip. Plain `JOIN x ON x.id = y.id` still
                // routes to Predicate.
                let next_is_conflict = tokens
                    .get(i + 1)
                    .map(|t| t.text.eq_ignore_ascii_case("CONFLICT"))
                    .unwrap_or(false);
                if !next_is_conflict && !scope.in_on_conflict {
                    scope.ctx = Predicate;
                }
            }
            "HAVING" => scope.ctx = HavingPredicate,
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
            // ON CONFLICT DO UPDATE SET col = … — the SET keyword
            // arrives long after the TableRef phase ended. Use the
            // write target so the assignment-list completion still
            // finds the right columns. Must check BEFORE the
            // SHOW/SET → GucParameter arm below.
            "SET" if scope.in_on_conflict && write_target.is_some() => {
                if let Some(t) = write_target.clone() {
                    scope.ctx = UpdateAssign(t);
                }
            }
            "INSERT" => {
                in_write_stmt = true;
            }
            // COPY tab (col1, col2) FROM '/path/to/file' — the column
            // list after the table reuses InsertColumns semantics.
            // TRUNCATE tab — just a table reference, no column list.
            "COPY" | "TRUNCATE" => {
                scope.ctx = TableRef;
                scope.pending_table_ref = true;
                in_write_stmt = true;
            }
            // ON CONFLICT … — the optional `(col_list)` after CONFLICT
            // names columns of the INSERT target, same shape as the
            // INSERT column list. Set the same flag the next `(`
            // consumes.
            "CONFLICT" => {
                scope.expecting_insert_paren = true;
                scope.in_on_conflict = true;
            }
            // `ON CONFLICT ON CONSTRAINT <name>` — the operator is
            // naming a unique / primary-key constraint on the target.
            "CONSTRAINT" if scope.in_on_conflict => {
                scope.ctx = ConstraintName;
            }
            "EXPLAIN" => {
                // The very next `(` opens the options list. Anything
                // else (a bare `EXPLAIN SELECT …`) just falls through
                // to whatever clause follows.
                scope.expecting_explain_paren = true;
            }
            // VACUUM and ANALYZE can take an option-list paren group
            // (`VACUUM (FULL, VERBOSE) tab`). When the operator types
            // a bare `VACUUM tab` or `ANALYZE tab` (no parens), the
            // flag stays set until next-`(` or end-of-statement — no
            // harm done.
            "VACUUM" | "ANALYZE" => {
                scope.expecting_vacuum_paren = true;
            }
            "DROP" => {
                scope.pending_drop_kind = true;
            }
            // REINDEX has the same `verb <kind> <name>` shape as DROP
            // (REINDEX INDEX i / REINDEX TABLE t / REINDEX SCHEMA s /
            // REINDEX DATABASE d). Reuse the pending-kind machinery.
            "REINDEX" => {
                scope.pending_drop_kind = true;
            }
            // After DROP, the kind keyword tells us what to suggest.
            // We have tables / views / matviews in the cache so those
            // route through DropTarget. INDEX / SEQUENCE etc. would
            // need the cache extending — leave ctx alone (vocab
            // continuations + nothing else) so we don't suggest
            // misleading table names there.
            "CREATE" => {
                scope.pending_create_kind = true;
            }
            "TABLE" if scope.pending_create_kind => {
                // CREATE TABLE <name> ( … — the `(` opens the column-
                // definition list. Reuse pending_table_ref so the next
                // identifier is read as the table name; set the paren
                // gate so the `(` knows what kind of scope to open.
                scope.ctx = TableRef;
                scope.pending_table_ref = true;
                scope.expecting_create_table_paren = true;
                scope.pending_create_kind = false;
            }
            "TABLE" | "VIEW" if scope.pending_drop_kind => {
                scope.ctx = DropTarget(DropKind::Table);
                scope.pending_drop_kind = false;
            }
            "MATERIALIZED" if scope.pending_drop_kind => {
                // `DROP MATERIALIZED VIEW name` — wait for the VIEW
                // token to flip ctx. Keep the pending flag.
            }
            "INDEX" if scope.pending_drop_kind => {
                scope.ctx = DropTarget(DropKind::Index);
                scope.pending_drop_kind = false;
            }
            "SEQUENCE" if scope.pending_drop_kind => {
                scope.ctx = DropTarget(DropKind::Sequence);
                scope.pending_drop_kind = false;
            }
            "CAST" => {
                // The next `(` opens CAST's argument scope. Inside,
                // `AS` flips ctx to TypeName.
                scope.expecting_cast_paren = true;
            }
            "AS" if scope.is_cast_scope => {
                // We're inside CAST's parens; AS introduces the type.
                scope.ctx = TypeName;
            }
            // `SHOW <param>` / `SET <param> = …` — after either, the
            // operator is naming a GUC. The value-side of SET isn't
            // classified specifically; once `=` appears the ctx falls
            // back to Unknown (which yields vocabulary fallbacks).
            "SHOW" | "SET" => scope.ctx = GucParameter,
            // Inside a CREATE TABLE column list, the first identifier
            // we see after `(` or `,` is the column name — the next
            // position is the type. Flip on any non-keyword identifier
            // so `(id ` and `(id INT, name ` both land on TypeName for
            // the position after the name.
            other if matches!(scope.ctx, CreateTableColumns) && is_identifier_like(other) => {
                scope.ctx = CreateTableColumnType;
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

/// A CTE definition captured from a `WITH` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CteDef {
    pub name: String,
    /// Column names the CTE exposes. Sourced from the explicit
    /// `WITH cte(a, b) AS (...)` column list when present, otherwise
    /// inferred from the body's SELECT list. May be empty when neither
    /// is determinable (e.g. `SELECT *` without a known FROM).
    pub columns: Vec<String>,
}

/// Extract CTEs (`WITH cte_name AS (...)`) declared in `buf` in
/// declaration order. Tolerant of partial input — an unterminated CTE
/// body still produces an entry for the name.
///
/// Columns: explicit `WITH cte(a, b) AS (...)` lists win; otherwise
/// pulled from the body's SELECT list via
/// `select_list::extract_select_columns` (no `*` expansion).
/// Use `extract_ctes_resolved` to get `SELECT * FROM …` expanded
/// against a schema cache.
pub fn extract_ctes(buf: &str) -> Vec<CteDef> {
    let tokens = tokenize(buf);
    let mut out: Vec<CteDef> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i].text.eq_ignore_ascii_case("WITH") {
            i += 1;
            if i < tokens.len() && tokens[i].text.eq_ignore_ascii_case("RECURSIVE") {
                i += 1;
            }
            loop {
                if i >= tokens.len() || !is_identifier_like(tokens[i].text) {
                    break;
                }
                let name = tokens[i].text.to_string();
                i += 1;
                // Optional column list `cte(a, b) AS …`.
                let mut explicit_columns: Vec<String> = Vec::new();
                if i < tokens.len() && tokens[i].text == "(" {
                    explicit_columns = read_paren_ident_list(&tokens, i);
                    i = skip_parenthesised(&tokens, i);
                }
                if i >= tokens.len() || !tokens[i].text.eq_ignore_ascii_case("AS") {
                    break;
                }
                i += 1;
                if i < tokens.len() && tokens[i].text.eq_ignore_ascii_case("NOT") {
                    i += 1;
                }
                if i < tokens.len() && tokens[i].text.eq_ignore_ascii_case("MATERIALIZED") {
                    i += 1;
                }
                // Body — extract the raw substring so we can re-run the
                // select-list extractor on it. Token positions aren't
                // tracked, so we slice via a paren-balance walk on the
                // raw bytes of `buf`.
                let body_text = extract_paren_body(buf, &tokens, i);
                let body_columns = if !explicit_columns.is_empty() {
                    explicit_columns
                } else {
                    body_text
                        .map(|body| crate::query::select_list::extract_select_columns(body))
                        .unwrap_or_default()
                };
                out.push(CteDef {
                    name,
                    columns: body_columns,
                });
                if i < tokens.len() && tokens[i].text == "(" {
                    i = skip_parenthesised(&tokens, i);
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

/// Convenience for callers that only care about the names. Wraps
/// `extract_ctes` so we don't break older consumers in one go.
pub fn extract_cte_names(buf: &str) -> Vec<String> {
    extract_ctes(buf).into_iter().map(|c| c.name).collect()
}

/// Like `extract_ctes` but expands `SELECT *` against the schema
/// cache. Use this from the completion engine — `extract_ctes` stays
/// pure for tests / future callers that don't have a cache.
pub fn extract_ctes_resolved(buf: &str, schema: &crate::query::schema::SchemaCache) -> Vec<CteDef> {
    let tokens = tokenize(buf);
    let mut out: Vec<CteDef> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i].text.eq_ignore_ascii_case("WITH") {
            i += 1;
            if i < tokens.len() && tokens[i].text.eq_ignore_ascii_case("RECURSIVE") {
                i += 1;
            }
            loop {
                if i >= tokens.len() || !is_identifier_like(tokens[i].text) {
                    break;
                }
                let name = tokens[i].text.to_string();
                i += 1;
                let mut explicit_columns: Vec<String> = Vec::new();
                if i < tokens.len() && tokens[i].text == "(" {
                    explicit_columns = read_paren_ident_list(&tokens, i);
                    i = skip_parenthesised(&tokens, i);
                }
                if i >= tokens.len() || !tokens[i].text.eq_ignore_ascii_case("AS") {
                    break;
                }
                i += 1;
                if i < tokens.len() && tokens[i].text.eq_ignore_ascii_case("NOT") {
                    i += 1;
                }
                if i < tokens.len() && tokens[i].text.eq_ignore_ascii_case("MATERIALIZED") {
                    i += 1;
                }
                let body_text = extract_paren_body(buf, &tokens, i);
                let body_columns = if !explicit_columns.is_empty() {
                    explicit_columns
                } else {
                    body_text
                        .map(|body| crate::query::select_list::resolve_select_columns(body, schema))
                        .unwrap_or_default()
                };
                out.push(CteDef {
                    name,
                    columns: body_columns,
                });
                if i < tokens.len() && tokens[i].text == "(" {
                    i = skip_parenthesised(&tokens, i);
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

/// Read identifier names from a `( a, b, c )` group starting at
/// `tokens[i] == "("`. Skips anything that isn't a plain identifier.
fn read_paren_ident_list(tokens: &[crate::query::from_parse::Tok<'_>], i: usize) -> Vec<String> {
    let mut out = Vec::new();
    if tokens.get(i).map(|t| t.text) != Some("(") {
        return out;
    }
    let mut depth = 1i32;
    let mut j = i + 1;
    while j < tokens.len() && depth > 0 {
        match tokens[j].text {
            "(" => depth += 1,
            ")" => depth -= 1,
            "," => {}
            t if is_identifier_like(t) && depth == 1 => out.push(t.to_string()),
            _ => {}
        }
        j += 1;
    }
    out
}

/// Slice the body of a parenthesised group from the original `buf`.
/// `tokens[i] == "("` is required; otherwise returns None.
/// Uses a parens-balance walk on the raw bytes (not the tokens),
/// honouring single-quoted strings and SQL comments so an in-string
/// `(` / `)` can't unbalance the count.
fn extract_paren_body<'a>(
    buf: &'a str,
    tokens: &[crate::query::from_parse::Tok<'_>],
    i: usize,
) -> Option<&'a str> {
    if tokens.get(i).map(|t| t.text) != Some("(") {
        return None;
    }
    // Find the byte offset of the i-th `(` token by walking the raw
    // bytes and counting tokenizable-`(`s. Cheap-and-cheerful: we
    // count any `(` not inside a string / comment.
    let mut byte_idx: Option<usize> = None;
    let mut count = 0usize;
    let bytes = buf.as_bytes();
    let target_paren_count = count_lparens_through_token(tokens, i);
    let mut k = 0;
    let mut in_single = false;
    let mut in_double = false;
    while k < bytes.len() {
        let b = bytes[k];
        if in_single {
            if b == b'\'' {
                in_single = false;
            }
            k += 1;
            continue;
        }
        if in_double {
            if b == b'"' {
                in_double = false;
            }
            k += 1;
            continue;
        }
        if b == b'-' && k + 1 < bytes.len() && bytes[k + 1] == b'-' {
            while k < bytes.len() && bytes[k] != b'\n' {
                k += 1;
            }
            continue;
        }
        if b == b'/' && k + 1 < bytes.len() && bytes[k + 1] == b'*' {
            k += 2;
            while k + 1 < bytes.len() && !(bytes[k] == b'*' && bytes[k + 1] == b'/') {
                k += 1;
            }
            if k + 1 < bytes.len() {
                k += 2;
            }
            continue;
        }
        match b {
            b'\'' => in_single = true,
            b'"' => in_double = true,
            b'(' => {
                count += 1;
                if count == target_paren_count {
                    byte_idx = Some(k);
                    break;
                }
            }
            _ => {}
        }
        k += 1;
    }
    let open = byte_idx?;
    // Walk forward from `open + 1` to the matching `)`.
    let mut depth = 1i32;
    let mut j = open + 1;
    let mut in_single = false;
    let mut in_double = false;
    while j < bytes.len() && depth > 0 {
        let b = bytes[j];
        if in_single {
            if b == b'\'' {
                in_single = false;
            }
            j += 1;
            continue;
        }
        if in_double {
            if b == b'"' {
                in_double = false;
            }
            j += 1;
            continue;
        }
        if b == b'-' && j + 1 < bytes.len() && bytes[j + 1] == b'-' {
            while j < bytes.len() && bytes[j] != b'\n' {
                j += 1;
            }
            continue;
        }
        if b == b'/' && j + 1 < bytes.len() && bytes[j + 1] == b'*' {
            j += 2;
            while j + 1 < bytes.len() && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
                j += 1;
            }
            if j + 1 < bytes.len() {
                j += 2;
            }
            continue;
        }
        match b {
            b'\'' => in_single = true,
            b'"' => in_double = true,
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        j += 1;
    }
    // Body is `(...)` exclusive of the outer parens — return inner.
    let inner_start = open + 1;
    let inner_end = if depth == 0 { j - 1 } else { j };
    if !buf.is_char_boundary(inner_start) || !buf.is_char_boundary(inner_end) {
        return None;
    }
    Some(&buf[inner_start..inner_end])
}

/// How many `(` tokens (as the tokenizer sees them) appear at-or-before
/// index `i`. Used by `extract_paren_body` to translate a token index
/// into a byte index in the original buffer.
fn count_lparens_through_token(tokens: &[crate::query::from_parse::Tok<'_>], i: usize) -> usize {
    tokens.iter().take(i + 1).filter(|t| t.text == "(").count()
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

/// Heuristic: did `INSERT INTO` or `COPY` appear earlier in this token
/// stream? Both shape `<verb> <table> (col_list)` the same way; the
/// `(` after the table opens a column list. `UPDATE foo (...)` is
/// unusual (function-shape, not a column list) so it stays out.
fn in_insert_path(tokens: &[crate::query::from_parse::Tok<'_>], up_to: usize) -> bool {
    tokens[..up_to]
        .iter()
        .any(|t| t.text.eq_ignore_ascii_case("INSERT") || t.text.eq_ignore_ascii_case("COPY"))
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
        assert_eq!(classify("SELECT * FROM u WHERE "), ClauseContext::Predicate);
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
        assert_eq!(classify("SELECT * FROM u; "), ClauseContext::StatementStart);
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
        assert_eq!(classify("INSERT INTO foo VALUES ("), ClauseContext::Values);
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
        assert_eq!(
            c.write_target.as_ref().map(|t| &t.name),
            Some(&"foo".to_string())
        );
    }

    #[test]
    fn write_target_carried_through_delete_where() {
        let c = classify_at("DELETE FROM foo WHERE ", 22);
        assert_eq!(c.ctx, ClauseContext::Predicate);
        assert_eq!(
            c.write_target.as_ref().map(|t| &t.name),
            Some(&"foo".to_string())
        );
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
    fn extract_ctes_pulls_columns_from_body_select() {
        let got = extract_ctes(
            "WITH active_users AS (SELECT id, email FROM users WHERE active) \
             SELECT * FROM active_users",
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "active_users");
        assert_eq!(got[0].columns, vec!["id", "email"]);
    }

    #[test]
    fn extract_ctes_explicit_column_list_wins() {
        // The column list on the CTE-name side overrides body inference.
        let got = extract_ctes("WITH foo (a, b) AS (SELECT 1, 2) SELECT * FROM foo");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].columns, vec!["a", "b"]);
    }

    #[test]
    fn extract_ctes_multiple_ctes_each_with_columns() {
        let got = extract_ctes(
            "WITH foo AS (SELECT a, b FROM x), \
             bar (one, two) AS (SELECT 1, 2) \
             SELECT * FROM bar JOIN foo ON true",
        );
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "foo");
        assert_eq!(got[0].columns, vec!["a", "b"]);
        assert_eq!(got[1].name, "bar");
        assert_eq!(got[1].columns, vec!["one", "two"]);
    }

    #[test]
    fn extract_ctes_handles_unnameable_body_gracefully() {
        // SELECT * → no inferred names; CTE still appears but with
        // empty columns. Completion will fall back to "no candidates"
        // for `cte.|` rather than guess wrong.
        let got = extract_ctes("WITH foo AS (SELECT * FROM users) SELECT * FROM foo");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].columns, Vec::<String>::new());
    }

    #[test]
    fn drop_table_enters_drop_target() {
        assert_eq!(
            classify("DROP TABLE "),
            ClauseContext::DropTarget(DropKind::Table)
        );
        assert_eq!(
            classify("DROP TABLE us"),
            ClauseContext::DropTarget(DropKind::Table)
        );
    }

    #[test]
    fn drop_view_enters_drop_target() {
        assert_eq!(
            classify("DROP VIEW "),
            ClauseContext::DropTarget(DropKind::Table)
        );
    }

    #[test]
    fn drop_materialized_view_enters_drop_target() {
        assert_eq!(
            classify("DROP MATERIALIZED VIEW "),
            ClauseContext::DropTarget(DropKind::Table)
        );
    }

    #[test]
    fn reindex_index_enters_drop_target_index() {
        assert_eq!(
            classify("REINDEX INDEX "),
            ClauseContext::DropTarget(DropKind::Index)
        );
    }

    #[test]
    fn reindex_table_enters_drop_target_table() {
        assert_eq!(
            classify("REINDEX TABLE "),
            ClauseContext::DropTarget(DropKind::Table)
        );
    }

    #[test]
    fn drop_index_enters_drop_target_index() {
        assert_eq!(
            classify("DROP INDEX "),
            ClauseContext::DropTarget(DropKind::Index)
        );
    }

    #[test]
    fn drop_sequence_enters_drop_target_sequence() {
        assert_eq!(
            classify("DROP SEQUENCE "),
            ClauseContext::DropTarget(DropKind::Sequence)
        );
    }

    #[test]
    fn drop_without_kind_keyword_leaves_ctx_alone() {
        // `DROP ` (no kind) — bare DROP shouldn't presume.
        // pending_drop_kind set, but no TABLE/VIEW follows; ctx stays
        // at the current value (StatementStart in this case).
        assert_eq!(classify("DROP "), ClauseContext::StatementStart);
    }

    #[test]
    fn constraint_keyword_inside_on_conflict_enters_constraint_name() {
        let c = classify_at(
            "INSERT INTO foo (a) VALUES (1) ON CONFLICT ON CONSTRAINT ",
            56,
        );
        assert_eq!(c.ctx, ClauseContext::ConstraintName);
    }

    #[test]
    fn constraint_keyword_outside_on_conflict_does_nothing() {
        // CONSTRAINT outside an ON CONFLICT (e.g. inside CREATE TABLE)
        // shouldn't flip to ConstraintName.
        let c = classify_at("CREATE TABLE t (id INT CONSTRAINT ", 32);
        assert_ne!(c.ctx, ClauseContext::ConstraintName);
    }

    #[test]
    fn create_table_paren_starts_at_column_name_position() {
        // Right after `(` — operator is naming a fresh column; no
        // completion.
        assert_eq!(
            classify("CREATE TABLE t ("),
            ClauseContext::CreateTableColumns
        );
    }

    #[test]
    fn create_table_after_column_name_flips_to_type_position() {
        // After `(id ` (column name typed, space) we're at the type
        // position. Wants TYPE_NAMES.
        assert_eq!(
            classify("CREATE TABLE t (id "),
            ClauseContext::CreateTableColumnType
        );
    }

    #[test]
    fn create_table_comma_returns_to_column_name_position() {
        // `(id INT, ` — after the comma we're naming the next column.
        assert_eq!(
            classify("CREATE TABLE t (id INT, "),
            ClauseContext::CreateTableColumns
        );
    }

    #[test]
    fn create_table_second_column_type_position() {
        // `(id INT, name ` — second column's type slot.
        assert_eq!(
            classify("CREATE TABLE t (id INT, name "),
            ClauseContext::CreateTableColumnType
        );
    }

    #[test]
    fn create_table_as_select_subquery_does_not_steal_create_paren() {
        // `CREATE TABLE t AS SELECT * FROM (SELECT * FROM x WHERE |` —
        // the inner `(` opens a subquery, not a CREATE TABLE column
        // list. The create-paren expectation must be cleared once we
        // pass AS / SELECT.
        let buf = "CREATE TABLE t AS SELECT * FROM (SELECT * FROM x WHERE ";
        assert_eq!(classify(buf), ClauseContext::Predicate);
    }

    #[test]
    fn create_table_as_select_open_paren_is_subquery_not_columns() {
        // Cursor right at the inner `(` — without the deferred-clear
        // fix, this would land on `CreateTableColumns` (the stale
        // create-paren expectation gets consumed by the subquery's
        // `(`). The classifier must clear the expectation once we've
        // moved past the table name into other clauses.
        let buf = "CREATE TABLE t AS SELECT * FROM (";
        assert_ne!(classify(buf), ClauseContext::CreateTableColumns);
    }

    #[test]
    fn create_table_temp_keyword_does_not_block_classification() {
        // `CREATE TEMP TABLE t (id ` still routes through the create-
        // table path (TEMP / UNLOGGED / IF NOT EXISTS sit between
        // CREATE and TABLE).
        assert_eq!(
            classify("CREATE TEMP TABLE t (id "),
            ClauseContext::CreateTableColumnType
        );
    }

    #[test]
    fn on_inside_on_conflict_does_not_trigger_predicate() {
        // `INSERT INTO foo (a) VALUES (1) ON CONFLICT ON CONSTRAINT |`
        // — the second ON belongs to "ON CONSTRAINT", not a Predicate
        // introducer. ctx must not flip to Predicate.
        let c = classify_at(
            "INSERT INTO foo (a) VALUES (1) ON CONFLICT ON CONSTRAINT ",
            55,
        );
        assert_ne!(c.ctx, ClauseContext::Predicate);
    }

    #[test]
    fn on_conflict_paren_enters_insert_columns_of_target() {
        let ctx = classify("INSERT INTO foo (a) VALUES (1) ON CONFLICT (");
        match ctx {
            ClauseContext::InsertColumns(t) => assert_eq!(t.name, "foo"),
            other => panic!("expected InsertColumns(foo), got {other:?}"),
        }
    }

    #[test]
    fn on_conflict_do_update_set_enters_update_assign_of_target() {
        let ctx = classify("INSERT INTO foo (a) VALUES (1) ON CONFLICT (id) DO UPDATE SET ");
        match ctx {
            ClauseContext::UpdateAssign(t) => assert_eq!(t.name, "foo"),
            other => panic!("expected UpdateAssign(foo), got {other:?}"),
        }
    }

    #[test]
    fn cast_as_enters_type_name() {
        assert_eq!(classify("SELECT CAST(x AS "), ClauseContext::TypeName);
        assert_eq!(classify("SELECT CAST(x AS in"), ClauseContext::TypeName);
    }

    #[test]
    fn cast_as_does_not_leak_to_outer_after_close() {
        // After CAST(x AS integer), the cursor's context goes back to
        // SelectList (the outer SELECT).
        assert_eq!(
            classify("SELECT CAST(x AS integer), em"),
            ClauseContext::SelectList
        );
    }

    #[test]
    fn regular_as_outside_cast_does_not_enter_type_name() {
        // `SELECT col AS alias` — AS is alias-introducing, not type.
        // SelectList stays.
        assert_eq!(classify("SELECT col AS "), ClauseContext::SelectList);
    }

    #[test]
    fn explain_paren_enters_explain_options() {
        assert_eq!(classify("EXPLAIN ("), ClauseContext::ExplainOptions);
        assert_eq!(classify("EXPLAIN (AN"), ClauseContext::ExplainOptions);
    }

    #[test]
    fn truncate_enters_table_ref() {
        assert_eq!(classify("TRUNCATE "), ClauseContext::TableRef);
        assert_eq!(classify("TRUNCATE us"), ClauseContext::TableRef);
    }

    #[test]
    fn copy_table_then_paren_enters_insert_columns() {
        let ctx = classify("COPY users (");
        match ctx {
            ClauseContext::InsertColumns(t) => assert_eq!(t.name, "users"),
            other => panic!("expected InsertColumns(users), got {other:?}"),
        }
    }

    #[test]
    fn copy_table_without_paren_stays_table_ref() {
        // `COPY users FROM '/tmp/u.csv'` — no column list.
        assert_eq!(
            classify("COPY users FROM '/tmp/u.csv'"),
            // Once `FROM` fires, ctx moves to TableRef (FROM rebinds);
            // the operator is now choosing where the data lives, which
            // is technically a file path. We don't classify file paths,
            // but the surrounding behaviour mustn't regress.
            ClauseContext::TableRef
        );
    }

    #[test]
    fn equals_after_set_param_enters_guc_value() {
        assert_eq!(classify("SET timezone = "), ClauseContext::GucValue);
        assert_eq!(classify("SET timezone = on"), ClauseContext::GucValue);
    }

    #[test]
    fn show_enters_guc_parameter() {
        assert_eq!(classify("SHOW "), ClauseContext::GucParameter);
        assert_eq!(classify("SHOW sea"), ClauseContext::GucParameter);
    }

    #[test]
    fn set_enters_guc_parameter_until_equals() {
        assert_eq!(classify("SET "), ClauseContext::GucParameter);
        assert_eq!(classify("SET time"), ClauseContext::GucParameter);
    }

    #[test]
    fn vacuum_paren_enters_vacuum_options() {
        assert_eq!(classify("VACUUM ("), ClauseContext::VacuumOptions);
        assert_eq!(classify("VACUUM (FU"), ClauseContext::VacuumOptions);
    }

    #[test]
    fn analyze_paren_enters_vacuum_options_too() {
        // ANALYZE (...) reuses the same options syntax as VACUUM.
        assert_eq!(classify("ANALYZE ("), ClauseContext::VacuumOptions);
    }

    #[test]
    fn vacuum_without_paren_stays_at_statement_start() {
        // `VACUUM tab` (bare form) — no parens, falls through.
        assert_eq!(classify("VACUUM "), ClauseContext::StatementStart);
    }

    #[test]
    fn explain_without_paren_does_not_enter_explain_options() {
        // `EXPLAIN SELECT …` — no parens, the next clause is what
        // matters. SELECT-list should fire normally.
        assert_eq!(classify("EXPLAIN SELECT "), ClauseContext::SelectList);
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
