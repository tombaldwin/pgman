//! Synthetic-data demo mode (`pgman --demo`).
//!
//! Builds a fully-populated [`App`] with no database, no network,
//! and no disk reads: `ledger`, the billing database of a fictional
//! B2B subscriptions product ([`schema`] has its two dozen relations,
//! partitions and views), a result grid of overdue invoices, saved
//! queries (including a `:param` one), environments from production to
//! a local container with a replica, drift and pending patches, and a
//! burst of JDBC-tap events that trips the live N+1 detector. Used for
//! screenshots, the README demo gif (`vhs demo.tape`), and talks, so
//! the frames are the same on every launch (the tap burst lands at the
//! same moment each time) and it never touches a real server.
//!
//! `App::demo` is set so the run loop skips the connection and
//! skips persisting draft / history / saved-queries to disk — a
//! demo run can't clobber the operator's real session.

mod schema;

pub use schema::{live_lint, table_ddl};

use crate::app::{App, ConnState, DatabaseInfo};
use crate::conn::Dsn;
use crate::grid::Grid;
use crate::query::schema::SchemaCache;
use crate::safety::SafetyConfig;
use crate::saved::SavedQuery;
use crate::tap::{TapEvent, TapKind};
use crate::theme::Theme;

/// The query the demo opens with (`app`) and the grid it answers.
const OPENING_QUERY: &str = "SELECT number, customer_id, status, currency, total, due_at\n\
                             FROM invoices\n\
                             WHERE status = 'open'\n\
                             ORDER BY due_at;";

/// Build the demo [`App`]. Pure (no I/O); everything is in memory.
pub fn app(theme: Theme) -> App {
    // Staging's RDS endpoint, as connecting from the environments view
    // would leave it; the header cuts it at a dot so Database and User
    // keep their place. The run loop never connects in demo mode.
    let dsn = Dsn::parse(
        "postgres://billing@ledger-staging.c7xk2rpq9w1d.eu-west-2.rds.amazonaws.com:5432/ledger",
    )
    .ok();
    let mut a = App::new(theme, dsn, Vec::new(), SafetyConfig::default());
    a.demo = true;
    // Demo runs are for screenshots / the README gif — no network,
    // no disk writes, and every frame identical. An update check
    // would violate all three.
    a.update_check_enabled = false;
    a.environments_loader = Some(crate::app::EnvironmentsLoader::Immediate(environments()));
    a.env_prober = Some(crate::app::EnvProber::Immediate(std::sync::Arc::new(
        environment_probes,
    )));
    a.env_log_fetcher = Some(crate::app::EnvLogFetcher {
        fetch: std::sync::Arc::new(|_, _| Ok(log_errors())),
        immediate: true,
    });
    a.env_patcher = Some(crate::app::EnvPatcher {
        plan: std::sync::Arc::new(|e, s, d, outside| {
            Box::pin(std::future::ready(Ok(patch_plan(&e, &s, &d, outside))))
        }),
        apply: std::sync::Arc::new(|p, mode, _cancel| {
            use crate::inventory::patching::{CopyNote, RunMode};
            let ran = p
                .patches
                .iter()
                .filter_map(|x| x.file.version_id)
                .max()
                .or(p.applied_max);
            Box::pin(std::future::ready(
                crate::inventory::patching::ApplyReport {
                    results: p
                        .patches
                        .iter()
                        .map(|x| crate::inventory::patching::PatchResult {
                            file: x.file.file.clone(),
                            outcome: Ok(x.statements.len()),
                        })
                        .collect(),
                    error: None,
                    // Every planned patch ran and recorded its own id;
                    // a dry run kept none of it.
                    recorded_after: match mode {
                        RunMode::DryRun => p.applied_max,
                        _ => ran,
                    },
                    mode,
                    copy: (mode == RunMode::Copy).then(|| CopyNote {
                        name: crate::query::apply::copy_name(&p.database, 1_760_000_000),
                        dropped: Ok(()),
                    }),
                },
            ))
        }),
        immediate: true,
    });
    a.env_discoverer = Some(crate::app::EnvDiscoverer {
        discover: std::sync::Arc::new(|_| Ok(discovery())),
        save: std::sync::Arc::new(|_| Err("--demo saves nothing".into())),
        immediate: true,
    });
    a.dsn_origin =
        Some("--demo, synthetic data · environment staging ledger-staging/ledger (direct)".into());
    a.conn_state = ConnState::Connected {
        server_version: "16.4 (demo)".into(),
    };
    a.schema_cache = schema::schema_cache();
    // The opening grid is the opening query's answer, so running it
    // again changes nothing.
    a.grid = answer(OPENING_QUERY, &a.schema_cache);
    // Populate the derived view state (visible rows + selection)
    // through the same path a real QueryOk uses, so the demo can't
    // drift from the live renderer.
    a.reset_grid_view();
    a.grid_view.source = Some(("public".into(), "invoices".into()));
    a.editor.buffer = OPENING_QUERY.into();
    a.editor.cursor = a.editor.buffer.len();
    for q in saved_queries() {
        a.saved_queries.upsert(q);
    }
    a.history = vec![
        "SELECT count(*) FROM webhook_deliveries WHERE delivered_at IS NULL;".into(),
        "SELECT * FROM invoice_lines WHERE invoice_id = 48207;".into(),
        "SELECT * FROM customers WHERE id = 2107;".into(),
    ];
    a.notifications.items = notifications();
    let events = tap_events();
    a.tap_health.query_count = events.len() as u64;
    if let Some(last) = events.last() {
        a.tap_health.last_event_at_unix_micros = last.received_at_unix_micros;
    }
    a.tap_events = events.into();
    // Replacing the ring wholesale is a mutation like any other —
    // the aggregates are memoised on this counter.
    a.bump_tap_generation();
    a
}

/// Build the demo [`App`] open on the start card instead of a
/// pre-run result — the sixty-second story (`docs/logs-to-sql.md`)
/// starts with an empty grid so the F8 / F4 hint is the first thing
/// on screen, not a result the operator hasn't asked for yet.
///
/// Everything else — schema cache, saved queries, history — is
/// identical to [`app`]; only the grid (cleared back to
/// `Grid::default()`, same as a fresh, query-less connection), the
/// editor (empty: the walkthrough says `e` then paste, and a paste
/// appended to the pre-filled SELECT), `databases` (populated, so the
/// start card's databases line isn't blank) and the tap (quiet until
/// its burst arrives, `DEMO_TAP_DELAY` in) differ.
pub fn launch_app(theme: Theme) -> App {
    let mut a = app(theme);
    a.grid = Grid::default();
    a.reset_grid_view();
    a.editor.buffer.clear();
    a.editor.cursor = 0;
    a.editor.preferred_col = None;
    a.editor_highlight_cache = None;
    a.databases = vec![
        DatabaseInfo {
            name: "ledger".into(),
            size: "6.2 GB".into(),
        },
        DatabaseInfo {
            name: "ledgerlog".into(),
            size: "1.1 GB".into(),
        },
        DatabaseInfo {
            name: "ledger_test".into(),
            size: "212 MB".into(),
        },
    ];
    // Nothing has happened at launch, so the tap is quiet: the burst
    // arrives a couple of seconds in, an event a frame, and the TAP /
    // N+1 badges light as it lands — the live tap's story, not a card
    // pre-lit before anything ran.
    let pending = std::mem::take(&mut a.tap_events);
    a.tap_health = Default::default();
    a.bump_tap_generation();
    a.demo_tap_feed = Some(crate::app::DemoTapFeed {
        due: std::time::Instant::now() + DEMO_TAP_DELAY,
        pending,
    });
    a
}

/// How long `--demo` keeps the tap quiet before its burst arrives.
pub const DEMO_TAP_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// Answer a query synthetically — no client, no network, called from
/// `App::spawn_run_demo` on `Guard::Allow` in `--demo` mode.
///
/// A `SELECT` whose single `FROM` relation is in `cache` gets rows
/// shaped by that relation's real columns: the written-out `invoices`
/// and `customers`, filtered by the WHERE's `column = value` terms;
/// deterministic generated rows for everything else, made to satisfy
/// those terms. Everything else — writes that reached `Allow` under a
/// customised safety profile, joins, unknown tables, `SELECT 1`-style
/// queries with no table at all — gets a one-row notice so the
/// operator sees *something* rather than a raw error.
pub fn answer(sql: &str, cache: &SchemaCache) -> Grid {
    let table = crate::app::infer_single_source_table(sql).filter(|(schema, name)| {
        cache
            .columns_meta_by_table
            .contains_key(&(schema.clone(), name.clone()))
    });
    let equalities = where_clause_of(sql)
        .map(where_equalities)
        .unwrap_or_default();
    let full = match table {
        Some((schema, name)) => {
            let canned = match (schema.as_str(), name.as_str()) {
                ("public", "invoices") => Some(schema::invoices()),
                ("public", "customers") => Some(schema::customers()),
                _ => None,
            };
            match canned {
                Some((columns, rows)) => filter_rows(
                    Grid {
                        columns,
                        rows,
                        truncated: false,
                    },
                    &equalities,
                ),
                None => {
                    let cols = cache
                        .columns_meta_by_table
                        .get(&(schema, name.clone()))
                        .cloned()
                        .unwrap_or_default();
                    generated_rows(&name, &cols, &equalities)
                }
            }
        }
        // The notice is a one-column answer about the demo itself, not
        // about the statement — projecting it would be nonsense.
        None => return notice_grid(),
    };
    let mut grid = match select_list_of(sql) {
        Some(list) => project_columns(list, full),
        None => full,
    };
    if let Some(n) = trailing_limit(sql) {
        grid.rows.truncate(n);
    }
    grid
}

/// `n` when `sql` ends `LIMIT n` (an optional `;` after it), so a
/// demo answer is no longer than the statement asked for — the
/// schema browser's `p` asks for five rows and `customers` has twelve.
fn trailing_limit(sql: &str) -> Option<usize> {
    let sql = sql.trim().trim_end_matches(';');
    let mut words = sql.split_whitespace().rev();
    let n = words.next()?.parse().ok()?;
    words
        .next()
        .filter(|w| w.eq_ignore_ascii_case("limit"))
        .map(|_| n)
}

/// The SELECT list of `sql` — the text between a leading `SELECT` and
/// the first top-level `FROM` — or `None` when `sql` isn't a plain
/// SELECT. A SELECT with no FROM at all (`SELECT 1`) yields the whole
/// remainder.
///
/// Only top-level `FROM` counts: parenthesised subqueries and string
/// literals are skipped, so `SELECT (SELECT 1 FROM t) AS x FROM u`
/// still cuts at the outer one.
fn select_list_of(sql: &str) -> Option<&str> {
    let after_select = {
        let s = sql.trim_start();
        let head = s.get(..6)?;
        if !head.eq_ignore_ascii_case("select") {
            return None;
        }
        let rest = &s[6..];
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        rest
    };
    let mut depth = 0usize;
    let mut in_string = false;
    for (i, c) in after_select.char_indices() {
        match c {
            '\'' => in_string = !in_string,
            '(' if !in_string => depth += 1,
            ')' if !in_string => depth = depth.saturating_sub(1),
            'f' | 'F' if !in_string && depth == 0 && word_at(after_select, i, "from") => {
                return Some(after_select[..i].trim());
            }
            _ => {}
        }
    }
    Some(after_select.trim())
}

/// True when `word` sits at byte offset `at` in `text` as a whole
/// word — i.e. neither neighbour is an identifier character.
fn word_at(text: &str, at: usize, word: &str) -> bool {
    let before_ok = text[..at]
        .chars()
        .next_back()
        .is_none_or(|c| !is_ident_char(c));
    let Some(rest) = text.get(at..) else {
        return false;
    };
    // `get`, not a byte slice: `word.len()` need not be a char boundary
    // in `rest` (`fé€` straddles byte 4). Once the head is ASCII-equal
    // to `word`, `word.len()` IS a boundary, so the slice below is safe.
    let Some(head) = rest.get(..word.len()) else {
        return false;
    };
    if !head.eq_ignore_ascii_case(word) {
        return false;
    }
    let after_ok = rest[word.len()..]
        .chars()
        .next()
        .is_none_or(|c| !is_ident_char(c));
    before_ok && after_ok
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The `column = value` terms of a WHERE that is nothing but terms
/// joined by `AND`: `(column, value)` with the column's last
/// dot-separated segment and the value unquoted. Terms of any other
/// shape (`due_at < now()`, `IN (…)`) are skipped; a WHERE with a
/// top-level `OR` yields nothing, since dropping a row its other branch
/// would keep answers a different question. Pure.
fn where_equalities(clause: &str) -> Vec<(String, String)> {
    let mut terms = Vec::new();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut start = 0usize;
    for (i, c) in clause.char_indices() {
        match c {
            '\'' => in_string = !in_string,
            '(' if !in_string => depth += 1,
            ')' if !in_string => depth = depth.saturating_sub(1),
            _ if in_string || depth > 0 => {}
            _ if word_at(clause, i, "or") => return Vec::new(),
            _ if word_at(clause, i, "and") => {
                terms.push(&clause[start..i]);
                start = i + "and".len();
            }
            _ => {}
        }
    }
    terms.push(&clause[start..]);
    terms
        .into_iter()
        .filter_map(|term| {
            let (lhs, rhs) = term.split_once('=')?;
            // `<=`, `>=`, `!=` leave their first character on the left,
            // which no column name holds.
            let column = column_name_of(lhs)?;
            let rhs = rhs.trim();
            let value = match rhs.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
                Some(v) if !v.contains('\'') => v,
                Some(_) => return None,
                None if !rhs.is_empty() && rhs.chars().all(|c| c.is_ascii_digit()) => rhs,
                None => return None,
            };
            Some((column.to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}

/// `grid`'s rows whose cell in each `equalities` column holds its
/// value. A column the grid doesn't have is no filter at all. Pure.
fn filter_rows(grid: Grid, equalities: &[(String, String)]) -> Grid {
    let tests: Vec<(usize, &str)> = equalities
        .iter()
        .filter_map(|(col, value)| {
            let i = grid
                .columns
                .iter()
                .position(|c| c.eq_ignore_ascii_case(col))?;
            Some((i, value.as_str()))
        })
        .collect();
    Grid {
        rows: grid
            .rows
            .into_iter()
            .filter(|row| {
                tests
                    .iter()
                    .all(|(i, v)| row.get(*i).is_some_and(|c| c == v))
            })
            .collect(),
        ..grid
    }
}

/// Narrow `grid` to the columns a simple SELECT list names, so
/// `SELECT id, status FROM invoices` doesn't answer with every column
/// the table has.
///
/// Deliberately conservative — the whole grid comes back unchanged
/// unless every item is a bare (optionally alias-qualified) column
/// name that the grid actually carries. `*`, an expression, a
/// function call, a `DISTINCT`, a name the grid doesn't have: all
/// keep every column, because a demo that quietly drops a column the
/// operator asked for teaches the wrong thing about the real one.
pub fn project_columns(select_list: &str, grid: Grid) -> Grid {
    let items = split_top_level_commas(select_list);
    if items.is_empty() {
        return grid;
    }
    let mut wanted = Vec::with_capacity(items.len());
    for item in items {
        let Some(name) = column_name_of(item) else {
            return grid;
        };
        let Some(idx) = grid
            .columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(name))
        else {
            return grid;
        };
        wanted.push(idx);
    }
    Grid {
        columns: wanted.iter().map(|&i| grid.columns[i].clone()).collect(),
        rows: grid
            .rows
            .iter()
            .map(|row| {
                wanted
                    .iter()
                    .map(|&i| row.get(i).cloned().unwrap_or_default())
                    .collect()
            })
            .collect(),
        truncated: grid.truncated,
    }
}

/// `item` as a plain column name: `id`, `o.id`, `public.o.id` — the
/// last dot-separated segment. `None` for anything else (`*`, `o.*`,
/// `count(*)`, `a + b`, `id AS x`), which is the caller's signal to
/// keep every column.
fn column_name_of(item: &str) -> Option<&str> {
    let item = item.trim();
    if item.is_empty() || !item.chars().all(|c| is_ident_char(c) || c == '.') {
        return None;
    }
    let last = item.rsplit('.').next()?;
    (!last.is_empty() && !last.chars().next()?.is_ascii_digit()).then_some(last)
}

/// Split a SELECT list on commas that aren't inside parentheses or a
/// string literal.
fn split_top_level_commas(list: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut start = 0usize;
    for (i, c) in list.char_indices() {
        match c {
            '\'' => in_string = !in_string,
            '(' if !in_string => depth += 1,
            ')' if !in_string => depth = depth.saturating_sub(1),
            ',' if !in_string && depth == 0 => {
                out.push(&list[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    let tail = &list[start..];
    if !tail.trim().is_empty() || !out.is_empty() {
        out.push(tail);
    }
    out
}

/// A one-row notice: what `answer` falls back to for anything it
/// can't shape a plausible result for.
fn notice_grid() -> Grid {
    Grid {
        columns: vec!["demo".to_string()],
        rows: vec![vec!["this is --demo mode, no database".to_string()]],
        truncated: false,
    }
}

/// `\df`'s answer in `--demo`: the demo schema's functions, in the
/// columns the live catalog query returns, filtered by `pattern` the
/// way the server would filter them (psql wildcards, `schema.`
/// prefix, unquoted text folded). With no pattern only `public` is
/// listed, as live; the one `pg_catalog` row is there so `\df now`
/// shows a pattern reaching system schemas.
pub fn functions_grid(pattern: Option<&str>) -> Grid {
    use crate::query::backslash::{glob_matches, split_pattern};
    let wanted = pattern.map(str::trim).filter(|p| !p.is_empty());
    let rows = schema::FUNCTIONS
        .iter()
        .filter(|[schema, name, ..]| match wanted {
            None => *schema == "public",
            Some(p) => {
                let (s, n) = split_pattern(p);
                glob_matches(&n, name) && s.is_none_or(|s| glob_matches(&s, schema))
            }
        })
        .map(|f| row(f))
        .collect();
    Grid {
        columns: ["schema", "name", "result", "arguments", "type"]
            .map(String::from)
            .to_vec(),
        rows,
        truncated: false,
    }
}

/// A canned `EXPLAIN (FORMAT JSON)` answer, shaped exactly like the
/// one-column / one-row grid Postgres returns — so
/// `AppMsg::QueryOk`'s handler parses it and opens `Mode::ExplainTree`
/// through the same path a live EXPLAIN takes.
///
/// `--demo` used to route an EXPLAIN through `answer`, which projects
/// *result* columns: the grid's first cell was a row value like `42`,
/// `explain::parse` failed on it, and the demo fell back to raw text
/// every single time. EXPLAIN was the one feature the demo could never
/// show.
///
/// A query on one relation is planned the way Postgres would plan it
/// against the demo's row estimates: an index scan when the WHERE pins
/// the leading column of one of the table's indexes, a sequential scan
/// (filtered) otherwise, and an Append over the partitions of a
/// partitioned table. Two tables joined by a foreign key are a Hash
/// Join on it; anything else is shown the invoices ⋈ customers join.
/// `analyze` adds the `Actual *` fields Postgres only emits under
/// ANALYZE, so the tree renders timings for `EXPLAIN ANALYZE` and
/// estimates for plain `EXPLAIN` — the same difference a live session
/// shows.
pub fn explain_plan_grid(analyze: bool, sql: &str, cache: &SchemaCache) -> Grid {
    let table = crate::app::infer_single_source_table(sql).filter(|(schema, name)| {
        cache
            .columns_meta_by_table
            .contains_key(&(schema.clone(), name.clone()))
    });
    let plan = match table.and_then(|(s, n)| schema::rel(&s, &n)) {
        Some(rel) => scan_plan(analyze, rel, where_clause_of(sql)),
        None => join_plan(analyze, sql),
    };
    let json = serde_json::json!([{ "Plan": plan }]);
    Grid {
        columns: vec!["QUERY PLAN".to_string()],
        rows: vec![vec![serde_json::to_string_pretty(&json).unwrap_or_default()]],
        truncated: false,
    }
}

/// Round to two decimals, as Postgres prints costs and timings.
fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// One plan node's JSON: the fields Postgres always writes, plus the
/// `Actual *` ones under ANALYZE (`ms` the node's total time).
fn node(
    analyze: bool,
    kind: &str,
    cost: (f64, f64),
    rows: u64,
    width: u64,
    ms: f64,
) -> serde_json::Map<String, serde_json::Value> {
    use serde_json::json;
    let mut n = serde_json::Map::new();
    n.insert("Node Type".into(), json!(kind));
    n.insert("Startup Cost".into(), json!(r2(cost.0)));
    n.insert("Total Cost".into(), json!(r2(cost.1)));
    n.insert("Plan Rows".into(), json!(rows));
    n.insert("Plan Width".into(), json!(width));
    if analyze {
        n.insert("Actual Startup Time".into(), json!(r2(ms / 40.0)));
        n.insert("Actual Total Time".into(), json!(r2(ms)));
        n.insert("Actual Rows".into(), json!(rows));
        n.insert("Actual Loops".into(), json!(1));
    }
    n
}

/// The leading column of each of `rel`'s full (non-partial) indexes,
/// with whether that index is unique.
fn leading_index_columns(rel: &schema::Rel) -> Vec<(String, String, bool)> {
    schema::indexes_of(rel)
        .into_iter()
        .filter(|ix| !ix.definition.contains(" WHERE "))
        .filter_map(|ix| {
            let cols = ix.definition.split("USING btree (").nth(1)?;
            let first = cols.split([',', ')', ' ']).next()?.to_string();
            // A composite key is unique only as a whole.
            let unique = ix.unique && !cols.split(')').next()?.contains(',');
            Some((ix.name, first, unique))
        })
        .collect()
}

/// How many rows an equality on `column` of `rel` finds: one for a
/// unique column, the table's rows per parent for a foreign key, and a
/// small fraction otherwise.
fn equality_rows(rel: &schema::Rel, column: &str, unique: bool) -> u64 {
    if unique {
        return 1;
    }
    let parent = schema::FOREIGN_KEYS
        .iter()
        .find(|(c, col, _)| *c == rel.name && *col == column)
        .and_then(|(_, _, p)| schema::rel("public", p));
    match parent {
        Some(p) if p.rows > 0 => (rel.rows / p.rows).max(1),
        _ => (rel.rows / 1_000).max(1),
    }
}

/// The plan for a query on one relation, filtered by `filter` when it
/// has a WHERE.
fn scan_plan(
    analyze: bool,
    rel: &schema::Rel,
    filter: Option<&str>,
) -> serde_json::Map<String, serde_json::Value> {
    use serde_json::json;
    let filter = filter.map(|f| f.split_whitespace().collect::<Vec<_>>().join(" "));
    if let schema::Kind::Partitioned(_) = rel.kind {
        let children: Vec<serde_json::Value> = schema::partitions_of(rel.name)
            .map(|p| {
                let mut child = scan_plan(analyze, p, filter.as_deref());
                child.insert("Parent Relationship".into(), json!("Member"));
                serde_json::Value::Object(child)
            })
            .collect();
        let sum = |key: &str| -> f64 {
            children
                .iter()
                .filter_map(|c| c.get(key).and_then(|v| v.as_f64()))
                .sum()
        };
        let rows = sum("Plan Rows") as u64;
        let mut n = node(
            analyze,
            "Append",
            (0.0, sum("Total Cost")),
            rows,
            48,
            sum("Actual Total Time"),
        );
        n.insert("Plans".into(), json!(children));
        return n;
    }
    let width = 8 * rel.columns.len() as u64 + 16;
    let index = filter.as_deref().and_then(|f| {
        let eqs = where_equalities(f);
        leading_index_columns(rel)
            .into_iter()
            .find(|(_, col, _)| eqs.iter().any(|(c, _)| c == col))
            .map(|(name, col, unique)| (name, col, unique, f.to_string()))
    });
    if let Some((name, col, unique, f)) = index {
        let rows = equality_rows(rel, &col, unique);
        let total = 0.43 + 4.0 + rows as f64 * 1.2;
        let mut n = node(
            analyze,
            "Index Scan",
            (0.43, total),
            rows,
            width,
            0.02 + rows as f64 * 0.004,
        );
        n.insert("Scan Direction".into(), json!("Forward"));
        n.insert("Index Name".into(), json!(name));
        n.insert("Relation Name".into(), json!(rel.name));
        n.insert("Alias".into(), json!(rel.name));
        n.insert("Index Cond".into(), json!(format!("({f})")));
        return n;
    }
    let pages = (rel.bytes.0 / 8192).max(1) as f64;
    let total_rows = rel.rows.max(1);
    let rows = match filter {
        Some(_) => (total_rows / 4).max(1),
        None => total_rows,
    };
    let cost = pages + total_rows as f64 * if filter.is_some() { 0.0125 } else { 0.01 };
    let mut n = node(
        analyze,
        "Seq Scan",
        (0.0, cost),
        rows,
        width,
        0.05 + total_rows as f64 / 1_000_000.0 * 95.0,
    );
    n.insert("Relation Name".into(), json!(rel.name));
    n.insert("Alias".into(), json!(rel.name));
    if let Some(f) = filter {
        n.insert("Filter".into(), json!(format!("({f})")));
        if analyze {
            n.insert("Rows Removed by Filter".into(), json!(total_rows - rows));
        }
    }
    n
}

/// The Hash Join two foreign-key-related tables in `sql` are planned
/// as, or invoices ⋈ customers for anything else. A WHERE filters the
/// child side, the one the join's rows come from.
fn join_plan(analyze: bool, sql: &str) -> serde_json::Map<String, serde_json::Value> {
    use serde_json::json;
    let refs = crate::query::from_parse::parse_from_tables(sql);
    let alias = |name: &str| {
        refs.iter()
            .find(|r| r.name == name)
            .and_then(|r| r.alias.clone())
            .unwrap_or_else(|| name.to_string())
    };
    let named: Vec<&str> = refs.iter().map(|r| r.name.as_str()).collect();
    let (child, column, parent) = schema::FOREIGN_KEYS
        .iter()
        .find(|(c, _, p)| named.contains(c) && named.contains(p))
        .copied()
        .unwrap_or(("invoices", "customer_id", "customers"));
    let (Some(outer), Some(inner)) = (schema::rel("public", child), schema::rel("public", parent))
    else {
        return node(analyze, "Result", (0.0, 0.01), 1, 0, 0.01);
    };
    let scan = |rel: &schema::Rel, rel_alias: String, filter: Option<&str>| {
        let mut n = scan_plan(analyze, rel, filter);
        n.insert("Alias".into(), json!(rel_alias));
        n
    };
    let mut outer_scan = scan(outer, alias(child), where_clause_of(sql));
    outer_scan.insert("Parent Relationship".into(), json!("Outer"));
    let outer_rows = outer_scan["Plan Rows"].as_u64().unwrap_or(outer.rows);
    let mut inner_scan = scan(inner, alias(parent), None);
    inner_scan.insert("Parent Relationship".into(), json!("Outer"));
    let inner_cost = inner_scan["Total Cost"].as_f64().unwrap_or(0.0);
    let inner_ms = inner_scan
        .get("Actual Total Time")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let mut hash = node(
        analyze,
        "Hash",
        (inner_cost, inner_cost),
        inner.rows,
        40,
        inner_ms * 1.3,
    );
    hash.insert("Parent Relationship".into(), json!("Inner"));
    hash.insert("Plans".into(), json!([inner_scan]));
    let outer_cost = outer_scan["Total Cost"].as_f64().unwrap_or(0.0);
    let outer_ms = outer_scan
        .get("Actual Total Time")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let mut join = node(
        analyze,
        "Hash Join",
        (inner_cost, inner_cost + outer_cost * 1.2),
        outer_rows,
        96,
        (outer_ms + inner_ms) * 1.6,
    );
    join.insert("Join Type".into(), json!("Inner"));
    join.insert(
        "Hash Cond".into(),
        json!(format!(
            "({}.{column} = {}.id)",
            alias(child),
            alias(parent)
        )),
    );
    join.insert("Plans".into(), json!([outer_scan, hash]));
    join
}

/// The text of `sql`'s top-level WHERE, up to its ORDER / GROUP /
/// LIMIT / `;`, or `None` when there is none.
fn where_clause_of(sql: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut start = None;
    for (i, c) in sql.char_indices() {
        match c {
            '\'' => in_string = !in_string,
            '(' if !in_string => depth += 1,
            ')' if !in_string => depth = depth.saturating_sub(1),
            ';' if !in_string && depth == 0 => {
                return start
                    .map(|s: usize| sql[s..i].trim())
                    .filter(|w| !w.is_empty());
            }
            _ if in_string || depth > 0 => {}
            _ if start.is_none() && word_at(sql, i, "where") => start = Some(i + "where".len()),
            _ if start.is_some()
                && ["order", "group", "limit", "offset"]
                    .iter()
                    .any(|w| word_at(sql, i, w)) =>
            {
                return start.map(|s| sql[s..i].trim()).filter(|w| !w.is_empty());
            }
            _ => {}
        }
    }
    start.map(|s| sql[s..].trim()).filter(|w| !w.is_empty())
}

/// Number of synthetic rows `generated_rows` fabricates per table —
/// a screenful, deterministic across runs.
const GENERATED_ROW_COUNT: usize = 8;

/// Fabricate `GENERATED_ROW_COUNT` deterministic rows shaped by
/// `cols` — every value is a pure function of `(table, column name /
/// type, row index)`, so the same query always answers the same way.
///
/// The WHERE's `column = value` terms hold in every row: the column
/// takes the value, and a term on `id` answers that one row.
fn generated_rows(
    table: &str,
    cols: &[crate::query::schema::ColumnMeta],
    equalities: &[(String, String)],
) -> Grid {
    let pinned = |name: &str| {
        equalities
            .iter()
            .find(|(c, _)| c.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    let count = if pinned("id").is_some() {
        1
    } else {
        GENERATED_ROW_COUNT
    };
    let columns = cols.iter().map(|c| c.name.clone()).collect();
    let rows = (0..count)
        .map(|i| {
            cols.iter()
                .map(|c| {
                    pinned(&c.name)
                        .unwrap_or_else(|| schema::synth_cell(table, &c.name, &c.type_name, i))
                })
                .collect()
        })
        .collect();
    Grid {
        columns,
        rows,
        truncated: false,
    }
}

fn row(cells: &[&str]) -> Vec<String> {
    cells.iter().map(|s| s.to_string()).collect()
}

fn saved_queries() -> Vec<SavedQuery> {
    vec![
        SavedQuery {
            name: "overdue-invoices".into(),
            body: "SELECT number, customer_id, currency, total, due_at\nFROM invoices\nWHERE status = 'open'\n  AND due_at < now()\nORDER BY due_at;".into(),
        },
        SavedQuery {
            name: "customer-invoices".into(),
            body: "SELECT number, status, total, due_at\nFROM invoices\nWHERE customer_id = :customer_id\nORDER BY issued_at DESC;".into(),
        },
        SavedQuery {
            name: "mrr-by-month".into(),
            body: "SELECT month, currency, mrr, customers\nFROM reporting.mrr_by_month\nORDER BY month DESC\nLIMIT 12;".into(),
        },
        SavedQuery {
            name: "failing-webhooks-24h".into(),
            body: "SELECT webhook_id, status_code, count(*)\nFROM webhook_deliveries\nWHERE status_code >= 500\n  AND created_at > now() - interval '24 hours'\nGROUP BY 1, 2\nORDER BY 3 DESC;".into(),
        },
    ]
}

/// `NOTIFY` arrivals on the billing service's channel, for the F3 panel.
fn notifications() -> Vec<crate::conn::NotificationMsg> {
    [
        (
            "invoice_events",
            31_822,
            "{\"invoice\": \"INV-004827\", \"event\": \"payment_failed\"}",
        ),
        (
            "invoice_events",
            31_822,
            "{\"invoice\": \"INV-004831\", \"event\": \"finalized\"}",
        ),
        ("dunning", 31_907, "{\"customer_id\": 2107, \"stage\": 2}"),
    ]
    .into_iter()
    .map(|(channel, pid, payload)| crate::conn::NotificationMsg {
        channel: channel.into(),
        pid,
        payload: payload.into(),
    })
    .collect()
}

/// A JDBC-tap event burst: one `InvoiceService.loadLines` N+1 (eight
/// `SELECT … FROM invoice_lines WHERE invoice_id = ?` in one
/// transaction within ~200ms) plus the requests around it, so the
/// TapMonitor's hotspots / callers / N+1 views all render with
/// real-looking data.
fn tap_events() -> Vec<TapEvent> {
    // Fixed base time so the demo is deterministic (well before
    // "now"; the views key off relative ordering, not wall clock).
    const BASE: u64 = 1_730_000_000_000_000;
    let mut events = vec![query(
        BASE,
        "conn-3",
        None,
        "SELECT id, name, billing_email, currency FROM customers WHERE id = ?",
        290,
        Some("CustomerRepository.findById:41"),
    )];
    events.push(query(
        BASE + 40_000,
        "conn-7",
        Some("conn-7#311"),
        "SELECT id, number, status, total, due_at FROM invoices WHERE customer_id = ? ORDER BY issued_at DESC LIMIT ?",
        1_840,
        Some("InvoiceController.list:58"),
    ));
    // The N+1: eight near-identical selects in one txn, ~25ms apart.
    for i in 0..8u64 {
        events.push(query(
            BASE + 60_000 + i * 25_000,
            "conn-7",
            Some("conn-7#311"),
            "SELECT id, description, quantity, unit_amount, amount FROM invoice_lines WHERE invoice_id = ?",
            420 + i * 15,
            Some("InvoiceService.loadLines:112"),
        ));
    }
    events.push(query(
        BASE + 400_000,
        "conn-2",
        Some("conn-2#90"),
        "UPDATE subscriptions SET status = ?, current_period_end = ? WHERE id = ?",
        2_310,
        Some("DunningJob.advance:77"),
    ));
    events
}

#[allow(clippy::too_many_arguments)]
fn query(
    ts: u64,
    conn: &str,
    txn: Option<&str>,
    sql: &str,
    dur: u64,
    caller: Option<&str>,
) -> TapEvent {
    TapEvent {
        v: 1,
        kind: TapKind::Query,
        ts_unix_micros: ts,
        received_at_unix_micros: ts,
        app: Some("billing-api".into()),
        pool: Some("primary".into()),
        conn: Some(conn.into()),
        txn: txn.map(str::to_string),
        sql: Some(sql.into()),
        params: None,
        params_redacted: false,
        duration_micros: Some(dur),
        rows: Some(1),
        error: None,
        caller: caller.map(|c| vec![c.into()]),
        dropped_events_total: None,
        txn_outcome: None,
    }
}

/// The demo's AWS account and region, as an environment's source.
const RDS_SOURCE: &str = "RDS 210987654321 eu-west-2";

/// The environments panel in `--demo`: synthetic, fixed, no aws CLI.
/// Production has a read replica serving both databases; staging is a
/// single instance a minor release ahead; dev is stopped; a local
/// container runs the newest major with every patch applied.
pub fn environments() -> Vec<crate::inventory::EnvReport> {
    use crate::inventory::{EnvReport, ProbeSettings, Target};
    use crate::mcp::connections::Role;
    const NO_SERVER: &str = "--demo has no server — restart without --demo to connect";
    let rds = |db: &str, server: &str, role: Role, of: Option<&str>, engine: &str| Target {
        database: db.into(),
        server: server.into(),
        role,
        replica_of: of.map(str::to_string),
        host: Some(format!("{server}.c7xk2rpq9w1d.eu-west-2.rds.amazonaws.com")),
        port: Some(5432),
        engine: Some(engine.into()),
        status: Some("available".into()),
        class: Some(
            match server {
                "ledger-prod" | "ledger-prod-replica" => "db.r6g.2xlarge",
                "ledger-staging" => "db.r6g.large",
                _ => "db.t4g.medium",
            }
            .into(),
        ),
        multi_az: Some(server == "ledger-prod"),
        connect: Err(NO_SERVER.into()),
    };
    const PROD_ENGINE: &str = "postgres 15.8 (↑15.12 · 17.4)";
    const STAGING_ENGINE: &str = "postgres 16.4 (↑16.8 · 17.4)";
    let probe = || Some(ProbeSettings::default());
    vec![
        EnvReport {
            name: "production".into(),
            source: RDS_SOURCE.into(),
            targets: vec![
                rds("ledger", "ledger-prod", Role::Primary, None, PROD_ENGINE),
                rds(
                    "ledger",
                    "ledger-prod-replica",
                    Role::Replica,
                    Some("ledger-prod"),
                    PROD_ENGINE,
                ),
                rds("ledgerlog", "ledger-prod", Role::Primary, None, PROD_ENGINE),
                rds(
                    "ledgerlog",
                    "ledger-prod-replica",
                    Role::Replica,
                    Some("ledger-prod"),
                    PROD_ENGINE,
                ),
            ],
            error: None,
            probe: probe(),
        },
        EnvReport {
            name: "staging".into(),
            source: RDS_SOURCE.into(),
            targets: vec![
                rds(
                    "ledger",
                    "ledger-staging",
                    Role::Direct,
                    None,
                    STAGING_ENGINE,
                ),
                rds(
                    "ledgerlog",
                    "ledger-staging",
                    Role::Direct,
                    None,
                    STAGING_ENGINE,
                ),
            ],
            error: None,
            probe: probe(),
        },
        EnvReport {
            name: "dev".into(),
            source: RDS_SOURCE.into(),
            targets: vec![Target {
                status: Some("stopped".into()),
                ..rds("ledger", "ledger-dev", Role::Direct, None, STAGING_ENGINE)
            }],
            error: None,
            probe: None,
        },
        EnvReport {
            name: "local".into(),
            source: "docker".into(),
            targets: vec![Target {
                database: "ledger".into(),
                server: "ledger-db-1".into(),
                role: Role::Direct,
                replica_of: None,
                host: Some("127.0.0.1".into()),
                port: Some(5432),
                engine: Some("postgres 17.2".into()),
                status: Some("running".into()),
                class: None,
                multi_az: None,
                connect: Err(NO_SERVER.into()),
            }],
            error: None,
            probe: probe(),
        },
    ]
}

/// Probe answers in `--demo`, by environment and database: production
/// read through its replica, three patches behind the local container,
/// its ledgerlog a Flyway migration behind staging's.
pub fn environment_probes(
    report: &crate::inventory::EnvReport,
) -> Vec<crate::inventory::probe::Outcome> {
    use crate::inventory::probe::{plan, Step};
    use crate::query::probe::Probe;
    let ledger = |version: &str, schema: i64, pending: usize, sessions: i64, size: &str| Probe {
        server_version: Some(version.into()),
        in_recovery: Some(false),
        schema_version: Some(schema),
        pending: Some(pending),
        sessions: Some(sessions),
        size: Some(size.into()),
        blocked: Some(0),
        long_tx: Some(0),
        ..Default::default()
    };
    let flyway = |version: &str, label: &str, applied: usize, sessions: i64, size: &str| Probe {
        server_version: Some(version.into()),
        in_recovery: Some(false),
        migration_tool: Some("flyway".into()),
        schema_label: Some(label.into()),
        migrations_applied: Some(applied),
        sessions: Some(sessions),
        size: Some(size.into()),
        ..Default::default()
    };
    let replica = |p: Probe| Probe {
        in_recovery: Some(true),
        lag_secs: Some(0.8),
        ..p
    };
    plan(&report.targets)
        .into_iter()
        .zip(&report.targets)
        .map(|(step, t)| match step {
            Step::ViaReplica | Step::NotRunning => None,
            Step::Connect => Some(Ok(match (report.name.as_str(), t.database.as_str()) {
                ("production", "ledger") => replica(Probe {
                    blocked: Some(1),
                    long_tx: Some(1),
                    ..ledger("15.8", 318, 3, 84, "48 GB")
                }),
                ("production", _) => replica(flyway("15.8", "1.42", 42, 9, "7.9 GB")),
                ("staging", "ledger") => ledger("16.4", 318, 3, 23, "6.2 GB"),
                ("staging", _) => flyway("16.4", "1.43", 43, 4, "1.1 GB"),
                _ => ledger("17.2", 321, 0, 3, "1.4 GB"),
            })),
        })
        .collect()
}

/// The slow-queries panel in `--demo`: synthetic `pg_stat_statements`
/// rows over the demo schema, by total time like the live panel.
pub fn slow_queries() -> Vec<crate::query::slow_queries::SlowQueryRow> {
    let row = |query: &str, calls: i64, mean_ms: f64, rows: i64| {
        crate::query::slow_queries::SlowQueryRow {
            query: query.into(),
            calls,
            total_ms: calls as f64 * mean_ms,
            mean_ms,
            rows,
        }
    };
    vec![
        row(
            "UPDATE webhook_deliveries SET delivered_at = now(), status_code = $1 WHERE id = $2",
            2_210_448,
            1.1,
            2_210_448,
        ),
        row(
            "SELECT i.* FROM invoices i JOIN customers c ON c.id = i.customer_id WHERE c.billing_email = $1 ORDER BY i.issued_at DESC",
            61_204,
            38.2,
            734_448,
        ),
        row(
            "SELECT id, description, quantity, unit_amount, amount FROM invoice_lines WHERE invoice_id = $1",
            4_812_330,
            0.42,
            14_436_990,
        ),
        row(
            "SELECT subscription_item_id, sum(quantity) FROM usage_events WHERE occurred_at >= $1 AND occurred_at < $2 GROUP BY subscription_item_id",
            744,
            1_812.6,
            7_348_488,
        ),
        row(
            "REFRESH MATERIALIZED VIEW reporting.revenue_daily",
            30,
            41_220.0,
            0,
        ),
        row(
            "SELECT count(*) FROM webhook_deliveries WHERE delivered_at IS NULL AND created_at < $1",
            8_640,
            96.4,
            8_640,
        ),
    ]
}

/// The sessions panel in `--demo`: the API waiting on a lock an idle
/// transaction holds, the dunning job, the webhook dispatcher, and an
/// autovacuum worker on the busiest table.
pub fn sessions() -> Vec<crate::query::sessions::SessionRow> {
    let s = |pid: i32,
             user: &str,
             app: &str,
             state: &str,
             wait: Option<&str>,
             by: &str,
             query: &str,
             age: f64| {
        crate::query::sessions::SessionRow {
            pid,
            user: user.into(),
            application: app.into(),
            state: state.into(),
            wait_event: wait.map(str::to_string),
            blocked_by: by.into(),
            query: query.into(),
            age_secs: age,
        }
    };
    vec![
        s(
            31_840,
            "billing",
            "billing-api",
            "active",
            Some("Lock:transactionid"),
            "31877",
            "UPDATE invoices SET status = 'paid', paid_at = now() WHERE id = 48207",
            8.2,
        ),
        s(
            31_877,
            "finance_ro",
            "psql",
            "idle in transaction",
            Some("Client:ClientRead"),
            "",
            "SELECT * FROM invoices WHERE customer_id = 2107 FOR UPDATE",
            412.0,
        ),
        s(
            31_907,
            "billing",
            "dunning",
            "active",
            None,
            "",
            "SELECT id, customer_id, total FROM invoices WHERE status = 'open' AND due_at < now() - interval '14 days'",
            3.6,
        ),
        s(
            31_822,
            "billing",
            "webhooks",
            "idle",
            Some("Client:ClientRead"),
            "",
            "UPDATE webhook_deliveries SET delivered_at = now(), status_code = $1 WHERE id = $2",
            0.3,
        ),
        s(
            31_790,
            "billing",
            "billing-api",
            "idle",
            Some("Client:ClientRead"),
            "",
            "COMMIT",
            1.1,
        ),
        s(
            31_655,
            "rdsadmin",
            "",
            "active",
            None,
            "",
            "autovacuum: VACUUM ANALYZE public.webhook_deliveries",
            96.0,
        ),
    ]
}

/// The patch files `--demo`'s plans are made of, newest last:
/// `(id, name, statements)`. The last can't run in a transaction.
const PATCHES: &[(i64, &str, &[&str])] = &[
    (
        319,
        "APP-101-invoice-dunning-stage",
        &[
            "INSERT INTO schema_version (id) VALUES (319)",
            "ALTER TABLE invoices ADD COLUMN dunning_stage smallint NOT NULL DEFAULT 0",
        ],
    ),
    (
        320,
        "APP-102-credit-note-reason-code",
        &[
            "INSERT INTO schema_version (id) VALUES (320)",
            "ALTER TABLE credit_notes ADD COLUMN reason_code text NOT NULL DEFAULT 'other'",
        ],
    ),
    (
        321,
        "APP-104-open-invoices-by-customer-idx",
        &[
            "INSERT INTO schema_version (id) VALUES (321)",
            "CREATE INDEX CONCURRENTLY invoices_open_customer_due_idx ON invoices (customer_id, due_at) WHERE status = 'open'",
        ],
    ),
];

/// A patch plan in `--demo`: the patches the environment hasn't
/// recorded — all three everywhere but the local container, which has
/// them — the last of which can't run in a transaction: left out, or
/// with `outside` planned to run without one. Protected when the
/// environment's name says production.
pub fn patch_plan(
    environment: &str,
    server: &str,
    database: &str,
    outside: bool,
) -> crate::inventory::patching::PatchPlan {
    use crate::inventory::patching::{PatchPlan, PlannedPatch};
    let applied: i64 = if environment == "local" { 321 } else { 318 };
    let patch = |id: i64, name: &str, statements: &[&str]| PlannedPatch {
        file: crate::query::patches::PatchFile {
            file: format!("{id}-{name}.sql"),
            filename_number: Some(id),
            version_id: Some(id),
        },
        text: statements.join("\n"),
        statements: statements.iter().map(|s| s.to_string()).collect(),
        outside: None,
    };
    let (last, rest) = PATCHES.split_last().expect("demo patches");
    let mut patches: Vec<PlannedPatch> = rest
        .iter()
        .filter(|(id, ..)| *id > applied)
        .map(|(id, name, statements)| patch(*id, name, statements))
        .collect();
    let (id, name, statements) = *last;
    let concurrent_pending = id > applied;
    let stopped_at = match (concurrent_pending, outside) {
        (false, _) => None,
        (true, true) => {
            patches.push(PlannedPatch {
                outside: Some("CONCURRENTLY"),
                ..patch(id, name, statements)
            });
            None
        }
        (true, false) => Some((
            format!("{id}-{name}.sql"),
            format!(
                "the patch uses CONCURRENTLY, which can't run in a transaction; {}",
                crate::query::apply::OUTSIDE_HINT
            ),
        )),
    };
    PatchPlan {
        environment: environment.into(),
        server: server.into(),
        database: database.into(),
        protected: environment.to_ascii_lowercase().contains("prod"),
        applied_max: Some(applied),
        patches,
        dir: "~/src/ledger/db/patches".into(),
        recorded: (1..=applied).collect(),
        below_recorded: Vec::new(),
        outside_transaction: outside,
        outside_would_help: concurrent_pending && !outside,
        stopped_at,
    }
}

/// Log errors in `--demo`: synthetic, in RDS's shape, about the demo's
/// own tables and roles.
pub fn log_errors() -> Vec<crate::inventory::rds_logs::LogError> {
    crate::inventory::rds_logs::parse_errors(
        "2026-10-21 06:02:14 UTC:10.20.4.7(51022):billing@ledger:[29412]:ERROR:  duplicate key value violates unique constraint \"payments_provider_ref_key\"\n\
         2026-10-21 07:41:09 UTC:10.20.4.7(51188):billing@ledger:[30005]:ERROR:  duplicate key value violates unique constraint \"payments_provider_ref_key\"\n\
         2026-10-21 08:15:33 UTC:10.20.4.9(40310):billing@ledger:[30577]:ERROR:  deadlock detected\n\
         2026-10-21 09:00:02 UTC:10.20.6.12(35020):ledger_report@ledger:[30981]:ERROR:  canceling statement due to statement timeout\n\
         2026-10-21 09:12:48 UTC:10.20.4.7(51420):billing@ledger:[31104]:ERROR:  duplicate key value violates unique constraint \"payments_provider_ref_key\"\n\
         2026-10-21 09:20:40 UTC:10.20.4.8(48812):billing@ledger:[31240]:ERROR:  insert or update on table \"usage_events_2026_10\" violates foreign key constraint \"usage_events_subscription_item_id_fkey\"\n\
         2026-10-21 09:31:12 UTC:10.20.6.15(40110):[unknown]@[unknown]:[31501]:FATAL:  password authentication failed for user \"finance_ro\"\n",
    )
}

/// The discovery view in `--demo`: proposals from the demo account,
/// production and staging already covered.
pub fn discovery() -> crate::inventory::discover::Found {
    use crate::inventory::discover::{propose, Ec2Host, Found};
    use crate::inventory::rds::RdsInstance;
    let inst =
        |id: &str, source: Option<&str>, replicas: &[&str], db: &str, version: &str| RdsInstance {
            id: id.into(),
            host: Some(format!("{id}.c7xk2rpq9w1d.eu-west-2.rds.amazonaws.com")),
            port: Some(5432),
            engine: "postgres".into(),
            engine_version: version.into(),
            status: if id == "ledger-dev" {
                "stopped".into()
            } else {
                "available".into()
            },
            class: "db.r6g.large".into(),
            multi_az: id == "ledger-prod",
            source: source.map(str::to_string),
            replicas: replicas.iter().map(|r| r.to_string()).collect(),
            db_name: Some(db.into()),
            vpc_id: Some("vpc-0a1b2c3d4e5f60718".into()),
            upgrades: None,
            cluster: None,
        };
    let instances = vec![
        inst(
            "ledger-prod",
            None,
            &["ledger-prod-replica"],
            "ledger",
            "15.8",
        ),
        inst(
            "ledger-prod-replica",
            Some("ledger-prod"),
            &[],
            "ledger",
            "15.8",
        ),
        inst("ledger-staging", None, &[], "ledger", "16.4"),
        inst("ledger-dev", None, &[], "ledger", "16.4"),
        inst(
            "analytics",
            None,
            &["analytics-replica"],
            "warehouse",
            "16.4",
        ),
        inst(
            "analytics-replica",
            Some("analytics"),
            &[],
            "warehouse",
            "16.4",
        ),
    ];
    let secrets: Vec<String> = [
        "ledger-prod/database/ledger/credentials",
        "ledger-prod/database/ledger_replica/credentials",
        "ledger-staging/database/ledger/credentials",
        "ledger-dev/database/ledger/credentials",
        "analytics/database/warehouse/credentials",
        "analytics/database/warehouse_replica/credentials",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let hosts = vec![Ec2Host {
        id: "i-0f3a9c1e7b2d45680".into(),
        name: Some("bastion".into()),
        vpc_id: Some("vpc-0a1b2c3d4e5f60718".into()),
        public_dns: "bastion.eu-west-2.compute.amazonaws.com".into(),
    }];
    let scope = crate::aws_cli::AwsScope {
        region: Some("eu-west-2".into()),
        profile: None,
        account: Some("210987654321".into()),
    };
    let existing = crate::inventory::parse_config(
        "[[environments]]\nname = \"production\"\nsource = { kind = \"rds\", instances = [\"ledger-prod*\"] }\n\
         [[environments]]\nname = \"staging\"\nsource = { kind = \"rds\", instances = [\"ledger-staging\"] }\n",
    )
    .unwrap_or_default();
    Found {
        header: "AWS account 210987654321, eu-west-2".into(),
        warnings: Vec::new(),
        proposals: propose(&instances, &secrets, &hosts, &scope, &existing),
        scopes: vec![
            crate::aws_cli::AwsScope::default(),
            crate::aws_cli::AwsScope {
                profile: Some("production".into()),
                ..crate::aws_cli::AwsScope::default()
            },
        ],
    }
}

#[cfg(test)]
mod tests;
