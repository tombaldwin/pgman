//! SQL identifier completion.
//!
//! Two pure ops:
//!
//! - [`extract_identifier`] finds the partial identifier the user is
//!   typing under the cursor — including a `qualifier.` prefix if there
//!   is one. Returns enough info that the renderer can replace the
//!   prefix without disturbing surrounding text.
//! - [`candidates_for`] turns a cursor position + the schema cache into
//!   an ordered list of [`Candidate`]s. FROM-aware: when the buffer
//!   contains `FROM users u`, typing `u.|` only offers columns of `users`.
//!   When the qualifier doesn't match an alias / table, falls back to
//!   "anything starting with `prefix`".
//!
//! The Tab handler in `app.rs` is the only consumer; everything in this
//! module is pure and unit-tested.

use crate::query::clause::{
    classify_at, extract_ctes_resolved, ClauseContext, CteDef, DropKind, QualifiedTable,
};
use crate::query::from_parse::{parse_from_tables_resolved, TableRefInQuery};
use crate::query::schema::SchemaCache;
use crate::query::vocabulary::{
    continuations, AGGREGATE_FUNCTIONS, DROP_CONTINUATIONS, EXPLAIN_OPTIONS, GUC_PARAMETERS,
    GUC_VALUES, JOIN_VARIANTS, PREDICATE_OPERATORS, SCALAR_FUNCTIONS, STATEMENT_KEYWORDS,
    TYPE_NAMES, WINDOW_FUNCTIONS,
};

/// The partial identifier the cursor is inside (or immediately after).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identifier {
    /// Byte offset where the identifier begins (start of the first
    /// segment, whether that's `schema`, `qualifier`, or `prefix`).
    pub start: usize,
    /// Byte offset where the identifier ENDS — walks forward from the
    /// cursor over any trailing identifier characters so Tab inside an
    /// existing word (`SELECT user|_id`) replaces the WHOLE word, not
    /// just the prefix-up-to-cursor (which would leave `_id` glued on).
    pub end: usize,
    /// The schema segment when the user typed a 3-part name
    /// (`schema.table.col`). `None` for 1- or 2-part names.
    pub schema: Option<String>,
    /// The middle / table-or-alias segment. For `audit.users.email` this
    /// is `users`. For `u.email` it's `u`. `None` for a bare identifier.
    pub qualifier: Option<String>,
    /// The right-most segment under the cursor. May be empty (cursor
    /// immediately after a `.` or in fresh space).
    pub prefix: String,
}

/// What kind of thing a `Candidate` points to. Drives the small hint
/// shown in the footer alongside the cycle index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CandidateKind {
    Schema,
    Table,
    Column,
    Alias,
    Keyword,
    /// SQL aggregate / window functions (`COUNT`, `SUM`, `AVG`, etc).
    /// Surfaced in SELECT-list / RETURNING contexts where they're
    /// usefully called inline.
    Function,
}

impl CandidateKind {
    pub fn label(self) -> &'static str {
        match self {
            CandidateKind::Schema => "schema",
            CandidateKind::Table => "table",
            CandidateKind::Column => "column",
            CandidateKind::Alias => "alias",
            CandidateKind::Keyword => "keyword",
            CandidateKind::Function => "fn",
        }
    }
}

// Vocabulary lives in `query::vocabulary` so adding a function or
// operator is a one-line change in one file. Each list below maps to
// a specific clause context — see `candidates_for_in_context`.

/// One completion the user can land on. `insert` is the text to substitute
/// for `prefix`; `display` is what the picker (footer hint) shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub display: String,
    pub insert: String,
    pub kind: CandidateKind,
}

/// Walk back from `cursor` over identifier-ish characters (letters,
/// digits, `_`) plus at most one trailing `.` to find the partial
/// identifier under the cursor. Returns `None` when the cursor isn't
/// inside / immediately after an identifier.
pub fn extract_identifier(buf: &str, cursor: usize) -> Option<Identifier> {
    // Clamp + snap to char boundary so byte-cursor arithmetic is safe.
    let cursor = cursor.min(buf.len());
    if !buf.is_char_boundary(cursor) {
        return None;
    }
    let bytes = buf.as_bytes();
    let mut start = cursor;
    while start > 0 {
        let prev = start - 1;
        let c = bytes[prev];
        if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' {
            start = prev;
        } else {
            break;
        }
    }
    // Walk forward over identifier chars (no dots — we only honour the
    // qualifier the user already typed) so `SELECT user|_id` + Tab
    // replaces the whole `user_id`, not just `user`.
    let mut end = cursor;
    while end < bytes.len() {
        let c = bytes[end];
        if c.is_ascii_alphanumeric() || c == b'_' {
            end += 1;
        } else {
            break;
        }
    }

    if start == cursor {
        // Nothing identifier-shaped immediately behind the cursor.
        // Still useful: return an empty identifier so completion can
        // start fresh from this position. We require the *previous*
        // non-identifier char to have been a `.` or whitespace before
        // emitting; otherwise punctuation like `;` shouldn't trigger.
        let prev = if cursor == 0 {
            None
        } else {
            Some(bytes[cursor - 1])
        };
        match prev {
            None => {
                return Some(Identifier {
                    start: cursor,
                    end,
                    schema: None,
                    qualifier: None,
                    prefix: String::new(),
                });
            }
            Some(c) if c.is_ascii_whitespace() => {
                return Some(Identifier {
                    start: cursor,
                    end,
                    schema: None,
                    qualifier: None,
                    prefix: String::new(),
                });
            }
            _ => return None,
        }
    }
    // Numeric literals look identifier-shaped (digits + dot) — reject
    // them so `SELECT price * 1.5` doesn't get mis-parsed as `1.5`
    // having qualifier="1", prefix="5".
    if let Some(first) = buf[start..].chars().next() {
        if first.is_ascii_digit() {
            return None;
        }
    }
    let raw = &buf[start..cursor];
    // Split into at most 3 segments by `.` — schema.table.col.
    // Anything beyond 3 collapses extra segments into the qualifier so
    // `a.b.c.d` reads as qualifier=`b.c`, prefix=`d` (unusual but at
    // least non-lossy).
    let segments: Vec<&str> = raw.split('.').collect();
    let (schema, qualifier, prefix) = match segments.as_slice() {
        [] | [_] => (None, None, raw.to_string()),
        [q, p] => (None, Some(q.to_string()), p.to_string()),
        [s, q, p] => (Some(s.to_string()), Some(q.to_string()), p.to_string()),
        all => {
            // Keep `schema = first`, prefix = last, qualifier =
            // everything in the middle joined by `.` (an unusual
            // shape, but preserves what the operator typed).
            let last = all.last().unwrap();
            let first = all[0];
            let middle = all[1..all.len() - 1].join(".");
            (
                Some(first.to_string()),
                Some(middle),
                last.to_string(),
            )
        }
    };
    Some(Identifier {
        start,
        end,
        schema,
        qualifier,
        prefix,
    })
}

/// Compute the ordered candidate list for the cursor position. The Tab
/// handler should drop the cycle and call this fresh on every new Tab
/// press that starts (rather than continues) a cycle.
///
/// Ordering rules (deterministic, so cycling is stable):
/// 1. Exact prefix matches (case-insensitive) before fuzzy / contains.
/// 2. Within a tier, sort lexicographically.
/// 3. When unqualified and a `FROM` is in scope: columns of FROM tables
///    first, then aliases, then FROM-table names, then anything else.
pub fn candidates_for(
    buf: &str,
    cursor: usize,
    schema: &SchemaCache,
) -> Vec<Candidate> {
    let Some(id) = extract_identifier(buf, cursor) else {
        return Vec::new();
    };
    // FROM-clause scope = everything before the partial PLUS everything
    // after the cursor. The text *inside* `[id.start, cursor)` is the
    // partial itself; if we passed the full buffer to the FROM parser it
    // would mis-classify the partial as a phantom table (so `FROM us|`
    // would conjure a "us" table in scope and crowd out the real
    // "users" hit). Splitting around the cursor keeps both the
    // `SELECT u.| FROM users u` case (FROM is *after* the cursor) and
    // the `SELECT u FROM users u WHERE u.|` case working.
    let cursor = cursor.min(buf.len());
    let before = parse_from_tables_resolved(&buf[..id.start], schema);
    let after = if cursor < buf.len() {
        parse_from_tables_resolved(&buf[cursor..], schema)
    } else {
        Vec::new()
    };
    let mut in_scope = before;
    in_scope.extend(after);

    let classification = classify_at(buf, cursor);
    // For write statements (UPDATE / INSERT / DELETE), the target
    // table isn't surfaced by the FROM parser — fold it into in_scope
    // so a WHERE / SET / RETURNING clause inside one of those statements
    // still gets the target's columns.
    if let Some(t) = &classification.write_target {
        in_scope.push(TableRefInQuery {
            schema: t.schema.clone(),
            name: t.name.clone(),
            alias: None,
            virtual_columns: None,
        });
        // `EXCLUDED` is the Postgres virtual reference inside an
        // `ON CONFLICT DO UPDATE SET col = EXCLUDED.col` clause — it
        // exposes the would-be-inserted row's columns, which match
        // the target table's columns one-for-one. Surface it so the
        // qualified path `EXCLUDED.|` autocompletes.
        if let Some(cols) = schema
            .columns_for(t.schema.as_deref(), &t.name)
            .cloned()
        {
            in_scope.push(TableRefInQuery {
                schema: None,
                name: "EXCLUDED".into(),
                alias: Some("EXCLUDED".into()),
                virtual_columns: Some(cols),
            });
        }
    }

    let ctes = extract_ctes_resolved(buf, schema);
    // The current statement's SELECT-list output names. Used by
    // HavingPredicate completion since Postgres lets HAVING reference
    // SELECT-list aliases that WHERE can't see. Pulled from the
    // statement that contains the cursor (split on `;`).
    let stmt = current_statement(buf, cursor);
    let select_aliases = crate::query::select_list::resolve_select_columns(stmt, schema);

    candidates_for_in_context(
        &id,
        &classification.ctx,
        &in_scope,
        &ctes,
        &select_aliases,
        schema,
    )
}

/// Slice of the buffer holding the current statement (everything since
/// the last unquoted `;` up through `cursor` and on to the next `;`).
/// Used to pin SELECT-list extraction to the right statement when the
/// buffer contains multiple `;`-separated commands.
fn current_statement(buf: &str, cursor: usize) -> &str {
    let cursor = cursor.min(buf.len());
    let bytes = buf.as_bytes();
    // Walk back from cursor for the previous `;` (outside strings —
    // approximated; see clause::statement_start for the careful
    // version). For SELECT-list extraction the approximation is fine.
    let mut start = 0usize;
    let mut i = 0usize;
    let mut in_str = false;
    while i < cursor {
        let b = bytes[i];
        if b == b'\'' {
            in_str = !in_str;
        }
        if !in_str && b == b';' {
            start = i + 1;
        }
        i += 1;
    }
    // Walk forward from cursor for the next `;`.
    let mut end = bytes.len();
    let mut j = cursor;
    let mut in_str = false;
    while j < bytes.len() {
        let b = bytes[j];
        if b == b'\'' {
            in_str = !in_str;
        }
        if !in_str && b == b';' {
            end = j;
            break;
        }
        j += 1;
    }
    if !buf.is_char_boundary(start) || !buf.is_char_boundary(end) {
        return "";
    }
    &buf[start..end]
}

/// Branch on clause context to keep candidates relevant to what the
/// operator is actually typing. Each arm has a tight, testable behaviour:
/// see `tests::candidates_for_*_context` for the locked-in cases.
fn candidates_for_in_context(
    id: &Identifier,
    ctx: &ClauseContext,
    in_scope: &[TableRefInQuery],
    ctes: &[CteDef],
    select_aliases: &[String],
    schema: &SchemaCache,
) -> Vec<Candidate> {
    // 3-segment `schema.table.col|` — always resolves to columns of
    // the exact named table in the named schema, regardless of clause
    // context. Returns empty when the schema/table pair isn't in the
    // cache (a typo gets dead silence rather than wrong columns).
    if let (Some(s), Some(t)) = (&id.schema, &id.qualifier) {
        let cols = schema
            .columns_for(Some(s), t)
            .cloned()
            .unwrap_or_default();
        return matches_for(&cols, &id.prefix, CandidateKind::Column);
    }
    // 2-segment `cte.col|` or `sub.col|` — resolve the qualifier
    // against CTE / subquery virtual columns BEFORE falling through to
    // catalog-table / alias logic.
    if id.schema.is_none() {
        if let Some(q) = id.qualifier.as_deref() {
            if let Some(cte) = ctes.iter().find(|c| c.name.eq_ignore_ascii_case(q)) {
                return matches_for(&cte.columns, &id.prefix, CandidateKind::Column);
            }
            for t in in_scope {
                if t.match_key() == q.to_ascii_lowercase() {
                    if let Some(cols) = &t.virtual_columns {
                        return matches_for(cols, &id.prefix, CandidateKind::Column);
                    }
                }
            }
        }
    }
    match ctx {
        // Inside a `VALUES (...)` literal list — operator is typing
        // constants, no identifier completion. After the closing `)`
        // the scope pops and continuations (RETURNING, ON CONFLICT)
        // can fire from the outer scope; here we offer them only if
        // the prefix matches, so a mid-literal Tab is still silent.
        ClauseContext::Values => {
            candidates_from_list(&id.prefix, continuations::AFTER_VALUES)
        }

        // INSERT INTO foo (|  → columns of `foo` (specifically).
        ClauseContext::InsertColumns(t) => columns_of(t, &id.prefix, schema),

        // EXPLAIN (|  → the documented options.
        ClauseContext::ExplainOptions => candidates_from_list(&id.prefix, EXPLAIN_OPTIONS),

        // SHOW | / SET |  → GUC parameter names.
        ClauseContext::GucParameter => GUC_PARAMETERS
            .iter()
            .filter(|p| starts_with_ci(p, &id.prefix))
            .map(|p| Candidate {
                display: (*p).to_string(),
                insert: (*p).to_string(),
                kind: CandidateKind::Keyword,
            })
            .collect(),

        // SET <param> = |  → on / off / true / false / default.
        ClauseContext::GucValue => GUC_VALUES
            .iter()
            .filter(|v| starts_with_ci(v, &id.prefix))
            .map(|v| Candidate {
                display: (*v).to_string(),
                insert: (*v).to_string(),
                kind: CandidateKind::Keyword,
            })
            .collect(),

        // CAST(expr AS |  → SQL type names. Multi-word types like
        // `timestamp with time zone` land as one Tab.
        ClauseContext::TypeName => TYPE_NAMES
            .iter()
            .filter(|t| starts_with_ci(t, &id.prefix))
            .map(|t| Candidate {
                display: (*t).to_string(),
                insert: (*t).to_string(),
                kind: CandidateKind::Keyword,
            })
            .collect(),

        // DROP <kind> |  → catalog set selected by `kind`, plus
        // DROP-specific continuations. NOT JOIN variants / WHERE etc.
        ClauseContext::DropTarget(kind) => {
            let names: &[crate::query::schema::TableMeta] = match kind {
                DropKind::Table => &schema.tables,
                DropKind::Index => &schema.indexes,
                DropKind::Sequence => &schema.sequences,
            };
            match id.qualifier.as_deref() {
                Some(q) => {
                    // schema-qualified — filter to that schema.
                    if !schema.schemas.iter().any(|s| s.eq_ignore_ascii_case(q)) {
                        return Vec::new();
                    }
                    let mut hits: Vec<String> = names
                        .iter()
                        .filter(|t| t.schema.eq_ignore_ascii_case(q))
                        .map(|t| t.name.clone())
                        .collect();
                    hits.sort();
                    hits.dedup();
                    hits.into_iter()
                        .filter(|n| starts_with_ci(n, &id.prefix))
                        .map(|n| Candidate {
                            display: n.clone(),
                            insert: n,
                            kind: CandidateKind::Table,
                        })
                        .collect()
                }
                None => {
                    let mut out: Vec<Candidate> = Vec::new();
                    let mut seen = std::collections::BTreeSet::new();
                    for t in names {
                        if starts_with_ci(&t.name, &id.prefix)
                            && seen.insert(t.name.clone())
                        {
                            out.push(Candidate {
                                display: t.name.clone(),
                                insert: t.name.clone(),
                                kind: CandidateKind::Table,
                            });
                        }
                    }
                    // Schemas are useful regardless of kind (operator
                    // may want to qualify).
                    for s in &schema.schemas {
                        if starts_with_ci(s, &id.prefix) {
                            out.push(Candidate {
                                display: s.clone(),
                                insert: s.clone(),
                                kind: CandidateKind::Schema,
                            });
                        }
                    }
                    out.extend(candidates_from_list(&id.prefix, DROP_CONTINUATIONS));
                    out
                }
            }
        }

        // UPDATE foo SET |  → columns of `foo`, plus continuations
        // (WHERE, RETURNING) once the operator's finished the
        // assignment list.
        ClauseContext::UpdateAssign(t) => {
            let mut out = columns_of(t, &id.prefix, schema);
            out.extend(candidates_from_list(&id.prefix, continuations::AFTER_UPDATE_ASSIGN));
            out
        }

        // Top of statement — operator is typing a verb. Offer SQL
        // keywords; the unusual `schema.|` case at statement start
        // still routes through the qualified path so it's not dead.
        ClauseContext::StatementStart => match id.qualifier.as_deref() {
            Some(q) => candidates_for_qualified(q, &id.prefix, in_scope, schema),
            None => candidates_keywords(&id.prefix),
        },

        // Table-reference position (FROM / JOIN / INSERT INTO target /
        // UPDATE target / DELETE FROM). Tables + schemas only — columns
        // are nonsensical here. CTE names declared earlier in the
        // buffer (`WITH cte AS (...)`) are surfaced as Table candidates
        // so `FROM cte` autocompletes. JOIN variants + continuation
        // keywords (WHERE, GROUP BY, …) come after the identifier
        // candidates so the cycle prioritises identifiers.
        ClauseContext::TableRef => match id.qualifier.as_deref() {
            Some(q) => candidates_tables_in_schema(q, &id.prefix, schema),
            None => {
                let mut out: Vec<Candidate> = ctes
                    .iter()
                    .filter(|c| starts_with_ci(&c.name, &id.prefix))
                    .map(|c| Candidate {
                        display: c.name.clone(),
                        insert: c.name.clone(),
                        kind: CandidateKind::Table,
                    })
                    .collect();
                out.extend(candidates_tables_and_schemas(&id.prefix, schema));
                out.extend(candidates_from_list(&id.prefix, JOIN_VARIANTS));
                out.extend(candidates_from_list(&id.prefix, continuations::AFTER_TABLE_REF));
                out
            }
        },

        // Predicate / SELECT list / ORDER BY / GROUP BY / HAVING / RETURNING
        // — columns of in-scope tables. Qualified path still allowed
        // for `alias.col` form. SELECT-list and RETURNING also surface
        // SQL aggregates so the operator can autocomplete `COUNT(`.
        ClauseContext::SelectList => match id.qualifier.as_deref() {
            Some(q) => candidates_for_qualified(q, &id.prefix, in_scope, schema),
            None => {
                let mut out = candidates_columns_only(&id.prefix, in_scope, schema);
                out.extend(candidates_functions(&id.prefix));
                out.extend(candidates_from_list(&id.prefix, continuations::AFTER_SELECT_LIST));
                out
            }
        },
        ClauseContext::Predicate => match id.qualifier.as_deref() {
            Some(q) => candidates_for_qualified(q, &id.prefix, in_scope, schema),
            None => {
                let mut out = candidates_columns_only(&id.prefix, in_scope, schema);
                // Word-shaped operators (LIKE, IN, IS NULL, …) come
                // after the column candidates so the cycle prioritises
                // identifiers; clause continuations (GROUP BY, ORDER BY,
                // LIMIT) come after those.
                out.extend(candidates_predicate_operators(&id.prefix));
                out.extend(candidates_from_list(&id.prefix, continuations::AFTER_PREDICATE));
                out
            }
        },
        // HAVING behaves like Predicate but also offers SELECT-list
        // output aliases — Postgres uniquely allows
        // `SELECT COUNT(*) AS n … HAVING n > 1`. The aliases are
        // pre-computed at the top level and passed in via
        // `select_aliases`.
        ClauseContext::HavingPredicate => match id.qualifier.as_deref() {
            Some(q) => candidates_for_qualified(q, &id.prefix, in_scope, schema),
            None => {
                let mut out = candidates_columns_only(&id.prefix, in_scope, schema);
                for alias in select_aliases {
                    if starts_with_ci(alias, &id.prefix) {
                        out.push(Candidate {
                            display: alias.clone(),
                            insert: alias.clone(),
                            kind: CandidateKind::Alias,
                        });
                    }
                }
                out.extend(candidates_predicate_operators(&id.prefix));
                out.extend(candidates_from_list(&id.prefix, continuations::AFTER_PREDICATE));
                out
            }
        },
        ClauseContext::OrderOrGroup => match id.qualifier.as_deref() {
            Some(q) => candidates_for_qualified(q, &id.prefix, in_scope, schema),
            None => {
                let mut out = candidates_columns_only(&id.prefix, in_scope, schema);
                out.extend(candidates_from_list(&id.prefix, continuations::AFTER_ORDER_OR_GROUP));
                out
            }
        },

        // Unknown context — fall back to the pre-grammar behaviour so
        // syntax we don't recognise still gets completion (just less
        // targeted).
        ClauseContext::Unknown => match id.qualifier.as_deref() {
            Some(q) => candidates_for_qualified(q, &id.prefix, in_scope, schema),
            None => candidates_for_unqualified(&id.prefix, in_scope, schema),
        },
    }
}

fn columns_of(table: &QualifiedTable, prefix: &str, schema: &SchemaCache) -> Vec<Candidate> {
    let cols = schema
        .columns_for(table.schema.as_deref(), &table.name)
        .cloned()
        .unwrap_or_default();
    matches_for(&cols, prefix, CandidateKind::Column)
}

fn candidates_keywords(prefix: &str) -> Vec<Candidate> {
    STATEMENT_KEYWORDS
        .iter()
        .filter(|kw| starts_with_ci(kw, prefix))
        .map(|kw| Candidate {
            display: (*kw).to_string(),
            insert: (*kw).to_string(),
            kind: CandidateKind::Keyword,
        })
        .collect()
}

/// Function candidates — aggregates + scalar + window functions, all
/// inserted as `NAME(` so the cursor lands inside the parens ready for
/// the operator's first argument. `display` stays as the bare name so
/// the popup row reads cleanly.
fn candidates_functions(prefix: &str) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut seen: std::collections::BTreeSet<&'static str> =
        std::collections::BTreeSet::new();
    for source in [AGGREGATE_FUNCTIONS, SCALAR_FUNCTIONS, WINDOW_FUNCTIONS] {
        for fname in source {
            if seen.insert(fname) && starts_with_ci(fname, prefix) {
                out.push(Candidate {
                    display: (*fname).to_string(),
                    insert: format!("{fname}("),
                    kind: CandidateKind::Function,
                });
            }
        }
    }
    out
}

/// Word-shaped predicate operators (LIKE, IN, IS NULL, …). Symbolic
/// operators (`=`, `>`) are omitted by design — see the vocabulary
/// module for the rationale.
fn candidates_predicate_operators(prefix: &str) -> Vec<Candidate> {
    PREDICATE_OPERATORS
        .iter()
        .filter(|op| starts_with_ci(op, prefix))
        .map(|op| Candidate {
            display: (*op).to_string(),
            insert: (*op).to_string(),
            kind: CandidateKind::Keyword,
        })
        .collect()
}

/// Generic helper — turn a static `&[&str]` of keywords into matching
/// `Candidate`s. Used for the "continuation" lists (what clauses can
/// follow this position) and the JOIN variants. All emitted as
/// `CandidateKind::Keyword` so the popup row reads `(keyword)`.
fn candidates_from_list(prefix: &str, list: &[&str]) -> Vec<Candidate> {
    list.iter()
        .filter(|w| starts_with_ci(w, prefix))
        .map(|w| Candidate {
            display: (*w).to_string(),
            insert: (*w).to_string(),
            kind: CandidateKind::Keyword,
        })
        .collect()
}

fn candidates_tables_and_schemas(prefix: &str, schema: &SchemaCache) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut seen_tables: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    for t in &schema.tables {
        if starts_with_ci(&t.name, prefix) && seen_tables.insert(t.name.clone()) {
            out.push(Candidate {
                display: t.name.clone(),
                insert: t.name.clone(),
                kind: CandidateKind::Table,
            });
        }
    }
    for s in &schema.schemas {
        if starts_with_ci(s, prefix) {
            out.push(Candidate {
                display: s.clone(),
                insert: s.clone(),
                kind: CandidateKind::Schema,
            });
        }
    }
    out
}

fn candidates_tables_in_schema(
    schema_name: &str,
    prefix: &str,
    schema: &SchemaCache,
) -> Vec<Candidate> {
    if !schema
        .schemas
        .iter()
        .any(|s| s.eq_ignore_ascii_case(schema_name))
    {
        return Vec::new();
    }
    let names: Vec<String> = schema
        .tables
        .iter()
        .filter(|t| t.schema.eq_ignore_ascii_case(schema_name))
        .map(|t| t.name.clone())
        .collect();
    matches_for(&names, prefix, CandidateKind::Table)
}

fn candidates_columns_only(
    prefix: &str,
    in_scope: &[TableRefInQuery],
    schema: &SchemaCache,
) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    // In-scope tables first.
    for t in in_scope {
        if let Some(cols) = schema.columns_for(t.schema.as_deref(), &t.name) {
            for c in cols {
                if starts_with_ci(c, prefix) && seen.insert(c.clone()) {
                    out.push(Candidate {
                        display: c.clone(),
                        insert: c.clone(),
                        kind: CandidateKind::Column,
                    });
                }
            }
        }
    }
    // Aliases bound by the FROM clause — useful for `SELECT u, e FROM users u, events e`
    // (typing the alias).
    for t in in_scope {
        if let Some(alias) = &t.alias {
            if starts_with_ci(alias, prefix) && seen.insert(alias.clone()) {
                out.push(Candidate {
                    display: alias.clone(),
                    insert: alias.clone(),
                    kind: CandidateKind::Alias,
                });
            }
        }
    }
    // If there's no FROM at all (in_scope empty), offer TABLES so the
    // operator can pick a target and add the FROM clause. Falling back
    // to "every column in the cache" was misleading: it inserted a
    // column name without the surrounding FROM table, often producing
    // nonsense like `SELECT email` against an unrelated `users` cache.
    if in_scope.is_empty() {
        for t in &schema.tables {
            if starts_with_ci(&t.name, prefix) && seen.insert(t.name.clone()) {
                out.push(Candidate {
                    display: t.name.clone(),
                    insert: t.name.clone(),
                    kind: CandidateKind::Table,
                });
            }
        }
    }
    out
}

fn candidates_for_qualified(
    qualifier: &str,
    prefix: &str,
    in_scope: &[TableRefInQuery],
    schema: &SchemaCache,
) -> Vec<Candidate> {
    let q_lower = qualifier.to_ascii_lowercase();

    // 1) Match against an alias / table in the FROM scope (pass-2 logic).
    for table in in_scope {
        if table.match_key() == q_lower {
            // We know which table this is — list its columns.
            let cols = schema
                .columns_for(table.schema.as_deref(), &table.name)
                .cloned()
                .unwrap_or_default();
            return matches_for(&cols, prefix, CandidateKind::Column);
        }
    }

    // 2) qualifier might be a schema name → offer its tables.
    if schema
        .schemas
        .iter()
        .any(|s| s.eq_ignore_ascii_case(qualifier))
    {
        let names: Vec<String> = schema
            .tables
            .iter()
            .filter(|t| t.schema.eq_ignore_ascii_case(qualifier))
            .map(|t| t.name.clone())
            .collect();
        return matches_for(&names, prefix, CandidateKind::Table);
    }

    // 3) qualifier might be a table name with no FROM scope yet (e.g.
    //    `SELECT users.|`). Offer its columns.
    if let Some(cols) = schema.columns_for(None, qualifier) {
        return matches_for(cols, prefix, CandidateKind::Column);
    }

    Vec::new()
}

fn candidates_for_unqualified(
    prefix: &str,
    in_scope: &[TableRefInQuery],
    schema: &SchemaCache,
) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut seen: std::collections::BTreeSet<(CandidateKind, String)> =
        std::collections::BTreeSet::new();

    // Tier 1: columns of in-scope tables. `virtual_columns` wins over
    // the catalog so subquery aliases (`FROM (SELECT a, b) sub`) and
    // CTE references contribute their inferred columns.
    for table in in_scope {
        let cols_owned: Option<Vec<String>> = if let Some(v) = &table.virtual_columns {
            Some(v.clone())
        } else {
            schema
                .columns_for(table.schema.as_deref(), &table.name)
                .cloned()
        };
        if let Some(cols) = cols_owned {
            for c in &cols {
                if starts_with_ci(c, prefix)
                    && seen.insert((CandidateKind::Column, c.clone()))
                {
                    out.push(Candidate {
                        display: c.clone(),
                        insert: c.clone(),
                        kind: CandidateKind::Column,
                    });
                }
            }
        }
    }

    // Tier 2: aliases in scope (helpful when typing the alias itself).
    for table in in_scope {
        if let Some(alias) = &table.alias {
            if starts_with_ci(alias, prefix)
                && seen.insert((CandidateKind::Alias, alias.clone()))
            {
                out.push(Candidate {
                    display: alias.clone(),
                    insert: alias.clone(),
                    kind: CandidateKind::Alias,
                });
            }
        }
    }

    // Tier 3: table names of in-scope tables.
    for table in in_scope {
        if starts_with_ci(&table.name, prefix)
            && seen.insert((CandidateKind::Table, table.name.clone()))
        {
            out.push(Candidate {
                display: table.name.clone(),
                insert: table.name.clone(),
                kind: CandidateKind::Table,
            });
        }
    }

    // Tier 4: every other table.
    for table in &schema.tables {
        if starts_with_ci(&table.name, prefix)
            && seen.insert((CandidateKind::Table, table.name.clone()))
        {
            out.push(Candidate {
                display: table.name.clone(),
                insert: table.name.clone(),
                kind: CandidateKind::Table,
            });
        }
    }

    // Tier 5: every other column.
    for col in schema.all_column_names() {
        if starts_with_ci(&col, prefix)
            && seen.insert((CandidateKind::Column, col.clone()))
        {
            out.push(Candidate {
                display: col.clone(),
                insert: col.clone(),
                kind: CandidateKind::Column,
            });
        }
    }

    // Tier 6: schema names.
    for s in &schema.schemas {
        if starts_with_ci(s, prefix)
            && seen.insert((CandidateKind::Schema, s.clone()))
        {
            out.push(Candidate {
                display: s.clone(),
                insert: s.clone(),
                kind: CandidateKind::Schema,
            });
        }
    }

    out
}

fn matches_for(names: &[String], prefix: &str, kind: CandidateKind) -> Vec<Candidate> {
    let mut hits: Vec<&String> = names.iter().filter(|n| starts_with_ci(n, prefix)).collect();
    hits.sort();
    hits.dedup();
    hits.into_iter()
        .map(|n| Candidate {
            display: n.clone(),
            insert: n.clone(),
            kind,
        })
        .collect()
}

fn starts_with_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.len() >= needle.len()
        && haystack[..needle.len()].eq_ignore_ascii_case(needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::schema::{SchemaCache, TableMeta};

    fn build_cache() -> SchemaCache {
        let mut c = SchemaCache::default();
        c.schemas = vec!["audit".into(), "public".into()];
        c.tables = vec![
            TableMeta {
                schema: "public".into(),
                name: "orders".into(),
            },
            TableMeta {
                schema: "public".into(),
                name: "users".into(),
            },
            TableMeta {
                schema: "audit".into(),
                name: "events".into(),
            },
        ];
        c.columns_by_table.insert(
            ("public".into(), "users".into()),
            vec!["id".into(), "email".into(), "name".into()],
        );
        c.columns_by_table.insert(
            ("public".into(), "orders".into()),
            vec!["id".into(), "user_id".into(), "total".into()],
        );
        c.columns_by_table.insert(
            ("audit".into(), "events".into()),
            vec!["id".into(), "actor".into(), "kind".into()],
        );
        c
    }

    // -- extract_identifier --

    #[test]
    fn extract_identifier_finds_unqualified_prefix() {
        let id = extract_identifier("SELECT em", 9).unwrap();
        assert_eq!(id.qualifier, None);
        assert_eq!(id.prefix, "em");
        assert_eq!(id.start, 7);
    }

    #[test]
    fn extract_identifier_splits_on_last_dot() {
        let id = extract_identifier("SELECT u.em", 11).unwrap();
        assert_eq!(id.qualifier.as_deref(), Some("u"));
        assert_eq!(id.prefix, "em");
        // Replace position: just after the `u.`
        assert_eq!(&"SELECT u.em"[id.start..11], "u.em");
    }

    #[test]
    fn extract_identifier_handles_dot_with_empty_prefix() {
        let id = extract_identifier("SELECT u.", 9).unwrap();
        assert_eq!(id.qualifier.as_deref(), Some("u"));
        assert_eq!(id.prefix, "");
    }

    #[test]
    fn extract_identifier_at_start_of_buffer() {
        let id = extract_identifier("us", 2).unwrap();
        assert_eq!(id.qualifier, None);
        assert_eq!(id.prefix, "us");
        assert_eq!(id.start, 0);
    }

    #[test]
    fn extract_identifier_after_whitespace_yields_empty_partial() {
        // Cursor on fresh space — completion can still start (offers
        // anything from the cache).
        let id = extract_identifier("SELECT ", 7).unwrap();
        assert_eq!(id.qualifier, None);
        assert_eq!(id.prefix, "");
    }

    #[test]
    fn extract_identifier_returns_none_after_random_punctuation() {
        // Punctuation that isn't a dot or whitespace shouldn't trigger.
        assert!(extract_identifier("SELECT *;", 9).is_none());
    }

    #[test]
    fn extract_identifier_walks_forward_over_trailing_word_chars() {
        // Cursor in the middle of `user_id`. The replace-range needs to
        // span the WHOLE word so Tab doesn't leave `_id` glued on.
        let id = extract_identifier("SELECT user_id", 11).unwrap(); // cursor right after `user`
        assert_eq!(id.prefix, "user");
        assert_eq!(id.start, 7);
        assert_eq!(id.end, 14, "end should walk past `_id`: {id:?}");
    }

    #[test]
    fn extract_identifier_end_equals_cursor_when_at_word_boundary() {
        let id = extract_identifier("SELECT users ", 12).unwrap();
        assert_eq!(id.end, 12);
    }

    #[test]
    fn extract_identifier_captures_three_segments() {
        let id = extract_identifier("SELECT audit.users.email", 24).unwrap();
        assert_eq!(id.schema.as_deref(), Some("audit"));
        assert_eq!(id.qualifier.as_deref(), Some("users"));
        assert_eq!(id.prefix, "email");
    }

    #[test]
    fn extract_identifier_three_segments_with_empty_prefix() {
        let id = extract_identifier("SELECT audit.users.", 19).unwrap();
        assert_eq!(id.schema.as_deref(), Some("audit"));
        assert_eq!(id.qualifier.as_deref(), Some("users"));
        assert_eq!(id.prefix, "");
    }

    #[test]
    fn extract_identifier_two_segments_has_no_schema() {
        let id = extract_identifier("SELECT u.email", 14).unwrap();
        assert!(id.schema.is_none());
        assert_eq!(id.qualifier.as_deref(), Some("u"));
        assert_eq!(id.prefix, "email");
    }

    #[test]
    fn extract_identifier_rejects_numeric_literals() {
        // `1.5` looks identifier-shaped (digits + dot) but mustn't be
        // mis-parsed as qualifier="1" / prefix="5".
        assert!(extract_identifier("SELECT 1.5", 10).is_none());
        assert!(extract_identifier("WHERE n > 0.0.0", 15).is_none());
    }

    // -- candidates_for: unqualified --

    #[test]
    fn unqualified_with_no_from_lists_tables_matching_prefix() {
        // Without a FROM clause yet, we can't tell whether the user is
        // in SELECT or FROM scope, so we offer every match that starts
        // with the prefix. The key invariant is that the real `users`
        // table shows up as a Table candidate.
        let cache = build_cache();
        let buf = "SELECT * FROM us";
        let cands = candidates_for(buf, buf.len(), &cache);
        let users = cands
            .iter()
            .find(|c| c.display == "users")
            .expect("users table should be a candidate");
        assert_eq!(users.kind, CandidateKind::Table);
    }

    #[test]
    fn unqualified_with_from_prefers_columns_of_in_scope_tables() {
        let cache = build_cache();
        let buf = "SELECT em FROM users";
        // Cursor right after the `em`
        let cur = buf.find(" FROM").unwrap();
        let cands = candidates_for(buf, cur, &cache);
        // Column `email` of users is the only "em*" hit and should come first.
        assert!(!cands.is_empty());
        assert_eq!(cands[0].display, "email");
        assert_eq!(cands[0].kind, CandidateKind::Column);
    }

    #[test]
    fn unqualified_offers_aliases_in_scope_for_select_list() {
        let cache = build_cache();
        // Cursor right after `u` in the SELECT list — clause-aware
        // completion offers columns + aliases here. Table names
        // (`users`) are deliberately NOT offered: writing `SELECT
        // users` against `FROM users u` would be a bug, and the alias
        // `u` is what the operator meant.
        let buf = "SELECT u FROM users u";
        let cur = buf.find(" FROM").unwrap();
        let cands = candidates_for(buf, cur, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"u"), "alias `u` should appear: {labels:?}");
        assert!(
            !labels.contains(&"users"),
            "SELECT-list completion shouldn't offer table name: {labels:?}"
        );
        let alias = cands.iter().find(|c| c.display == "u").unwrap();
        assert_eq!(alias.kind, CandidateKind::Alias);
    }

    // -- candidates_for: qualified --

    #[test]
    fn alias_dot_offers_only_columns_of_aliased_table() {
        let cache = build_cache();
        let buf = "SELECT u. FROM users u";
        let cur = buf.find(" FROM").unwrap();
        let cands = candidates_for(buf, cur, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert_eq!(labels, vec!["email", "id", "name"]);
        for c in &cands {
            assert_eq!(c.kind, CandidateKind::Column);
        }
    }

    #[test]
    fn table_dot_offers_columns_when_no_alias() {
        let cache = build_cache();
        let buf = "SELECT users.";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert_eq!(labels, vec!["email", "id", "name"]);
    }

    #[test]
    fn schema_dot_offers_tables_of_that_schema() {
        let cache = build_cache();
        let buf = "SELECT * FROM audit.";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert_eq!(labels, vec!["events"]);
        assert_eq!(cands[0].kind, CandidateKind::Table);
    }

    #[test]
    fn qualified_unknown_alias_yields_nothing() {
        let cache = build_cache();
        let buf = "SELECT x.foo FROM users u";
        let cur = buf.find(" FROM").unwrap();
        let cands = candidates_for(buf, cur, &cache);
        assert!(cands.is_empty());
    }

    // -- mixed case --

    #[test]
    fn matching_is_case_insensitive() {
        // Grammar-aware: SELECT-list with FROM in scope, mixed-case
        // prefix must still hit the cached column.
        let cache = build_cache();
        let buf = "SELECT EM FROM users";
        let cur = buf.find(" FROM").unwrap();
        let cands = candidates_for(buf, cur, &cache);
        assert!(cands.iter().any(|c| c.display == "email"));
    }

    // -- grammar-aware completion --

    #[test]
    fn statement_start_offers_sql_keywords() {
        let cache = build_cache();
        let buf = "SEL";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"SELECT"), "got: {labels:?}");
        assert!(cands.iter().all(|c| c.kind == CandidateKind::Keyword));
    }

    #[test]
    fn from_context_offers_tables_not_columns() {
        let cache = build_cache();
        // Cursor right after the FROM, before any table name.
        let buf = "SELECT * FROM ";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        // Tables show up.
        assert!(labels.contains(&"users"));
        assert!(labels.contains(&"orders"));
        // Columns must NOT — they don't belong in the FROM list.
        assert!(!labels.iter().any(|l| *l == "email" || *l == "user_id"));
    }

    #[test]
    fn where_context_offers_columns_of_in_scope_tables() {
        let cache = build_cache();
        let buf = "SELECT * FROM users WHERE em";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        // `email` is a column of `users` — should be offered. Table
        // names should not.
        assert!(labels.contains(&"email"), "got: {labels:?}");
        assert!(!labels.contains(&"users"));
    }

    #[test]
    fn order_by_context_offers_columns() {
        let cache = build_cache();
        let buf = "SELECT * FROM users ORDER BY em";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"email"));
    }

    #[test]
    fn insert_into_column_list_offers_only_columns_of_that_table() {
        let cache = build_cache();
        // Cursor sitting in the column list of `INSERT INTO users (...)`.
        let buf = "INSERT INTO users (em";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"email"));
        // No columns from other tables.
        assert!(!labels.contains(&"total"));
        assert!(!labels.contains(&"user_id"));
    }

    #[test]
    fn update_set_offers_columns_of_target() {
        let cache = build_cache();
        let buf = "UPDATE users SET em";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"email"));
    }

    #[test]
    fn update_where_includes_update_target_columns_without_from() {
        // UPDATE has no FROM clause, but the target table's columns
        // should still be available inside WHERE via write_target.
        let cache = build_cache();
        let buf = "UPDATE users SET name = 'x' WHERE em";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"email"), "got: {labels:?}");
    }

    #[test]
    fn delete_where_includes_target_columns() {
        let cache = build_cache();
        let buf = "DELETE FROM users WHERE em";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"email"));
    }

    #[test]
    fn values_context_yields_no_candidates() {
        let cache = build_cache();
        let buf = "INSERT INTO users (id, email) VALUES (";
        let cands = candidates_for(buf, buf.len(), &cache);
        // Inside VALUES the operator is typing literals — no identifier
        // completion. (Tests that we don't accidentally offer column
        // names where they'd be syntactically wrong.)
        assert!(cands.is_empty(), "got: {cands:?}");
    }

    #[test]
    fn schema_dot_in_from_clause_offers_tables_in_that_schema() {
        let cache = build_cache();
        let buf = "SELECT * FROM audit.";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert_eq!(labels, vec!["events"]);
        assert_eq!(cands[0].kind, CandidateKind::Table);
    }

    #[test]
    fn alias_dot_in_where_still_routes_to_columns() {
        let cache = build_cache();
        let buf = "SELECT * FROM users u WHERE u.";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert_eq!(labels, vec!["email", "id", "name"]);
    }

    #[test]
    fn function_candidates_insert_with_open_paren() {
        // Display stays as the bare name so the popup row is clean;
        // insert ends with `(` so the cursor lands ready for the first
        // argument.
        let cache = build_cache();
        let cands = candidates_for("SELECT COU FROM users", 10, &cache);
        let count = cands
            .iter()
            .find(|c| c.display == "COUNT")
            .expect("COUNT should be a candidate");
        assert_eq!(count.display, "COUNT");
        assert_eq!(count.insert, "COUNT(");
        assert_eq!(count.kind, CandidateKind::Function);
    }

    #[test]
    fn predicate_position_offers_word_operators() {
        let cache = build_cache();
        // After WHERE col, the operator (`LIKE` / `IN` / `IS NULL` etc)
        // is a natural next token.
        let buf = "SELECT * FROM users WHERE email LI";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"LIKE"), "got: {labels:?}");
    }

    #[test]
    fn predicate_offers_is_null_as_one_phrase() {
        let cache = build_cache();
        let buf = "SELECT * FROM users WHERE email IS";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(
            labels.contains(&"IS NULL"),
            "multi-word phrases should appear: {labels:?}"
        );
        assert!(labels.contains(&"IS NOT NULL"));
    }

    #[test]
    fn predicate_columns_rank_before_operators() {
        // When typing in a WHERE clause, columns should appear before
        // operators (the cycle prioritises identifiers — operators are
        // a fallback for the rest of the alphabet).
        let cache = build_cache();
        let buf = "SELECT * FROM users WHERE i";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        let id_pos = labels.iter().position(|l| *l == "id");
        let in_pos = labels.iter().position(|l| *l == "IN");
        let is_null_pos = labels.iter().position(|l| *l == "IS NULL");
        match (id_pos, in_pos.or(is_null_pos)) {
            (Some(i), Some(op)) => {
                assert!(i < op, "`id` column should rank before operators: {labels:?}");
            }
            (Some(_), None) => {} // operators not present — fine
            (None, _) => panic!("expected `id` to appear: {labels:?}"),
        }
    }

    #[test]
    fn scalar_and_window_functions_also_surface() {
        let cache = build_cache();
        // COALESCE is a scalar function.
        let cands = candidates_for("SELECT COA FROM users", 10, &cache);
        assert!(cands.iter().any(|c| c.display == "COALESCE"));
        // ROW_NUMBER is a window function.
        let cands = candidates_for("SELECT ROW FROM users", 10, &cache);
        assert!(cands.iter().any(|c| c.display == "ROW_NUMBER"));
    }

    #[test]
    fn select_list_offers_aggregate_functions() {
        let cache = build_cache();
        // After FROM exists, SELECT-list completion offers both columns
        // of in-scope tables AND SQL aggregates.
        let buf = "SELECT CO FROM users";
        let cur = buf.find(" FROM").unwrap();
        let cands = candidates_for(buf, cur, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"COUNT"), "got: {labels:?}");
        assert!(cands
            .iter()
            .find(|c| c.display == "COUNT")
            .map(|c| c.kind == CandidateKind::Function)
            .unwrap_or(false));
    }

    #[test]
    fn from_clause_offers_cte_names_from_with_block() {
        let cache = build_cache();
        let buf = "WITH active_users AS (SELECT * FROM users WHERE 1=1) SELECT * FROM act";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(
            labels.contains(&"active_users"),
            "CTE name should appear in FROM completion: {labels:?}"
        );
        let cte = cands.iter().find(|c| c.display == "active_users").unwrap();
        assert_eq!(cte.kind, CandidateKind::Table);
    }

    #[test]
    fn from_completion_still_works_without_any_cte() {
        let cache = build_cache();
        let buf = "SELECT * FROM us";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"users"));
    }

    #[test]
    fn select_list_with_no_from_offers_tables_not_random_columns() {
        // Regression: SELECT-list without a FROM used to fall back to
        // `all_column_names`, suggesting columns from any table in the
        // cache — even though without a FROM there's nowhere to bind
        // them. Tables are the useful candidates here.
        let cache = build_cache();
        let buf = "SELECT us";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"users"), "got: {labels:?}");
        // The `user_id` column from `orders` is no longer offered.
        assert!(!labels.contains(&"user_id"));
    }

    #[test]
    fn values_keyword_appears_in_statement_start() {
        // After `INSERT INTO foo (a, b) ` the operator types VALUES;
        // it should appear in the statement-start keyword list.
        let cache = build_cache();
        let cands = candidates_for("VAL", 3, &cache);
        assert!(cands.iter().any(|c| c.display == "VALUES"));
    }

    #[test]
    fn from_clause_offers_join_variants() {
        let cache = build_cache();
        // After typing a table in FROM, JOIN variants are continuation
        // candidates.
        let buf = "SELECT * FROM users LE";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"LEFT JOIN"), "got: {labels:?}");
        assert!(labels.contains(&"LEFT OUTER JOIN"));
    }

    #[test]
    fn from_clause_offers_clause_continuations() {
        let cache = build_cache();
        // After typing a table in FROM, WHERE / GROUP BY / ORDER BY
        // are the natural next clauses.
        let buf = "SELECT * FROM users WH";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"WHERE"), "got: {labels:?}");
    }

    #[test]
    fn predicate_offers_clause_continuations() {
        let cache = build_cache();
        let buf = "SELECT * FROM users WHERE id = 1 OR";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        // `ORDER BY` is a multi-word continuation candidate.
        assert!(labels.contains(&"ORDER BY"), "got: {labels:?}");
    }

    #[test]
    fn order_by_offers_limit() {
        let cache = build_cache();
        let buf = "SELECT * FROM users ORDER BY id LI";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"LIMIT"));
    }

    #[test]
    fn update_set_offers_where_returning() {
        let cache = build_cache();
        let buf = "UPDATE users SET name = 'x' WH";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"WHERE"));
    }

    #[test]
    fn statement_start_now_offers_ddl_verbs() {
        let cache = build_cache();
        let cands = candidates_for("CR", 2, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"CREATE"), "got: {labels:?}");
    }

    #[test]
    fn statement_start_now_offers_set_session() {
        let cache = build_cache();
        let cands = candidates_for("SE", 2, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        // SET (session variable) and SELECT both start with SE — both
        // should be offered.
        assert!(labels.contains(&"SET"));
        assert!(labels.contains(&"SELECT"));
    }

    #[test]
    fn three_segment_disambiguates_same_named_tables_across_schemas() {
        // build_cache has BOTH public.users (id, email, name) and
        // audit.users would NOT exist — but build_cache only has the
        // 3 tables listed. Let me construct a richer cache here.
        let mut cache = SchemaCache::default();
        cache.schemas = vec!["audit".into(), "public".into()];
        cache.tables = vec![
            crate::query::schema::TableMeta {
                schema: "public".into(),
                name: "users".into(),
            },
            crate::query::schema::TableMeta {
                schema: "audit".into(),
                name: "users".into(),
            },
        ];
        cache.columns_by_table.insert(
            ("public".into(), "users".into()),
            vec!["id".into(), "email".into()],
        );
        cache.columns_by_table.insert(
            ("audit".into(), "users".into()),
            vec!["id".into(), "actor".into(), "kind".into()],
        );
        // 3-segment: must resolve to audit.users, NOT public.users.
        let buf = "SELECT audit.users.";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"actor"), "got: {labels:?}");
        assert!(labels.contains(&"kind"));
        // public.users' `email` column must NOT appear.
        assert!(
            !labels.contains(&"email"),
            "should not leak public.users columns: {labels:?}"
        );
    }

    #[test]
    fn three_segment_unknown_schema_yields_no_candidates() {
        let cache = build_cache();
        let buf = "SELECT nope.users.";
        let cands = candidates_for(buf, buf.len(), &cache);
        // Unknown schema → silent (no fall-through to ambiguous lookup).
        assert!(cands.is_empty());
    }

    #[test]
    fn truncate_offers_tables_not_columns() {
        let cache = build_cache();
        let cands = candidates_for("TRUNCATE us", 11, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"users"));
        // No columns leaking through.
        assert!(!labels.contains(&"id"));
        assert!(!labels.contains(&"email"));
    }

    #[test]
    fn copy_paren_offers_target_columns() {
        let cache = build_cache();
        let cands = candidates_for("COPY users (em", 14, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"email"));
        // Columns of OTHER tables must NOT leak in.
        assert!(!labels.contains(&"total"));
    }

    #[test]
    fn show_all_appears_in_guc_parameter_list() {
        let cache = build_cache();
        let cands = candidates_for("SHOW al", 7, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"all"), "got: {labels:?}");
    }

    #[test]
    fn set_value_side_offers_on_off_default() {
        let cache = build_cache();
        let cands = candidates_for("SET enable_seqscan = of", 23, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"off"));
    }

    #[test]
    fn set_value_side_does_not_offer_parameter_names() {
        let cache = build_cache();
        let cands = candidates_for("SET enable_seqscan = ", 21, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        // After the `=`, the operator is typing a value — NOT another
        // parameter name.
        assert!(!labels.contains(&"timezone"));
        assert!(labels.contains(&"default"));
    }

    #[test]
    fn show_offers_guc_parameter_names() {
        let cache = build_cache();
        let cands = candidates_for("SHOW sear", 9, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"search_path"), "got: {labels:?}");
        // Must NOT offer tables / columns.
        assert!(!labels.contains(&"users"));
    }

    #[test]
    fn set_offers_guc_parameter_names() {
        let cache = build_cache();
        let cands = candidates_for("SET time", 8, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"timezone"));
    }

    #[test]
    fn drop_table_offers_table_names_and_drop_keywords() {
        let cache = build_cache();
        let cands = candidates_for("DROP TABLE us", 13, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"users"));
        // IF EXISTS / CASCADE / RESTRICT are drop-specific continuations.
        // (None match "us" so they don't appear here; they appear when
        // the prefix matches — covered by the next test.)
        assert!(!labels.contains(&"id")); // no column leakage
    }

    #[test]
    fn drop_table_offers_drop_continuations_with_matching_prefix() {
        let cache = build_cache();
        let cands = candidates_for("DROP TABLE users CAS", 20, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"CASCADE"));
    }

    #[test]
    fn drop_index_offers_index_names_not_tables() {
        let mut cache = build_cache();
        cache.indexes.push(crate::query::schema::TableMeta {
            schema: "public".into(),
            name: "users_email_idx".into(),
        });
        cache.indexes.push(crate::query::schema::TableMeta {
            schema: "public".into(),
            name: "orders_user_id_idx".into(),
        });
        let cands = candidates_for("DROP INDEX users_", 17, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"users_email_idx"), "got: {labels:?}");
        // Tables must NOT appear when dropping an index.
        assert!(!labels.contains(&"users"));
        assert!(!labels.contains(&"orders"));
    }

    #[test]
    fn drop_sequence_offers_sequence_names_not_tables() {
        let mut cache = build_cache();
        cache.sequences.push(crate::query::schema::TableMeta {
            schema: "public".into(),
            name: "user_id_seq".into(),
        });
        let cands = candidates_for("DROP SEQUENCE user_", 19, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"user_id_seq"));
        assert!(!labels.contains(&"users")); // table shouldn't leak
    }

    #[test]
    fn drop_table_does_not_leak_join_or_where_continuations() {
        // Sanity: TableRef offers JOIN variants + WHERE etc. We DON'T
        // want those in DropTarget — they'd just clutter the popup.
        let cache = build_cache();
        let cands = candidates_for("DROP TABLE users WH", 19, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(!labels.contains(&"WHERE"));
        assert!(!labels.contains(&"LEFT JOIN"));
    }

    #[test]
    fn on_conflict_on_constraint_does_not_leak_column_candidates() {
        let cache = build_cache();
        let buf = "INSERT INTO users (id) VALUES (1) ON CONFLICT ON CONSTRAINT us";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        // We don't have constraint names; the key invariant is that
        // columns / operators do NOT surface here.
        assert!(!labels.contains(&"email"));
        assert!(!labels.contains(&"id"));
    }

    #[test]
    fn on_conflict_paren_offers_target_columns() {
        let cache = build_cache();
        let buf = "INSERT INTO users (id) VALUES (1) ON CONFLICT (em";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"email"));
        // No other table's columns.
        assert!(!labels.contains(&"total"));
    }

    #[test]
    fn on_conflict_do_update_set_offers_target_columns() {
        let cache = build_cache();
        let buf = "INSERT INTO users (id) VALUES (1) ON CONFLICT (id) DO UPDATE SET em";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"email"));
    }

    #[test]
    fn excluded_dot_resolves_to_target_columns() {
        // `SET col = EXCLUDED.|` — EXCLUDED is the Postgres virtual
        // table holding the would-be-inserted row.
        let cache = build_cache();
        let buf = "INSERT INTO users (id) VALUES (1) \
                   ON CONFLICT (id) DO UPDATE SET email = EXCLUDED.";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"id"));
        assert!(labels.contains(&"email"));
        assert!(labels.contains(&"name"));
    }

    #[test]
    fn cast_as_offers_type_names() {
        let cache = build_cache();
        let cands = candidates_for("SELECT CAST(x AS in", 19, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"integer"), "got: {labels:?}");
        // Must NOT offer columns / tables.
        assert!(!labels.contains(&"id"));
        assert!(!labels.contains(&"users"));
    }

    #[test]
    fn cast_as_supports_multi_word_types() {
        let cache = build_cache();
        let cands = candidates_for("SELECT CAST(x AS time", 21, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"timestamp"));
        assert!(labels.contains(&"timestamp with time zone"));
    }

    #[test]
    fn explain_paren_offers_explain_options() {
        let cache = build_cache();
        let cands = candidates_for("EXPLAIN (AN", 11, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"ANALYZE"));
        // Must NOT offer columns / tables from the cache.
        assert!(!labels.contains(&"id"));
        assert!(!labels.contains(&"users"));
    }

    #[test]
    fn having_surfaces_select_list_aliases() {
        // `SELECT COUNT(*) AS n FROM users GROUP BY id HAVING n|`
        // Postgres lets HAVING reference the SELECT-list alias `n`.
        // WHERE doesn't — that's why HAVING is its own ctx.
        let cache = build_cache();
        let buf = "SELECT COUNT(*) AS n FROM users GROUP BY id HAVING n";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"n"), "got: {labels:?}");
        let n = cands.iter().find(|c| c.display == "n").unwrap();
        assert_eq!(n.kind, CandidateKind::Alias);
    }

    #[test]
    fn where_does_not_surface_select_list_aliases() {
        // Reverse of the above: WHERE doesn't get aliases.
        let cache = build_cache();
        let buf = "SELECT COUNT(*) AS n FROM users WHERE n";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(
            !labels.contains(&"n"),
            "WHERE shouldn't see SELECT-list aliases: {labels:?}"
        );
    }

    #[test]
    fn cte_select_star_expands_against_catalog() {
        // `WITH foo AS (SELECT * FROM users)` should make `foo.id`,
        // `foo.email`, `foo.name` available.
        let cache = build_cache();
        let buf = "WITH foo AS (SELECT * FROM users) SELECT * FROM foo WHERE foo.";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        // users' columns: id, email, name.
        assert!(labels.contains(&"id"));
        assert!(labels.contains(&"email"));
        assert!(labels.contains(&"name"));
    }

    #[test]
    fn subquery_select_star_expands_against_catalog() {
        let cache = build_cache();
        let buf = "SELECT * FROM (SELECT * FROM users) sub WHERE sub.";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"id"));
        assert!(labels.contains(&"email"));
        assert!(labels.contains(&"name"));
    }

    #[test]
    fn cte_dot_offers_cte_columns_qualified() {
        // `WITH active AS (SELECT id, email FROM users) SELECT a|`
        // → typing `active.|` should offer id + email.
        let cache = build_cache();
        let buf = "WITH active AS (SELECT id, email FROM users) SELECT * FROM active WHERE active.";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"id"), "got: {labels:?}");
        assert!(labels.contains(&"email"));
        // The cache also has `name` column on `users` — but
        // `name` is NOT in the CTE's SELECT list, so it must NOT
        // be offered for the CTE qualifier.
        assert!(!labels.contains(&"name"));
    }

    #[test]
    fn cte_columns_appear_in_outer_select_unqualified() {
        // The outer SELECT's column completion sees the CTE's columns
        // because `FROM active` brought it into scope.
        let cache = build_cache();
        let buf = "WITH active AS (SELECT id, email FROM users) SELECT em FROM active";
        let cur = buf.find(" FROM active").unwrap();
        let cands = candidates_for(buf, cur, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"email"), "got: {labels:?}");
    }

    #[test]
    fn cte_with_explicit_column_list_uses_those_columns() {
        let cache = build_cache();
        let buf = "WITH t(a, b) AS (SELECT 1, 2) SELECT * FROM t WHERE t.";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert_eq!(labels, vec!["a", "b"]);
    }

    #[test]
    fn subquery_alias_dot_offers_inferred_columns() {
        // `FROM (SELECT id, email FROM users) sub` — `sub.|` should
        // offer the inferred id + email.
        let cache = build_cache();
        let buf = "SELECT * FROM (SELECT id, email FROM users) sub WHERE sub.";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"id"));
        assert!(labels.contains(&"email"));
        assert!(!labels.contains(&"name"));
    }

    #[test]
    fn subquery_with_aliased_column_uses_alias_name() {
        // `SELECT id AS user_id` — the subquery exposes `user_id`, not
        // `id`.
        let cache = build_cache();
        let buf = "SELECT * FROM (SELECT id AS user_id FROM users) sub WHERE sub.";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"user_id"));
        assert!(!labels.contains(&"id"));
    }

    #[test]
    fn subquery_alias_appears_as_alias_candidate_in_where() {
        // `FROM (SELECT * FROM users) sub` — the alias `sub` should
        // come up in WHERE completion as an Alias kind.
        let cache = build_cache();
        let buf = "SELECT * FROM (SELECT * FROM users) sub WHERE su";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(
            labels.contains(&"sub"),
            "subquery alias should appear: {labels:?}"
        );
    }

    #[test]
    fn select_offers_postgres_catalog_functions() {
        let cache = build_cache();
        let buf = "SELECT pg_s FROM users";
        let cur = buf.find(" FROM").unwrap();
        let cands = candidates_for(buf, cur, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(
            labels.iter().any(|l| l.starts_with("PG_SIZE")),
            "expected PG_SIZE_* function: {labels:?}"
        );
    }

    #[test]
    fn empty_cache_still_offers_vocabulary_suggestions() {
        // With no schema connected, identifier suggestions (columns,
        // tables, aliases) are empty — but the SQL vocabulary (functions
        // in SELECT, operators in WHERE, keywords at statement start)
        // doesn't depend on a cache, so those still appear.
        let cache = SchemaCache::default();
        let cands = candidates_for("SELECT UP", 9, &cache);
        // `UPPER` is a scalar function in our vocabulary.
        assert!(cands.iter().any(|c| c.display == "UPPER"));
        // Statement-start keywords also work without a cache.
        let cands = candidates_for("SE", 2, &cache);
        assert!(cands.iter().any(|c| c.display == "SELECT"));
    }
}
