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
    TYPE_NAMES, VACUUM_OPTIONS, WINDOW_FUNCTIONS,
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
/// `context` carries a disambiguating origin for the popup — typically the
/// owning table for Column candidates (`email (column · users)`). `None`
/// keeps the popup row terse (`FROM (keyword)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub display: String,
    pub insert: String,
    pub kind: CandidateKind,
    pub context: Option<String>,
}

/// The cursor sits inside a `nextval('|')` literal — `start..cursor`
/// is the partial sequence-name the operator has typed inside the
/// single quotes. Detected by `detect_nextval_literal` and consumed
/// by both `extract_identifier` (to surface the replace range) and
/// `candidates_for` (to emit `cache.sequences` entries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextvalLiteral {
    /// Byte offset of the first char inside the opening `'`.
    pub start: usize,
    /// Byte offset = cursor (end of the typed partial).
    pub end: usize,
    /// Substring `buf[start..end]` — the typed partial.
    pub prefix: String,
}

/// Detect the `nextval('|')` literal context. Returns `Some` only
/// when:
///   - the cursor is inside a single-quoted string,
///   - the opening `'` is preceded by `(` (possibly via whitespace),
///   - which is preceded by the bare word `nextval` (case-insensitive,
///     not part of a longer identifier like `not_nextval`).
/// This is a fallback used by completion; it ignores escape sequences
/// inside the string — a sequence name with a `'` in it would be
/// pathological and is out of scope.
pub fn detect_nextval_literal(buf: &str, cursor: usize) -> Option<NextvalLiteral> {
    let cursor = cursor.min(buf.len());
    if !buf.is_char_boundary(cursor) {
        return None;
    }
    let before = &buf[..cursor];
    // Walk the prefix; toggle in_str on each `'`. Track the most
    // recent opening `'` so we know the literal's start position when
    // we end with in_str == true.
    let mut in_str = false;
    let mut last_open: Option<usize> = None;
    for (i, c) in before.char_indices() {
        if c == '\'' {
            if in_str {
                in_str = false;
            } else {
                in_str = true;
                last_open = Some(i);
            }
        }
    }
    if !in_str {
        return None;
    }
    let open = last_open?;
    // Anchor: head must end with `nextval (` (with optional whitespace
    // around the `(`).
    let head = &buf[..open];
    let trimmed = head.trim_end();
    let trimmed = trimmed.strip_suffix('(')?;
    let trimmed = trimmed.trim_end();
    const WANT: &str = "nextval";
    if trimmed.len() < WANT.len() {
        return None;
    }
    let tail_byte = trimmed.len() - WANT.len();
    if !trimmed.is_char_boundary(tail_byte) {
        return None;
    }
    let (head_before, tail) = trimmed.split_at(tail_byte);
    if !tail.eq_ignore_ascii_case(WANT) {
        return None;
    }
    // Word-boundary check: `not_nextval(` etc. must NOT match.
    if let Some(prev) = head_before.chars().next_back() {
        if prev.is_alphanumeric() || prev == '_' {
            return None;
        }
    }
    let prefix = buf[open + 1..cursor].to_string();
    Some(NextvalLiteral {
        start: open + 1,
        end: cursor,
        prefix,
    })
}

/// Whether `c` counts as an identifier-continuation character. Matches
/// Postgres's unquoted identifier rule loosely: ASCII letters, digits,
/// `_`, plus any Unicode alphabetic codepoint (so `café`, `naïve`, and
/// Cyrillic / CJK identifiers all complete). Numbers / `$` are out of
/// scope for now — the former clashes with numeric-literal handling
/// below.
fn is_ident_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// Walk back from `cursor` over identifier-ish characters (Unicode
/// letters, digits, `_`) plus any `.` segment separators to find the
/// partial identifier under the cursor. Returns `None` when the cursor
/// isn't inside / immediately after an identifier.
///
/// Special case: inside `nextval('|')` the cursor sits in a single-
/// quoted string, which the regular walker rejects. We synthesize an
/// `Identifier` with the in-string partial so the editor's replace
/// range and the candidate engine line up.
pub fn extract_identifier(buf: &str, cursor: usize) -> Option<Identifier> {
    // Clamp + snap to char boundary so byte-cursor arithmetic is safe.
    let cursor = cursor.min(buf.len());
    if !buf.is_char_boundary(cursor) {
        return None;
    }
    if let Some(nv) = detect_nextval_literal(buf, cursor) {
        return Some(Identifier {
            start: nv.start,
            end: nv.end,
            schema: None,
            qualifier: None,
            prefix: nv.prefix,
        });
    }
    // Walk backward by char so multi-byte UTF-8 (`café`, `naïve`,
    // Cyrillic, CJK) is accepted as identifier-shaped.
    let mut start = cursor;
    for (idx, ch) in buf[..cursor].char_indices().rev() {
        if is_ident_char(ch) || ch == '.' {
            start = idx;
        } else {
            break;
        }
    }
    // Walk forward over identifier chars (no dots — we only honour the
    // qualifier the user already typed) so `SELECT user|_id` + Tab
    // replaces the whole `user_id`, not just `user`. Char-aware too.
    let mut end = cursor;
    for (i, ch) in buf[cursor..].char_indices() {
        if is_ident_char(ch) {
            end = cursor + i + ch.len_utf8();
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
        let prev_char = buf[..cursor].chars().next_back();
        match prev_char {
            None => {
                return Some(Identifier {
                    start: cursor,
                    end,
                    schema: None,
                    qualifier: None,
                    prefix: String::new(),
                });
            }
            Some(c) if c.is_whitespace() => {
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
            (Some(first.to_string()), Some(middle), last.to_string())
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
pub fn candidates_for(buf: &str, cursor: usize, schema: &SchemaCache) -> Vec<Candidate> {
    // In-string nextval('|') context — wholly separate candidate set
    // (sequence names from `cache.sequences`). Short-circuit before
    // the regular extract / classify pipeline.
    if let Some(nv) = detect_nextval_literal(buf, cursor) {
        return schema
            .sequences
            .iter()
            .filter(|s| {
                let qualified = format!("{}.{}", s.schema, s.name);
                starts_with_ci(&s.name, &nv.prefix) || starts_with_ci(&qualified, &nv.prefix)
            })
            .map(|s| {
                // Render schema-qualified when not in `public` so the
                // operator sees disambiguation; the `insert` value
                // matches the display.
                let qualified = if s.schema.eq_ignore_ascii_case("public") {
                    s.name.clone()
                } else {
                    format!("{}.{}", s.schema, s.name)
                };
                Candidate {
                    display: qualified.clone(),
                    insert: qualified,
                    kind: CandidateKind::Table,
                    context: Some("sequence".to_string()),
                }
            })
            .collect();
    }
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
        if let Some(cols) = schema.columns_for(t.schema.as_deref(), &t.name).cloned() {
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

    let cands = candidates_for_in_context(
        &id,
        &classification.ctx,
        classification.write_target.as_ref(),
        &in_scope,
        &ctes,
        &select_aliases,
        schema,
    );
    if !cands.is_empty() {
        return cands;
    }
    // Fuzzy fallback: prefix-anchored matching turned up nothing, but
    // the operator typed enough chars to be specific. Try a subsequence
    // match across identifiers in scope — `usr` → `users`,
    // `user_logs`, `user_roles`; `idnt_ld` → `identity_load`. Keyed off
    // a 3-char threshold so single letters / pairs don't fan out to
    // every identifier in the schema.
    let prefix_chars = id.prefix.chars().count();
    if prefix_chars >= 3 {
        return candidates_fuzzy(&id, &in_scope, &ctes, schema);
    }
    cands
}

/// Subsequence-match score for fuzzy completion. Lower = better.
/// Returns None when `needle` isn't a subsequence of `haystack` (after
/// ASCII case folding). The score combines three factors so tighter
/// matches rank above looser ones:
///   - match SPAN (last_matched - first_matched + 1) — contiguous
///     matches beat strung-out ones,
///   - position of the FIRST matched char — matches near the start
///     beat matches deep in the candidate,
///   - candidate LENGTH — shorter beats longer when the rest is a tie.
pub fn fuzzy_score(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let needle_lc: Vec<char> = needle.chars().map(|c| c.to_ascii_lowercase()).collect();
    let haystack_lc: Vec<char> = haystack.chars().map(|c| c.to_ascii_lowercase()).collect();
    if needle_lc.len() > haystack_lc.len() {
        return None;
    }
    let mut ni = 0usize;
    let mut first_match: Option<usize> = None;
    let mut last_match: usize = 0;
    for (i, &hc) in haystack_lc.iter().enumerate() {
        if ni < needle_lc.len() && hc == needle_lc[ni] {
            if first_match.is_none() {
                first_match = Some(i);
            }
            last_match = i;
            ni += 1;
        }
    }
    if ni < needle_lc.len() {
        return None;
    }
    let first = first_match.unwrap_or(0);
    let span = last_match - first + 1;
    // Weights chosen so SPAN dominates (100×), then first position
    // (10×), then length is the tiebreaker. Concrete numbers don't
    // matter as long as the ordering matches the intent.
    Some(span * 100 + first * 10 + haystack_lc.len())
}

/// Fuzzy-fallback candidate set. Scans every name we could plausibly
/// surface — in-scope tables/aliases/CTEs and the full schema cache —
/// scores each with `fuzzy_score`, and returns the top results sorted
/// by score (tightest match first). Skips keywords / operators /
/// functions: those are short and the operator usually remembers
/// them; bulking the fuzzy result with `FROM` for `usr` would just be
/// noise.
/// Cap on the fuzzy result list — both the unqualified scan and each
/// qualified arm honour this. Lift here so a future tweak only touches
/// one site (per code review: redeclaring this inside each arm was a
/// maintenance trap waiting to drift).
const MAX_FUZZY_RESULTS: usize = 30;

fn candidates_fuzzy(
    id: &Identifier,
    in_scope: &[TableRefInQuery],
    ctes: &[crate::query::clause::CteDef],
    schema: &SchemaCache,
) -> Vec<Candidate> {
    let prefix = &id.prefix;
    let mut scored: Vec<(usize, Candidate)> = Vec::new();
    let mut seen: std::collections::BTreeSet<(CandidateKind, String)> =
        std::collections::BTreeSet::new();

    let push = |scored: &mut Vec<(usize, Candidate)>,
                seen: &mut std::collections::BTreeSet<(CandidateKind, String)>,
                name: &str,
                kind: CandidateKind,
                context: Option<String>| {
        if let Some(score) = fuzzy_score(name, prefix) {
            if seen.insert((kind, name.to_string())) {
                scored.push((
                    score,
                    Candidate {
                        display: name.to_string(),
                        insert: name.to_string(),
                        kind,
                        context,
                    },
                ));
            }
        }
    };

    // Qualified fuzzy (`alias.usr` / `schema.usr`) — narrow the scan
    // to the qualifier's children only. The operator typed the
    // qualifier intentionally; broadening to the whole cache would
    // ignore that signal.
    if let Some(q) = &id.qualifier {
        let q_lower = q.to_ascii_lowercase();
        // Alias / table-in-scope qualifier → its columns.
        for t in in_scope {
            if t.match_key() == q_lower {
                let cols_owned: Option<Vec<String>> = if let Some(v) = &t.virtual_columns {
                    Some(v.clone())
                } else {
                    schema.columns_for(t.schema.as_deref(), &t.name).cloned()
                };
                if let Some(cols) = cols_owned {
                    for c in &cols {
                        push(
                            &mut scored,
                            &mut seen,
                            c,
                            CandidateKind::Column,
                            Some(t.name.clone()),
                        );
                    }
                }
                scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.display.cmp(&b.1.display)));
                return scored
                    .into_iter()
                    .take(MAX_FUZZY_RESULTS)
                    .map(|(_, c)| c)
                    .collect();
            }
        }
        // Schema qualifier → its tables.
        if schema.schemas.iter().any(|s| s.eq_ignore_ascii_case(q)) {
            for t in &schema.tables {
                if t.schema.eq_ignore_ascii_case(q) {
                    push(
                        &mut scored,
                        &mut seen,
                        &t.name,
                        CandidateKind::Table,
                        Some(q.clone()),
                    );
                }
            }
            scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.display.cmp(&b.1.display)));
            return scored
                .into_iter()
                .take(MAX_FUZZY_RESULTS)
                .map(|(_, c)| c)
                .collect();
        }
        // Bare table-name qualifier with no FROM scope → its columns.
        if let Some(cols) = schema.columns_for(None, q) {
            for c in cols {
                push(
                    &mut scored,
                    &mut seen,
                    c,
                    CandidateKind::Column,
                    Some(q.clone()),
                );
            }
            scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.display.cmp(&b.1.display)));
            return scored
                .into_iter()
                .take(MAX_FUZZY_RESULTS)
                .map(|(_, c)| c)
                .collect();
        }
        // Unrecognised qualifier — nothing to fuzz over.
        return Vec::new();
    }

    // Unqualified fuzzy — broad scan across everything in scope plus
    // the cache.

    // Columns of in-scope tables (virtual_columns wins, like everywhere
    // else, so CTE / subquery columns participate).
    for t in in_scope {
        let cols_owned: Option<Vec<String>> = if let Some(v) = &t.virtual_columns {
            Some(v.clone())
        } else {
            schema.columns_for(t.schema.as_deref(), &t.name).cloned()
        };
        if let Some(cols) = cols_owned {
            let ctx = t.alias.clone().unwrap_or_else(|| t.name.clone());
            for c in &cols {
                push(
                    &mut scored,
                    &mut seen,
                    c,
                    CandidateKind::Column,
                    Some(ctx.clone()),
                );
            }
        }
    }
    // Aliases bound by the FROM clause.
    for t in in_scope {
        if let Some(alias) = &t.alias {
            push(
                &mut scored,
                &mut seen,
                alias,
                CandidateKind::Alias,
                Some(t.name.clone()),
            );
        }
    }
    // CTE names.
    for c in ctes {
        push(&mut scored, &mut seen, &c.name, CandidateKind::Table, None);
    }
    // All tables in the cache.
    for t in &schema.tables {
        let ctx = if t.schema.eq_ignore_ascii_case("public") {
            None
        } else {
            Some(t.schema.clone())
        };
        push(&mut scored, &mut seen, &t.name, CandidateKind::Table, ctx);
    }
    // All schemas.
    for s in &schema.schemas {
        push(&mut scored, &mut seen, s, CandidateKind::Schema, None);
    }
    // Every column anywhere (no per-table context here — could be
    // ambiguous; the fuzzy fallback is exploratory anyway).
    for col in schema.all_column_names() {
        push(&mut scored, &mut seen, &col, CandidateKind::Column, None);
    }

    // Sort ascending by score (tightest match first). Cap the visible
    // result list so a 3-char prefix that subsequence-matches half the
    // schema doesn't drown the popup.
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.display.cmp(&b.1.display)));
    scored
        .into_iter()
        .take(MAX_FUZZY_RESULTS)
        .map(|(_, c)| c)
        .collect()
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
    write_target: Option<&QualifiedTable>,
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
        let cols = schema.columns_for(Some(s), t).cloned().unwrap_or_default();
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
        ClauseContext::Values => candidates_from_list(&id.prefix, continuations::AFTER_VALUES),

        // INSERT INTO foo (|  → columns of `foo` (specifically).
        ClauseContext::InsertColumns(t) => columns_of(t, &id.prefix, schema),

        // EXPLAIN (|  → the documented options.
        ClauseContext::ExplainOptions => candidates_from_list(&id.prefix, EXPLAIN_OPTIONS),

        // VACUUM (|  /  ANALYZE (|  → maintenance options.
        ClauseContext::VacuumOptions => candidates_from_list(&id.prefix, VACUUM_OPTIONS),

        // SHOW | / SET |  → GUC parameter names.
        ClauseContext::GucParameter => GUC_PARAMETERS
            .iter()
            .filter(|p| starts_with_ci(p, &id.prefix))
            .map(|p| Candidate {
                display: (*p).to_string(),
                insert: (*p).to_string(),
                kind: CandidateKind::Keyword,
                context: None,
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
                context: None,
            })
            .collect(),

        // ON CONFLICT ON CONSTRAINT |  → unique/PK constraints owned
        // by the INSERT target. Filtered by write_target.table so a
        // hundred-table database doesn't list unrelated constraints.
        ClauseContext::ConstraintName => {
            let target = match write_target {
                Some(t) => t,
                None => return Vec::new(),
            };
            schema
                .constraints
                .iter()
                .filter(|c| {
                    c.table.eq_ignore_ascii_case(&target.name)
                        && match &target.schema {
                            Some(s) => c.schema.eq_ignore_ascii_case(s),
                            None => true,
                        }
                        && starts_with_ci(&c.name, &id.prefix)
                })
                .map(|c| Candidate {
                    display: c.name.clone(),
                    insert: c.name.clone(),
                    kind: CandidateKind::Table,
                    context: None,
                })
                .collect()
        }

        // CAST(expr AS |  → SQL type names. Multi-word types like
        // `timestamp with time zone` land as one Tab.
        // Also used for the type position inside a CREATE TABLE
        // column list: `CREATE TABLE t (id |` and
        // `CREATE TABLE t (id INT, name |`.
        ClauseContext::TypeName | ClauseContext::CreateTableColumnType => TYPE_NAMES
            .iter()
            .filter(|t| starts_with_ci(t, &id.prefix))
            .map(|t| Candidate {
                display: (*t).to_string(),
                insert: (*t).to_string(),
                kind: CandidateKind::Keyword,
                context: None,
            })
            .collect(),

        // CREATE TABLE t (|  or  (id INT, |  — the operator is naming
        // a fresh column. We have nothing useful to offer; returning
        // an empty list suppresses the popup without breaking
        // anything.
        ClauseContext::CreateTableColumns => Vec::new(),

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
                            context: None,
                        })
                        .collect()
                }
                None => {
                    let mut out: Vec<Candidate> = Vec::new();
                    let mut seen = std::collections::BTreeSet::new();
                    for t in names {
                        if starts_with_ci(&t.name, &id.prefix) && seen.insert(t.name.clone()) {
                            out.push(Candidate {
                                display: t.name.clone(),
                                insert: t.name.clone(),
                                kind: CandidateKind::Table,
                                context: None,
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
                                context: None,
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
            out.extend(candidates_from_list(
                &id.prefix,
                continuations::AFTER_UPDATE_ASSIGN,
            ));
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
                        context: None,
                    })
                    .collect();
                out.extend(candidates_tables_and_schemas(&id.prefix, schema));
                out.extend(candidates_from_list(&id.prefix, JOIN_VARIANTS));
                out.extend(candidates_from_list(
                    &id.prefix,
                    continuations::AFTER_TABLE_REF,
                ));
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
                // Clause continuations BEFORE functions: after typing
                // `SELECT * F`, FROM is the natural next clause and
                // should rank above FORMAT / FLOOR.
                out.extend(candidates_from_list(
                    &id.prefix,
                    continuations::AFTER_SELECT_LIST,
                ));
                out.extend(candidates_functions(&id.prefix));
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
                out.extend(candidates_from_list(
                    &id.prefix,
                    continuations::AFTER_PREDICATE,
                ));
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
                            context: None,
                        });
                    }
                }
                out.extend(candidates_predicate_operators(&id.prefix));
                out.extend(candidates_from_list(
                    &id.prefix,
                    continuations::AFTER_PREDICATE,
                ));
                out
            }
        },
        ClauseContext::OrderOrGroup => match id.qualifier.as_deref() {
            Some(q) => candidates_for_qualified(q, &id.prefix, in_scope, schema),
            None => {
                let mut out = candidates_columns_only(&id.prefix, in_scope, schema);
                out.extend(candidates_from_list(
                    &id.prefix,
                    continuations::AFTER_ORDER_OR_GROUP,
                ));
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
        .map(|kw| {
            let text = case_match(kw, prefix);
            Candidate {
                display: text.clone(),
                insert: text,
                kind: CandidateKind::Keyword,
                context: None,
            }
        })
        .collect()
}

/// Function candidates — aggregates + scalar + window functions, all
/// inserted as `NAME(` so the cursor lands inside the parens ready for
/// the operator's first argument. `display` stays as the bare name so
/// the popup row reads cleanly. Case mirrors the operator's prefix.
fn candidates_functions(prefix: &str) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut seen: std::collections::BTreeSet<&'static str> = std::collections::BTreeSet::new();
    for source in [AGGREGATE_FUNCTIONS, SCALAR_FUNCTIONS, WINDOW_FUNCTIONS] {
        for fname in source {
            if seen.insert(fname) && starts_with_ci(fname, prefix) {
                let text = case_match(fname, prefix);
                out.push(Candidate {
                    display: text.clone(),
                    insert: format!("{text}("),
                    kind: CandidateKind::Function,
                    context: None,
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
        .map(|op| {
            let text = case_match(op, prefix);
            Candidate {
                display: text.clone(),
                insert: text,
                kind: CandidateKind::Keyword,
                context: None,
            }
        })
        .collect()
}

/// Generic helper — turn a static `&[&str]` of keywords into matching
/// `Candidate`s. Used for the "continuation" lists (what clauses can
/// follow this position) and the JOIN variants. All emitted as
/// `CandidateKind::Keyword` so the popup row reads `(keyword)`. Case
/// mirrors the operator's prefix.
fn candidates_from_list(prefix: &str, list: &[&str]) -> Vec<Candidate> {
    list.iter()
        .filter(|w| starts_with_ci(w, prefix))
        .map(|w| {
            let text = case_match(w, prefix);
            Candidate {
                display: text.clone(),
                insert: text,
                kind: CandidateKind::Keyword,
                context: None,
            }
        })
        .collect()
}

fn candidates_tables_and_schemas(prefix: &str, schema: &SchemaCache) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut seen_tables: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for t in &schema.tables {
        if starts_with_ci(&t.name, prefix) && seen_tables.insert(t.name.clone()) {
            out.push(Candidate {
                display: t.name.clone(),
                insert: t.name.clone(),
                kind: CandidateKind::Table,
                context: None,
            });
        }
    }
    for s in &schema.schemas {
        if starts_with_ci(s, prefix) {
            out.push(Candidate {
                display: s.clone(),
                insert: s.clone(),
                kind: CandidateKind::Schema,
                context: None,
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
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // In-scope tables first.
    for t in in_scope {
        if let Some(cols) = schema.columns_for(t.schema.as_deref(), &t.name) {
            for c in cols {
                if starts_with_ci(c, prefix) && seen.insert(c.clone()) {
                    out.push(Candidate {
                        display: c.clone(),
                        insert: c.clone(),
                        kind: CandidateKind::Column,
                        // Disambiguates same-named columns across joined
                        // tables (e.g. two `id` columns). Use the alias
                        // when present — it's the shorter, more familiar
                        // label and matches what the operator would type
                        // for qualified access.
                        context: Some(t.alias.clone().unwrap_or_else(|| t.name.clone())),
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
                    // The aliased table — so the popup shows
                    // `u (alias · users)`.
                    context: Some(t.name.clone()),
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
                    context: None,
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
            let mut out = matches_for(&cols, prefix, CandidateKind::Column);
            // Context = the underlying table name (so `u.|` against
            // `users u` shows `email (column · users)` — the qualifier
            // the operator already typed is just an alias, the table
            // name is the disambiguating info).
            for c in &mut out {
                c.context = Some(table.name.clone());
            }
            return out;
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
        let mut out = matches_for(&names, prefix, CandidateKind::Table);
        for c in &mut out {
            c.context = Some(qualifier.to_string());
        }
        return out;
    }

    // 3) qualifier might be a table name with no FROM scope yet (e.g.
    //    `SELECT users.|`). Offer its columns.
    if let Some(cols) = schema.columns_for(None, qualifier) {
        let mut out = matches_for(cols, prefix, CandidateKind::Column);
        for c in &mut out {
            c.context = Some(qualifier.to_string());
        }
        return out;
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
                if starts_with_ci(c, prefix) && seen.insert((CandidateKind::Column, c.clone())) {
                    out.push(Candidate {
                        display: c.clone(),
                        insert: c.clone(),
                        kind: CandidateKind::Column,
                        context: Some(table.alias.clone().unwrap_or_else(|| table.name.clone())),
                    });
                }
            }
        }
    }

    // Tier 2: aliases in scope (helpful when typing the alias itself).
    for table in in_scope {
        if let Some(alias) = &table.alias {
            if starts_with_ci(alias, prefix) && seen.insert((CandidateKind::Alias, alias.clone())) {
                out.push(Candidate {
                    display: alias.clone(),
                    insert: alias.clone(),
                    kind: CandidateKind::Alias,
                    context: Some(table.name.clone()),
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
                context: None,
            });
        }
    }

    // Tier 4: every other table. Non-public schemas surface as context
    // so the popup shows `events (table · analytics)` and the operator
    // knows they'll need to qualify the name.
    for table in &schema.tables {
        if starts_with_ci(&table.name, prefix)
            && seen.insert((CandidateKind::Table, table.name.clone()))
        {
            let ctx = if table.schema.eq_ignore_ascii_case("public") {
                None
            } else {
                Some(table.schema.clone())
            };
            out.push(Candidate {
                display: table.name.clone(),
                insert: table.name.clone(),
                kind: CandidateKind::Table,
                context: ctx,
            });
        }
    }

    // Tier 5: every other column.
    for col in schema.all_column_names() {
        if starts_with_ci(&col, prefix) && seen.insert((CandidateKind::Column, col.clone())) {
            out.push(Candidate {
                display: col.clone(),
                insert: col.clone(),
                kind: CandidateKind::Column,
                context: None,
            });
        }
    }

    // Tier 6: schema names.
    for s in &schema.schemas {
        if starts_with_ci(s, prefix) && seen.insert((CandidateKind::Schema, s.clone())) {
            out.push(Candidate {
                display: s.clone(),
                insert: s.clone(),
                kind: CandidateKind::Schema,
                context: None,
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
            context: None,
        })
        .collect()
}

fn starts_with_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.len() >= needle.len() && haystack[..needle.len()].eq_ignore_ascii_case(needle)
}

/// Mirror the operator's chosen case onto a keyword / function /
/// operator template. The vocabulary stores everything uppercase (or
/// lowercase for GUCs / types) by contract — this lets the inserted
/// candidate match whatever the operator was typing instead of
/// always forcing case.
///
/// Rule: if the prefix is non-empty and contains no uppercase letters,
/// downcase the template. Otherwise keep the template as authored.
/// Empty prefix → lowercase (modern Postgres style; the operator can
/// still cycle past it).
pub(crate) fn case_match(template: &str, prefix: &str) -> String {
    let lower = prefix.chars().all(|c| !c.is_ascii_uppercase());
    if lower {
        template.to_ascii_lowercase()
    } else {
        template.to_string()
    }
}

/// Longest common prefix of `xs`, compared case-insensitively but
/// returning the substring of the FIRST entry (so it keeps that
/// entry's case for the operator to see). `[]` and one-element inputs
/// return the whole first entry / empty.
pub(crate) fn longest_common_prefix_ci(xs: &[&str]) -> String {
    let mut iter = xs.iter();
    let Some(first) = iter.next() else {
        return String::new();
    };
    let rest: Vec<&&str> = iter.collect();
    if rest.is_empty() {
        return (*first).to_string();
    }
    let mut end = 0usize;
    for (i, c) in first.char_indices() {
        let next = i + c.len_utf8();
        let all_match = rest.iter().all(|s| {
            s.get(i..next)
                .map(|seg| seg.eq_ignore_ascii_case(&first[i..next]))
                .unwrap_or(false)
        });
        if all_match {
            end = next;
        } else {
            break;
        }
    }
    first[..end].to_string()
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
    fn extract_identifier_accepts_non_ascii_letters_at_end_of_partial() {
        // `café` ends in a non-ASCII letter; the byte-walker rejected
        // it. Char-walker accepts it.
        let id = extract_identifier("SELECT café", 12).unwrap();
        assert_eq!(id.prefix, "café");
        assert_eq!(id.qualifier, None);
        // Replace range covers the whole word (5 bytes for `café` — `é`
        // is 2 bytes — plus the 7-byte `SELECT ` prefix).
        assert_eq!(id.start, 7);
        assert_eq!(id.end, 12);
    }

    #[test]
    fn extract_identifier_dot_completion_after_non_ascii_qualifier() {
        // `café.|` — the bug from the backlog. Cursor sits right after
        // the dot; completion should fire with qualifier=café and an
        // empty prefix.
        let buf = "SELECT café.";
        let id = extract_identifier(buf, buf.len()).unwrap();
        assert_eq!(id.qualifier.as_deref(), Some("café"));
        assert_eq!(id.prefix, "");
    }

    #[test]
    fn extract_identifier_accepts_cyrillic_identifier() {
        // Non-Latin Unicode identifiers (Cyrillic, CJK, …) are valid
        // Postgres unquoted identifiers; completion should treat them
        // the same as ASCII.
        let buf = "SELECT пользователь";
        let id = extract_identifier(buf, buf.len()).unwrap();
        assert_eq!(id.prefix, "пользователь");
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
                assert!(
                    i < op,
                    "`id` column should rank before operators: {labels:?}"
                );
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
    fn on_constraint_offers_constraint_names_scoped_to_target() {
        let mut cache = build_cache();
        cache
            .constraints
            .push(crate::query::schema::ConstraintMeta {
                schema: "public".into(),
                table: "users".into(),
                name: "users_email_key".into(),
            });
        cache
            .constraints
            .push(crate::query::schema::ConstraintMeta {
                schema: "public".into(),
                table: "users".into(),
                name: "users_pkey".into(),
            });
        // Different table — must NOT appear.
        cache
            .constraints
            .push(crate::query::schema::ConstraintMeta {
                schema: "public".into(),
                table: "orders".into(),
                name: "orders_pkey".into(),
            });
        let buf = "INSERT INTO users (id) VALUES (1) ON CONFLICT ON CONSTRAINT us";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"users_email_key"), "got: {labels:?}");
        assert!(labels.contains(&"users_pkey"));
        // Orders' constraint must NOT leak.
        assert!(!labels.contains(&"orders_pkey"));
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
    fn vacuum_paren_offers_vacuum_options() {
        let cache = build_cache();
        let cands = candidates_for("VACUUM (FU", 10, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"FULL"), "got: {labels:?}");
        // Must NOT offer columns / tables.
        assert!(!labels.contains(&"users"));
    }

    #[test]
    fn analyze_paren_offers_vacuum_options() {
        let cache = build_cache();
        let cands = candidates_for("ANALYZE (VER", 12, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"VERBOSE"));
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
        let labels: Vec<String> = cands.iter().map(|c| c.display.clone()).collect();
        // Case follows the operator's prefix (lowercase here).
        assert!(
            labels
                .iter()
                .any(|l| l.to_ascii_lowercase().starts_with("pg_size")),
            "expected pg_size_* function: {labels:?}"
        );
    }

    // -- pure helpers --

    #[test]
    fn case_match_lowercases_when_prefix_is_lowercase() {
        assert_eq!(super::case_match("SELECT", "sel"), "select");
        assert_eq!(super::case_match("LEFT JOIN", "le"), "left join");
    }

    #[test]
    fn case_match_keeps_template_when_prefix_has_any_uppercase() {
        assert_eq!(super::case_match("SELECT", "SEL"), "SELECT");
        assert_eq!(super::case_match("SELECT", "Sel"), "SELECT");
    }

    #[test]
    fn case_match_empty_prefix_lowercases() {
        // Default to lowercase for the empty-prefix case (modern style).
        assert_eq!(super::case_match("SELECT", ""), "select");
    }

    #[test]
    fn lcp_of_empty_and_single() {
        assert_eq!(super::longest_common_prefix_ci(&[]), "");
        assert_eq!(super::longest_common_prefix_ci(&["foo"]), "foo");
    }

    #[test]
    fn lcp_finds_common_prefix() {
        assert_eq!(
            super::longest_common_prefix_ci(&["t_users", "t_user_logs", "t_user_roles"]),
            "t_user"
        );
        assert_eq!(
            super::longest_common_prefix_ci(&["users", "user_logs", "user_roles"]),
            "user"
        );
    }

    #[test]
    fn lcp_handles_case_insensitivity() {
        // Different case across entries — the LCP comparison is
        // case-insensitive, but the returned value is from the first
        // entry's case.
        let xs = vec!["UsErS", "users_logs", "USERS_ROLES"];
        let got = super::longest_common_prefix_ci(&xs);
        assert!(got.eq_ignore_ascii_case("users"));
    }

    #[test]
    fn lcp_returns_empty_when_no_overlap() {
        assert_eq!(super::longest_common_prefix_ci(&["foo", "bar"]), "");
    }

    // -- case preservation --

    #[test]
    fn lowercase_prefix_lowercase_keyword_candidate() {
        // `sel|` should offer `select`, not `SELECT`.
        let cache = build_cache();
        let cands = candidates_for("sel", 3, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"select"), "got: {labels:?}");
        assert!(!labels.contains(&"SELECT"));
    }

    #[test]
    fn uppercase_prefix_keeps_uppercase_keyword() {
        let cache = build_cache();
        let cands = candidates_for("SEL", 3, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"SELECT"));
    }

    #[test]
    fn lowercase_prefix_lowercases_multiword_join_variants() {
        let cache = build_cache();
        let buf = "select * from users le";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"left join"));
        assert!(labels.contains(&"left outer join"));
    }

    // -- continuation ranking --

    #[test]
    fn select_then_f_prefers_from_over_format() {
        let cache = build_cache();
        let cands = candidates_for("SELECT * F", 10, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        let from_pos = labels.iter().position(|l| l.eq_ignore_ascii_case("FROM"));
        let format_pos = labels.iter().position(|l| l.eq_ignore_ascii_case("FORMAT"));
        match (from_pos, format_pos) {
            (Some(f), Some(fmt)) => assert!(f < fmt, "FROM should rank before FORMAT: {labels:?}"),
            (Some(_), None) => {} // fine, FORMAT not offered
            (None, _) => panic!("FROM should appear: {labels:?}"),
        }
    }

    #[test]
    fn column_candidates_carry_their_table_as_context() {
        let cache = build_cache();
        // SELECT-list, two tables in scope sharing `id`. Both `id`
        // candidates should carry the table name as context.
        let cands = candidates_for("SELECT i FROM users u JOIN orders o ON true", 8, &cache);
        let id_contexts: Vec<&str> = cands
            .iter()
            .filter(|c| c.display == "id" && c.kind == CandidateKind::Column)
            .filter_map(|c| c.context.as_deref())
            .collect();
        // One `id` per table — the dedup is on column name, so we get
        // the first occurrence (`u` since users comes first in FROM).
        // The contract is: when context is set, it's the alias / table.
        assert!(
            id_contexts.iter().any(|c| *c == "u" || *c == "users"),
            "expected an `id` candidate with context u/users, got {id_contexts:?}"
        );
    }

    #[test]
    fn qualified_columns_carry_table_context() {
        let cache = build_cache();
        // `u.|` — column candidates for users.
        let cands = candidates_for("SELECT u. FROM users u", 9, &cache);
        let cols: Vec<&Candidate> = cands
            .iter()
            .filter(|c| c.kind == CandidateKind::Column)
            .collect();
        assert!(!cols.is_empty(), "expected some column candidates");
        for c in &cols {
            assert_eq!(
                c.context.as_deref(),
                Some("users"),
                "alias.col completion should set context to underlying table: {c:?}"
            );
        }
    }

    #[test]
    fn keywords_have_no_context() {
        let cache = build_cache();
        let cands = candidates_for("SE", 2, &cache);
        let select = cands
            .iter()
            .find(|c| c.display.eq_ignore_ascii_case("SELECT"))
            .expect("SELECT keyword");
        assert!(select.context.is_none());
    }

    // --- fuzzy fallback ---

    #[test]
    fn fuzzy_score_subsequence_matches_with_relative_ranking() {
        // `usr` is a subsequence of all three; the tighter match
        // (`users`: matched chars 0..3 within length 5) should score
        // lower than the looser one (`user_logs`: matched chars 0..3
        // within length 9).
        let users = fuzzy_score("users", "usr").expect("users matches usr");
        let user_logs = fuzzy_score("user_logs", "usr").expect("user_logs matches usr");
        let user_login_session_records =
            fuzzy_score("user_login_session_records", "usr").expect("matches");
        assert!(users < user_logs);
        assert!(user_logs < user_login_session_records);

        // Anchored at start beats anchored deeper: `users` (u at 0)
        // beats `housing_users` (u at 0 too actually — let me use a
        // clearer pair).
        let start = fuzzy_score("abc_target", "abc").expect("starts-with-like");
        let deep = fuzzy_score("xy_abc_target", "abc").expect("contains");
        assert!(
            start < deep,
            "earlier match should beat later: {start} vs {deep}"
        );

        // Non-match: needle has char not in haystack.
        assert!(fuzzy_score("users", "xyz").is_none());
        // Non-match: order matters (`s` before `u` in haystack rules
        // it out).
        assert!(fuzzy_score("ab", "ba").is_none());
        // Empty needle is rejected.
        assert!(fuzzy_score("users", "").is_none());
    }

    #[test]
    fn fuzzy_fallback_kicks_in_when_starts_with_returns_nothing() {
        let cache = build_cache();
        // `usr` doesn't prefix-match anything in build_cache(), but
        // `users` is a subsequence — fallback should surface it.
        let cands = candidates_for("SELECT * FROM usr", 17, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(
            labels.iter().any(|l| *l == "users"),
            "fuzzy should surface `users` for prefix `usr`; got {labels:?}"
        );
    }

    #[test]
    fn fuzzy_threshold_does_not_fire_for_short_prefixes() {
        let cache = build_cache();
        // Two chars `xz` — no prefix-match. Should NOT fall back to
        // fuzzy (would otherwise scan everything containing x and z).
        let cands = candidates_for("SELECT * FROM xz", 16, &cache);
        // Result: empty (or only continuations not matching `xz`).
        // The contract we test: no table whose name contains x and z
        // in order should appear here.
        let table_hits: Vec<&str> = cands
            .iter()
            .filter(|c| c.kind == CandidateKind::Table)
            .map(|c| c.display.as_str())
            .collect();
        assert!(
            table_hits.is_empty(),
            "fuzzy must not fire below the 3-char threshold; got {table_hits:?}"
        );
    }

    #[test]
    fn fuzzy_respects_alias_qualifier() {
        let cache = build_cache();
        // `u.nme` (typo of name) — no starts-with against users
        // columns. Fuzzy fallback should ONLY scan `u`'s columns,
        // not the whole cache. So `actor` (from audit.events) must
        // NOT appear, but `name` (from users) should.
        let cands = candidates_for("SELECT u.nme FROM users u", 12, &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(
            labels.iter().any(|l| *l == "name"),
            "expected fuzzy-`nmm` to surface `name` from users; got {labels:?}"
        );
        assert!(
            !labels.iter().any(|l| *l == "actor"),
            "qualifier `u` should bound fuzzy scan to users' columns, not all tables; got {labels:?}"
        );
    }

    #[test]
    fn fuzzy_skips_when_starts_with_already_has_matches() {
        let cache = build_cache();
        // `use` prefix-matches `users` directly — fuzzy should NOT
        // be invoked, so we still get the standard starts-with
        // result set (including continuation keywords like JOIN).
        let cands = candidates_for("SELECT * FROM use", 17, &cache);
        let displays: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(displays.iter().any(|l| *l == "users"));
    }

    #[test]
    fn nextval_literal_detects_open_string_after_nextval_paren() {
        let buf = "SELECT nextval('";
        let nv = detect_nextval_literal(buf, buf.len()).unwrap();
        assert_eq!(nv.prefix, "");
        assert_eq!(nv.start, buf.len());
    }

    #[test]
    fn nextval_literal_captures_partial_prefix_inside_quotes() {
        let buf = "SELECT nextval('user_id_se";
        let nv = detect_nextval_literal(buf, buf.len()).unwrap();
        assert_eq!(nv.prefix, "user_id_se");
        // Replace range starts right after the opening `'`.
        assert_eq!(&buf[nv.start..nv.end], "user_id_se");
    }

    #[test]
    fn nextval_literal_is_case_insensitive_for_keyword() {
        let buf = "SELECT NEXTVAL('";
        assert!(detect_nextval_literal(buf, buf.len()).is_some());
    }

    #[test]
    fn nextval_literal_rejects_when_string_is_closed_before_cursor() {
        // `'users_seq'` is a complete literal — the cursor sits AFTER
        // the closing quote, so we're not in-string anymore.
        let buf = "SELECT nextval('users_seq')";
        assert!(detect_nextval_literal(buf, buf.len()).is_none());
    }

    #[test]
    fn nextval_literal_rejects_when_identifier_extends_into_nextval() {
        // `not_nextval(` would otherwise pass the suffix check; the
        // word-boundary guard rejects it.
        let buf = "SELECT not_nextval('";
        assert!(detect_nextval_literal(buf, buf.len()).is_none());
    }

    #[test]
    fn nextval_literal_rejects_string_outside_nextval_context() {
        let buf = "SELECT * FROM t WHERE x = 'hello";
        assert!(detect_nextval_literal(buf, buf.len()).is_none());
    }

    #[test]
    fn candidates_for_nextval_literal_offers_sequence_names() {
        let mut cache = build_cache();
        cache.sequences = vec![
            TableMeta {
                schema: "public".into(),
                name: "users_id_seq".into(),
            },
            TableMeta {
                schema: "public".into(),
                name: "orders_id_seq".into(),
            },
            TableMeta {
                schema: "audit".into(),
                name: "events_id_seq".into(),
            },
        ];
        let buf = "SELECT nextval('users";
        let cands = candidates_for(buf, buf.len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        // Filtered to those starting with "users".
        assert!(labels.contains(&"users_id_seq"));
        assert!(!labels.contains(&"orders_id_seq"));
        // Public sequences render bare; non-public render schema-
        // qualified.
        let cands = candidates_for("SELECT nextval('", "SELECT nextval('".len(), &cache);
        let labels: Vec<&str> = cands.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.contains(&"users_id_seq"));
        assert!(labels.contains(&"audit.events_id_seq"));
    }

    #[test]
    fn extract_identifier_surfaces_nextval_replace_range() {
        let buf = "SELECT nextval('user";
        let id = extract_identifier(buf, buf.len()).unwrap();
        // The synthesized Identifier should let the editor replace
        // just the in-string partial, not the surrounding `nextval('`.
        assert_eq!(id.prefix, "user");
        assert_eq!(&buf[id.start..id.end], "user");
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
