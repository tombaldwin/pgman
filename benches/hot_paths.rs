//! Benchmarks for the hot paths the operator hits every keystroke:
//! the syntax highlighter, completion candidate generation, and
//! grid filtering. Run with `cargo bench`; reports land under
//! `target/criterion/`.
//!
//! These exist to catch perf regressions during refactors — when
//! a number doubles in a PR, ask why before merging.

use criterion::{criterion_group, criterion_main, Criterion};
use pgman::app::compute_visible_rows;
use pgman::query::complete::candidates_for;
use pgman::query::highlight::{classify, tokenize};
use pgman::query::schema::{SchemaCache, TableMeta};
use std::hint::black_box;

/// A realistic-ish ~1 KB buffer: a CTE + a multi-table join + a
/// WHERE clause. Mirrors what an operator typically has on screen.
fn sample_buffer() -> String {
    [
        "WITH recent_orders AS (",
        "  SELECT o.id, o.user_id, o.total",
        "  FROM orders o",
        "  WHERE o.created_at >= now() - interval '7 days'",
        "    AND o.status = 'paid'",
        ")",
        "SELECT u.id, u.email, COUNT(ro.id) AS order_count,",
        "       SUM(ro.total) AS revenue",
        "FROM users u",
        "LEFT JOIN recent_orders ro ON ro.user_id = u.id",
        "WHERE u.email ILIKE '%@example.com'",
        "GROUP BY u.id, u.email",
        "ORDER BY revenue DESC NULLS LAST",
        "LIMIT 50;",
    ]
    .join("\n")
}

/// A cache the size of a real enterprise app's schema — hundreds of
/// tables, thousands of columns. This is what the highlighter and
/// completion have to walk on every keystroke.
fn big_cache() -> SchemaCache {
    let mut cache = SchemaCache::default();
    cache.schemas.push("public".into());
    for i in 0..500 {
        let name = format!("table_{i}");
        cache.tables.push(TableMeta {
            schema: "public".into(),
            name: name.clone(),
        });
        let cols: Vec<String> = (0..20).map(|j| format!("col_{j}")).collect();
        cache.columns_by_table.insert(("public".into(), name), cols);
    }
    cache
}

fn bench_highlight(c: &mut Criterion) {
    let buf = sample_buffer();
    c.bench_function("highlight::tokenize ~1KB buffer", |b| {
        b.iter(|| {
            let spans = tokenize(black_box(&buf));
            black_box(spans.len());
        })
    });

    let cache = big_cache();
    c.bench_function("highlight::classify against 500-table cache", |b| {
        b.iter(|| {
            let spans = tokenize(&buf);
            let resolved = classify(spans, &buf, black_box(&cache), &[], &[]);
            black_box(resolved.len());
        })
    });
}

fn bench_complete(c: &mut Criterion) {
    let cache = big_cache();
    // Cursor right after a prefix that prompts a typical Tab — the
    // operator typed `SELECT * FROM tab` and hits Tab.
    let buf = "SELECT * FROM tab";
    let cursor = buf.len();
    c.bench_function("complete::candidates_for FROM tab|", |b| {
        b.iter(|| {
            let cands = candidates_for(black_box(buf), cursor, black_box(&cache));
            black_box(cands.len());
        })
    });
}

fn bench_filter(c: &mut Criterion) {
    // 1000-row grid (the renderer's MAX_ROWS cap) with a typical
    // mix of strings and numbers. compute_visible_rows runs on every
    // filter keystroke; this is the hot path.
    let rows: Vec<Vec<String>> = (0..1000)
        .map(|i| {
            vec![
                i.to_string(),
                format!("user_{i}"),
                format!("user_{i}@example.com"),
                if i % 7 == 0 { "active" } else { "inactive" }.into(),
            ]
        })
        .collect();
    c.bench_function("compute_visible_rows 1000 × 4-col, narrow filter", |b| {
        b.iter(|| {
            let visible = compute_visible_rows(black_box(&rows), Some("user_42"));
            black_box(visible.len());
        })
    });
    c.bench_function("compute_visible_rows 1000 × 4-col, broad filter", |b| {
        b.iter(|| {
            let visible = compute_visible_rows(black_box(&rows), Some("user"));
            black_box(visible.len());
        })
    });
}

criterion_group!(benches, bench_highlight, bench_complete, bench_filter);
criterion_main!(benches);
