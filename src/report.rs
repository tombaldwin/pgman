//! `\report` command — dump pgman's current advisor +
//! tap insights to a shareable artifact (Markdown by
//! default, HTML when the path ends in `.html`/`.htm`).
//!
//! The flow: operator types `\report` (or `\report <path>`)
//! in the editor; pgman snapshots the relevant App state
//! into a [`ReportSnapshot`], renders it via
//! [`render_markdown`] or [`render_html`], and writes the
//! result via `tui_common::util::write_atomic`. Designed for
//! pasting into PR descriptions, attaching to DBA handoffs,
//! or saving alongside a deploy for later "what was the
//! state before this?" comparisons.
//!
//! All rendering is pure on [`ReportSnapshot`] so callers
//! and tests can construct one without an App instance.

use crate::query::lint::Finding as LintFinding;
use crate::tap::{CallerStats, Hotspot, HotspotDiff, NplusOneFinding, TxnStats};

/// All of the App state the report renders. Built by
/// [`ReportSnapshot::from_app`] in the dispatcher; the pure
/// renderers take this so tests can construct fixture
/// snapshots without standing up an App.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportSnapshot {
    /// Header text — typically `"pgman report"`. Lets future
    /// callers (e.g. CI integration) customise.
    pub title: String,
    /// Human-readable timestamp for the report header. Caller
    /// formats this so the report is wall-clock-stable across
    /// regenerations of the same fixture.
    pub generated_at: String,
    /// Redacted DSN (or other connection hint) for context.
    /// `None` when pgman wasn't connected at report time.
    pub connection: Option<String>,
    /// Schema-lint findings already loaded into the App
    /// (LINT001-106). Sorted as the App stored them.
    pub lint_findings: Vec<LintFinding>,
    /// Current ring-derived tap insights. Empty vecs render as
    /// "(no entries)" placeholders — sections never disappear
    /// because their absence is itself useful signal.
    pub hotspots: Vec<Hotspot>,
    pub callers: Vec<CallerStats>,
    pub transactions: Vec<TxnStats>,
    pub nplus1: Vec<NplusOneFinding>,
    /// Diff vs the captured baseline. `None` when no baseline
    /// has been captured; renders as a "no baseline captured"
    /// note rather than being omitted.
    pub baseline_diff: Option<Vec<HotspotDiff>>,
    /// `tap::dropped_at_listener()` snapshot at report time.
    /// Non-zero means the figures elsewhere in this report are
    /// a subsample of the real workload. Surfaced in the
    /// summary block with a visible warning so a reader of the
    /// shared artifact understands they're not seeing the
    /// full picture.
    pub listener_dropped: u64,
    /// JAR-side cumulative drop count (most recent heartbeat).
    /// Independent of `listener_dropped`: the JAR drops events
    /// before they reach pgman; the listener drops them after.
    pub jar_dropped: u64,
}

/// One-shot summary of the snapshot for the header block.
/// Pure derivative of `ReportSnapshot` — exposed so both
/// renderers share the exact same arithmetic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryStats {
    /// Sum of `count` across all hotspots — i.e. total
    /// **query** events observed in the ring. Heartbeat /
    /// txn-boundary events are not counted (they don't carry
    /// SQL and don't belong to a hotspot bucket).
    pub total_query_events: usize,
    /// Distinct SQL fingerprints (one per hotspot bucket).
    pub unique_fingerprints: usize,
    /// Listener-side drop count carried over from the
    /// snapshot — non-zero means the figures elsewhere are a
    /// subsample.
    pub listener_dropped: u64,
    /// JAR-side drop count from the most recent heartbeat —
    /// dropped before reaching pgman at all.
    pub jar_dropped: u64,
    pub nplus1_count: usize,
    pub lint_count: usize,
    pub txn_count: usize,
    pub txn_open_count: usize,
    /// Pre-rendered baseline label (`"none captured"` /
    /// `"N changed fingerprint(s)"` / `"no changes vs
    /// baseline"`). Keeps the renderers identical without
    /// pushing logic into each one.
    pub baseline_label: String,
}

/// Pure: derive a [`SummaryStats`] from a [`ReportSnapshot`].
/// Stable enough to test against fixture snapshots.
pub fn summary_stats(snapshot: &ReportSnapshot) -> SummaryStats {
    let total_query_events: usize = snapshot.hotspots.iter().map(|h| h.count).sum();
    let baseline_label = match &snapshot.baseline_diff {
        None => "none captured".to_string(),
        Some(diff) if diff.is_empty() => "no changes vs baseline".to_string(),
        Some(diff) => format!("{} changed fingerprint(s)", diff.len()),
    };
    let txn_open_count = snapshot
        .transactions
        .iter()
        .filter(|t| t.outcome.is_none())
        .count();
    SummaryStats {
        total_query_events,
        unique_fingerprints: snapshot.hotspots.len(),
        listener_dropped: snapshot.listener_dropped,
        jar_dropped: snapshot.jar_dropped,
        nplus1_count: snapshot.nplus1.len(),
        lint_count: snapshot.lint_findings.len(),
        txn_count: snapshot.transactions.len(),
        txn_open_count,
        baseline_label,
    }
}

/// Choose a report format from a path's extension. `.html`
/// / `.htm` → HTML; anything else (including no extension)
/// → Markdown. The default is Markdown because it pastes
/// cleanly into PRs / issues / Slack.
pub fn format_for_path(path: &std::path::Path) -> ReportFormat {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html") | Some("htm") => ReportFormat::Html,
        _ => ReportFormat::Markdown,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    Markdown,
    Html,
}

/// Render `snapshot` as Markdown. Sections always appear in
/// the same order so a diff of two reports surfaces the
/// changes cleanly.
pub fn render_markdown(snapshot: &ReportSnapshot) -> String {
    let mut out = String::with_capacity(4096);
    out.push_str(&format!("# {}\n\n", snapshot.title));
    out.push_str(&format!("_generated at {}_\n\n", snapshot.generated_at));
    if let Some(conn) = &snapshot.connection {
        out.push_str(&format!("**Connection:** `{conn}`\n\n"));
    }
    // Summary block — gives the reader the gist before they
    // scroll. Always present; counts may all be zero on a
    // clean run, which is itself useful signal.
    let summary = summary_stats(snapshot);
    out.push_str("## Summary\n\n");
    out.push_str(&format!(
        "- **{}** query events across **{}** SQL fingerprints\n",
        summary.total_query_events, summary.unique_fingerprints
    ));
    out.push_str(&format!(
        "- **{}** N+1 finding(s) · **{}** lint finding(s)\n",
        summary.nplus1_count, summary.lint_count
    ));
    out.push_str(&format!(
        "- **{}** transaction(s) observed (**{}** still open)\n",
        summary.txn_count, summary.txn_open_count
    ));
    out.push_str(&format!(
        "- baseline: {}\n",
        summary.baseline_label
    ));
    if summary.listener_dropped > 0 || summary.jar_dropped > 0 {
        out.push_str(&format!(
            "- ⚠ **{} listener drop(s)** · **{} JAR drop(s)** — figures below are a subsample of the real workload\n",
            summary.listener_dropped, summary.jar_dropped
        ));
    }
    out.push_str("\n---\n\n");

    // Schema lint findings.
    out.push_str(&format!(
        "## Schema lint findings ({n})\n\n",
        n = snapshot.lint_findings.len()
    ));
    if snapshot.lint_findings.is_empty() {
        out.push_str("_no lint findings — schema looks clean._\n\n");
    } else {
        out.push_str("| Severity | Code | Object | Title |\n");
        out.push_str("|---|---|---|---|\n");
        for f in &snapshot.lint_findings {
            out.push_str(&format!(
                "| {sev:?} | {code} | {obj} | {title} |\n",
                sev = f.severity,
                code = md_escape(&f.code),
                obj = md_escape(&f.object),
                title = md_escape(&f.title),
            ));
        }
        out.push('\n');
    }

    // Tap hotspots.
    out.push_str(&format!(
        "## Tap hotspots ({n} fingerprints)\n\n",
        n = snapshot.hotspots.len()
    ));
    if snapshot.hotspots.is_empty() {
        out.push_str("_no tap events yet — start with `--tap-otlp :4318` or wait for the pgman-tap JAR._\n\n");
    } else {
        out.push_str("| Calls | Errors | p50 (µs) | p95 (µs) | p99 (µs) | Fingerprint |\n");
        out.push_str("|---:|---:|---:|---:|---:|---|\n");
        for h in &snapshot.hotspots {
            out.push_str(&format!(
                "| {c} | {e} | {p50} | {p95} | {p99} | {fp} |\n",
                c = h.count,
                e = h.error_count,
                p50 = h.p50_micros,
                p95 = h.p95_micros,
                p99 = h.p99_micros,
                fp = md_escape(&h.fingerprint),
            ));
        }
        out.push('\n');
    }

    // Per-caller rollup.
    out.push_str(&format!(
        "## Per-caller rollup ({n} app frames)\n\n",
        n = snapshot.callers.len()
    ));
    if snapshot.callers.is_empty() {
        out.push_str("_no caller frames — the JAR may have stack capture disabled._\n\n");
    } else {
        out.push_str("| Calls | Errors | Distinct SQL | Total (µs) | Caller |\n");
        out.push_str("|---:|---:|---:|---:|---|\n");
        for c in &snapshot.callers {
            out.push_str(&format!(
                "| {n} | {e} | {fps} | {total} | {caller} |\n",
                n = c.count,
                e = c.error_count,
                fps = c.distinct_fingerprints,
                total = c.total_micros,
                caller = md_escape(&c.caller),
            ));
        }
        out.push('\n');
    }

    // Transactions.
    out.push_str(&format!(
        "## Transactions ({n} observed)\n\n",
        n = snapshot.transactions.len()
    ));
    if snapshot.transactions.is_empty() {
        out.push_str("_no transactions observed yet._\n\n");
    } else {
        out.push_str("| State | Stmts | SQL shapes | Span (µs) | DB time (µs) | Pool | Txn / Conn |\n");
        out.push_str("|---|---:|---:|---:|---:|---|---|\n");
        for t in &snapshot.transactions {
            let state = match t.outcome {
                None => "open",
                Some(crate::tap::TxnOutcome::Commit) => "commit",
                Some(crate::tap::TxnOutcome::Rollback) => "rollback",
            };
            let id = match (t.txn.as_deref(), t.conn.as_deref()) {
                (Some(txn), _) => txn.to_string(),
                (None, Some(conn)) => format!("(autocommit) {conn}"),
                _ => "?".into(),
            };
            out.push_str(&format!(
                "| {state} | {stmts} | {fps} | {span} | {dbt} | {pool} | {id} |\n",
                stmts = t.statement_count,
                fps = t.distinct_fingerprints,
                span = t.span_micros,
                dbt = t.total_query_micros,
                pool = md_escape(t.pool.as_deref().unwrap_or("—")),
                id = md_escape(&id),
            ));
        }
        out.push('\n');
    }

    // N+1 findings.
    out.push_str(&format!(
        "## N+1 findings ({n})\n\n",
        n = snapshot.nplus1.len()
    ));
    if snapshot.nplus1.is_empty() {
        out.push_str("_no N+1 bursts detected._\n\n");
    } else {
        out.push_str("| Calls | Span (µs) | Caller | Fingerprint |\n");
        out.push_str("|---:|---:|---|---|\n");
        for f in &snapshot.nplus1 {
            out.push_str(&format!(
                "| {c} | {span} | {caller} | {fp} |\n",
                c = f.count,
                span = f.span_micros,
                caller = md_escape(f.last_caller.as_deref().unwrap_or("?")),
                fp = md_escape(&f.fingerprint),
            ));
        }
        out.push('\n');
    }

    // Baseline diff.
    out.push_str("## Baseline diff\n\n");
    match &snapshot.baseline_diff {
        None => {
            out.push_str("_no baseline captured — press Shift-B in the TapMonitor to set one._\n\n");
        }
        Some(diff) if diff.is_empty() => {
            out.push_str("_no changes since baseline — nothing new, no regressions, no disappearances._\n\n");
        }
        Some(diff) => {
            out.push_str("| Change | Calls (now) | p95 now (µs) | p95 baseline (µs) | Fingerprint |\n");
            out.push_str("|---|---:|---:|---:|---|\n");
            for d in diff {
                let label = match d.kind {
                    crate::tap::DiffKind::Regressed => "regressed",
                    crate::tap::DiffKind::New => "new",
                    crate::tap::DiffKind::Disappeared => "disappeared",
                    crate::tap::DiffKind::Unchanged => "unchanged",
                };
                out.push_str(&format!(
                    "| {label} | {c} | {p95_now} | {p95_base} | {fp} |\n",
                    c = d.current_count,
                    p95_now = d.current_p95_micros,
                    p95_base = d.baseline_p95_micros,
                    fp = md_escape(&d.fingerprint),
                ));
            }
            out.push('\n');
        }
    }

    out.push_str("---\n");
    out.push_str("_generated by pgman_\n");
    out
}

/// Render `snapshot` as a minimal self-contained HTML
/// document. Same sections + ordering as the Markdown
/// renderer; the styling is deliberately tiny so the file
/// stays under a few KB and renders cleanly in any browser.
pub fn render_html(snapshot: &ReportSnapshot) -> String {
    let mut out = String::with_capacity(4096);
    out.push_str("<!doctype html><html><head><meta charset=\"utf-8\">");
    out.push_str("<title>");
    out.push_str(&html_escape(&snapshot.title));
    out.push_str("</title>");
    out.push_str("<style>body{font-family:system-ui,sans-serif;margin:2rem;max-width:80rem}");
    out.push_str("table{border-collapse:collapse;margin:1em 0}");
    out.push_str("th,td{border:1px solid #ccc;padding:0.25rem 0.5rem;text-align:left}");
    out.push_str("th{background:#f3f3f3}");
    out.push_str("code{background:#f7f7f7;padding:0 0.2rem}");
    out.push_str(".muted{color:#666;font-style:italic}");
    out.push_str("</style></head><body>");
    out.push_str(&format!(
        "<h1>{}</h1>",
        html_escape(&snapshot.title)
    ));
    out.push_str(&format!(
        "<p class=\"muted\">generated at {}</p>",
        html_escape(&snapshot.generated_at)
    ));
    if let Some(conn) = &snapshot.connection {
        out.push_str(&format!(
            "<p><strong>Connection:</strong> <code>{}</code></p>",
            html_escape(conn)
        ));
    }

    // Summary block — mirrors the Markdown version so a
    // reader gets the gist before scrolling.
    let summary = summary_stats(snapshot);
    out.push_str("<h2>Summary</h2><ul>");
    out.push_str(&format!(
        "<li><strong>{}</strong> query events across <strong>{}</strong> SQL fingerprints</li>",
        summary.total_query_events, summary.unique_fingerprints
    ));
    out.push_str(&format!(
        "<li><strong>{}</strong> N+1 finding(s) · <strong>{}</strong> lint finding(s)</li>",
        summary.nplus1_count, summary.lint_count
    ));
    out.push_str(&format!(
        "<li><strong>{}</strong> transaction(s) observed (<strong>{}</strong> still open)</li>",
        summary.txn_count, summary.txn_open_count
    ));
    out.push_str(&format!(
        "<li>baseline: {}</li>",
        html_escape(&summary.baseline_label)
    ));
    if summary.listener_dropped > 0 || summary.jar_dropped > 0 {
        out.push_str(&format!(
            "<li>⚠ <strong>{} listener drop(s)</strong> · <strong>{} JAR drop(s)</strong> — figures below are a subsample of the real workload</li>",
            summary.listener_dropped, summary.jar_dropped
        ));
    }
    out.push_str("</ul>");

    // Schema lint findings.
    out.push_str(&format!(
        "<h2>Schema lint findings ({n})</h2>",
        n = snapshot.lint_findings.len()
    ));
    if snapshot.lint_findings.is_empty() {
        out.push_str("<p class=\"muted\">no lint findings — schema looks clean.</p>");
    } else {
        out.push_str("<table><tr><th>Severity</th><th>Code</th><th>Object</th><th>Title</th></tr>");
        for f in &snapshot.lint_findings {
            out.push_str(&format!(
                "<tr><td>{sev:?}</td><td>{code}</td><td>{obj}</td><td>{title}</td></tr>",
                sev = f.severity,
                code = html_escape(&f.code),
                obj = html_escape(&f.object),
                title = html_escape(&f.title),
            ));
        }
        out.push_str("</table>");
    }

    // Tap hotspots.
    out.push_str(&format!(
        "<h2>Tap hotspots ({n} fingerprints)</h2>",
        n = snapshot.hotspots.len()
    ));
    if snapshot.hotspots.is_empty() {
        out.push_str("<p class=\"muted\">no tap events yet.</p>");
    } else {
        out.push_str(
            "<table><tr><th>Calls</th><th>Errors</th><th>p50 (µs)</th><th>p95 (µs)</th><th>p99 (µs)</th><th>Fingerprint</th></tr>",
        );
        for h in &snapshot.hotspots {
            out.push_str(&format!(
                "<tr><td>{c}</td><td>{e}</td><td>{p50}</td><td>{p95}</td><td>{p99}</td><td>{fp}</td></tr>",
                c = h.count,
                e = h.error_count,
                p50 = h.p50_micros,
                p95 = h.p95_micros,
                p99 = h.p99_micros,
                fp = html_escape(&h.fingerprint),
            ));
        }
        out.push_str("</table>");
    }

    // Per-caller rollup.
    out.push_str(&format!(
        "<h2>Per-caller rollup ({n} app frames)</h2>",
        n = snapshot.callers.len()
    ));
    if snapshot.callers.is_empty() {
        out.push_str("<p class=\"muted\">no caller frames — the JAR may have stack capture disabled.</p>");
    } else {
        out.push_str(
            "<table><tr><th>Calls</th><th>Errors</th><th>Distinct SQL</th><th>Total (µs)</th><th>Caller</th></tr>",
        );
        for c in &snapshot.callers {
            out.push_str(&format!(
                "<tr><td>{n}</td><td>{e}</td><td>{fps}</td><td>{total}</td><td>{caller}</td></tr>",
                n = c.count,
                e = c.error_count,
                fps = c.distinct_fingerprints,
                total = c.total_micros,
                caller = html_escape(&c.caller),
            ));
        }
        out.push_str("</table>");
    }

    // Transactions.
    out.push_str(&format!(
        "<h2>Transactions ({n} observed)</h2>",
        n = snapshot.transactions.len()
    ));
    if snapshot.transactions.is_empty() {
        out.push_str("<p class=\"muted\">no transactions observed yet.</p>");
    } else {
        out.push_str(
            "<table><tr><th>State</th><th>Stmts</th><th>SQL shapes</th><th>Span (µs)</th><th>DB time (µs)</th><th>Pool</th><th>Txn / Conn</th></tr>",
        );
        for t in &snapshot.transactions {
            let state = match t.outcome {
                None => "open",
                Some(crate::tap::TxnOutcome::Commit) => "commit",
                Some(crate::tap::TxnOutcome::Rollback) => "rollback",
            };
            let id = match (t.txn.as_deref(), t.conn.as_deref()) {
                (Some(txn), _) => txn.to_string(),
                (None, Some(conn)) => format!("(autocommit) {conn}"),
                _ => "?".into(),
            };
            out.push_str(&format!(
                "<tr><td>{state}</td><td>{stmts}</td><td>{fps}</td><td>{span}</td><td>{dbt}</td><td>{pool}</td><td>{id}</td></tr>",
                stmts = t.statement_count,
                fps = t.distinct_fingerprints,
                span = t.span_micros,
                dbt = t.total_query_micros,
                pool = html_escape(t.pool.as_deref().unwrap_or("—")),
                id = html_escape(&id),
            ));
        }
        out.push_str("</table>");
    }

    // N+1 findings (the high-leverage section operators want
    // to share most often).
    out.push_str(&format!(
        "<h2>N+1 findings ({n})</h2>",
        n = snapshot.nplus1.len()
    ));
    if snapshot.nplus1.is_empty() {
        out.push_str("<p class=\"muted\">no N+1 bursts detected.</p>");
    } else {
        out.push_str(
            "<table><tr><th>Calls</th><th>Span (µs)</th><th>Caller</th><th>Fingerprint</th></tr>",
        );
        for f in &snapshot.nplus1 {
            out.push_str(&format!(
                "<tr><td>{c}</td><td>{span}</td><td>{caller}</td><td>{fp}</td></tr>",
                c = f.count,
                span = f.span_micros,
                caller = html_escape(f.last_caller.as_deref().unwrap_or("?")),
                fp = html_escape(&f.fingerprint),
            ));
        }
        out.push_str("</table>");
    }

    // Baseline diff.
    out.push_str("<h2>Baseline diff</h2>");
    match &snapshot.baseline_diff {
        None => {
            out.push_str("<p class=\"muted\">no baseline captured — press Shift-B in the TapMonitor to set one.</p>");
        }
        Some(diff) if diff.is_empty() => {
            out.push_str(
                "<p class=\"muted\">no changes since baseline — nothing new, no regressions, no disappearances.</p>",
            );
        }
        Some(diff) => {
            out.push_str(
                "<table><tr><th>Change</th><th>Calls (now)</th><th>p95 now (µs)</th><th>p95 baseline (µs)</th><th>Fingerprint</th></tr>",
            );
            for d in diff {
                let label = match d.kind {
                    crate::tap::DiffKind::Regressed => "regressed",
                    crate::tap::DiffKind::New => "new",
                    crate::tap::DiffKind::Disappeared => "disappeared",
                    crate::tap::DiffKind::Unchanged => "unchanged",
                };
                out.push_str(&format!(
                    "<tr><td>{label}</td><td>{c}</td><td>{p95_now}</td><td>{p95_base}</td><td>{fp}</td></tr>",
                    c = d.current_count,
                    p95_now = d.current_p95_micros,
                    p95_base = d.baseline_p95_micros,
                    fp = html_escape(&d.fingerprint),
                ));
            }
            out.push_str("</table>");
        }
    }

    out.push_str("<hr><p class=\"muted\">generated by pgman</p>");
    out.push_str("</body></html>");
    out
}

/// Markdown-escape a cell value. The renderer cares about
/// pipes + backticks (Markdown table syntax) AND about
/// `<`/`>`/`&` because most Markdown renderers (GitHub /
/// GitLab / VS Code preview) pass raw HTML through. Tap
/// data flows through this from a JVM we don't control —
/// so a SQL fingerprint or caller frame containing
/// `<script>` would otherwise render live. Escape
/// defensively rather than trusting the downstream renderer.
fn md_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '|' => out.push_str("\\|"),
            '`' => out.push_str("\\`"),
            // HTML-passthrough renderers would happily run
            // <script> from a cell. Substitute the HTML
            // entities — well-rendered everywhere we care
            // about, and inert in plain-Markdown viewers.
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '\n' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

/// HTML-escape a cell value. Full `<`/`>`/`&`/`"`/`'`
/// substitution set — `'` matters when future templates
/// might end up inside single-quoted attributes.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::lint::{Finding as LintFinding, Severity};

    fn empty_snapshot() -> ReportSnapshot {
        ReportSnapshot {
            title: "pgman report".into(),
            generated_at: "2026-05-28T12:00:00Z".into(),
            connection: Some("postgres://app@db/billing".into()),
            lint_findings: Vec::new(),
            hotspots: Vec::new(),
            callers: Vec::new(),
            transactions: Vec::new(),
            nplus1: Vec::new(),
            baseline_diff: None,
            listener_dropped: 0,
            jar_dropped: 0,
        }
    }

    #[test]
    fn report_summary_surfaces_listener_drop_warning() {
        let mut s = empty_snapshot();
        s.listener_dropped = 42;
        let md = render_markdown(&s);
        assert!(
            md.contains("42 listener drop(s)"),
            "markdown should warn about listener drops; got:\n{md}"
        );
        assert!(
            md.contains("subsample"),
            "warning should explicitly say 'subsample':\n{md}"
        );
        let html = render_html(&s);
        assert!(
            html.contains("42 listener drop(s)"),
            "html should warn about listener drops; got:\n{html}"
        );
    }

    #[test]
    fn report_summary_omits_drop_warning_when_both_zero() {
        let s = empty_snapshot();
        let md = render_markdown(&s);
        assert!(
            !md.contains("subsample"),
            "no-drop case must not show the warning:\n{md}"
        );
    }

    #[test]
    fn format_for_path_picks_html_for_html_extensions() {
        let p = std::path::PathBuf::from("/tmp/report.html");
        assert_eq!(format_for_path(&p), ReportFormat::Html);
        let p = std::path::PathBuf::from("/tmp/report.HTM");
        assert_eq!(format_for_path(&p), ReportFormat::Html);
    }

    #[test]
    fn format_for_path_defaults_to_markdown() {
        let p = std::path::PathBuf::from("/tmp/report.md");
        assert_eq!(format_for_path(&p), ReportFormat::Markdown);
        let p = std::path::PathBuf::from("/tmp/report");
        assert_eq!(format_for_path(&p), ReportFormat::Markdown);
        let p = std::path::PathBuf::from("/tmp/report.txt");
        assert_eq!(format_for_path(&p), ReportFormat::Markdown);
    }

    #[test]
    fn render_markdown_includes_every_section_even_when_empty() {
        let s = empty_snapshot();
        let md = render_markdown(&s);
        assert!(md.contains("# pgman report"));
        assert!(md.contains("Schema lint findings"));
        assert!(md.contains("Tap hotspots"));
        assert!(md.contains("Per-caller rollup"));
        assert!(md.contains("Transactions"));
        assert!(md.contains("N+1 findings"));
        assert!(md.contains("Baseline diff"));
        // Connection banner present.
        assert!(md.contains("Connection:"));
        // Empty-section placeholders rendered.
        assert!(md.contains("no lint findings"));
        assert!(md.contains("no N+1 bursts detected"));
        assert!(md.contains("no baseline captured"));
    }

    #[test]
    fn render_markdown_includes_lint_findings_in_table() {
        let mut s = empty_snapshot();
        s.lint_findings.push(LintFinding {
            severity: Severity::High,
            code: "LINT001".into(),
            title: "missing primary key".into(),
            object: "public.orders".into(),
            detail: String::new(),
            suggestion: None,
        });
        let md = render_markdown(&s);
        assert!(md.contains("| High | LINT001 | public.orders | missing primary key |"));
    }

    #[test]
    fn render_markdown_includes_hotspots_in_table() {
        let mut s = empty_snapshot();
        s.hotspots.push(Hotspot {
            fingerprint: "select * from t".into(),
            example_sql: "select * from t".into(),
            count: 42,
            error_count: 2,
            total_micros: 4200,
            p50_micros: 90,
            p95_micros: 100,
            p99_micros: 110,
            distinct_callers: 0,
            last_caller: None,
            last_app: None,
        });
        let md = render_markdown(&s);
        assert!(
            md.contains("| 42 | 2 | 90 | 100 | 110 | select * from t |"),
            "hotspot row missing; got:\n{md}"
        );
    }

    #[test]
    fn render_markdown_baseline_diff_distinguishes_no_baseline_from_no_changes() {
        let mut s = empty_snapshot();
        s.baseline_diff = Some(Vec::new());
        let md = render_markdown(&s);
        assert!(md.contains("no changes since baseline"));
        s.baseline_diff = None;
        let md = render_markdown(&s);
        assert!(md.contains("no baseline captured"));
    }

    #[test]
    fn render_html_emits_a_self_contained_document() {
        let s = empty_snapshot();
        let html = render_html(&s);
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<title>pgman report</title>"));
        assert!(html.contains("<style>"), "must include inline style for portability");
        assert!(html.ends_with("</html>"));
    }

    #[test]
    fn render_html_html_escapes_user_content() {
        let mut s = empty_snapshot();
        s.lint_findings.push(LintFinding {
            severity: Severity::High,
            code: "<script>".into(),
            title: "x & y".into(),
            object: "\"foo\"".into(),
            detail: String::new(),
            suggestion: None,
        });
        let html = render_html(&s);
        assert!(html.contains("&lt;script&gt;"), "code field must be escaped");
        assert!(html.contains("x &amp; y"), "amp must be escaped");
        assert!(
            html.contains("&quot;foo&quot;"),
            "double-quote must be escaped"
        );
        assert!(!html.contains("<script>"), "raw tag must NOT leak through");
    }

    #[test]
    fn summary_stats_aggregates_event_count_and_baseline_label() {
        let mut s = empty_snapshot();
        s.hotspots.push(Hotspot {
            fingerprint: "a".into(),
            example_sql: "a".into(),
            count: 10,
            error_count: 0,
            total_micros: 0,
            p50_micros: 0,
            p95_micros: 0,
            p99_micros: 0,
            distinct_callers: 0,
            last_caller: None,
            last_app: None,
        });
        s.hotspots.push(Hotspot {
            fingerprint: "b".into(),
            example_sql: "b".into(),
            count: 5,
            error_count: 0,
            total_micros: 0,
            p50_micros: 0,
            p95_micros: 0,
            p99_micros: 0,
            distinct_callers: 0,
            last_caller: None,
            last_app: None,
        });
        let stats = summary_stats(&s);
        assert_eq!(stats.total_query_events, 15);
        assert_eq!(stats.unique_fingerprints, 2);
        assert_eq!(stats.baseline_label, "none captured");
        // Baseline captured but no changes.
        s.baseline_diff = Some(Vec::new());
        assert_eq!(summary_stats(&s).baseline_label, "no changes vs baseline");
        // Baseline captured with one change.
        s.baseline_diff = Some(vec![HotspotDiff {
            fingerprint: "x".into(),
            example_sql: "x".into(),
            kind: crate::tap::DiffKind::New,
            baseline_count: 0,
            baseline_p95_micros: 0,
            current_count: 3,
            current_p95_micros: 100,
        }]);
        assert_eq!(
            summary_stats(&s).baseline_label,
            "1 changed fingerprint(s)"
        );
    }

    #[test]
    fn summary_stats_counts_open_transactions() {
        let mut s = empty_snapshot();
        s.transactions.push(crate::tap::TxnStats {
            txn: Some("a".into()),
            conn: None,
            app: None,
            pool: None,
            statement_count: 1,
            error_count: 0,
            distinct_fingerprints: 1,
            last_fingerprint: None,
            first_ts_unix_micros: 0,
            last_ts_unix_micros: 0,
            span_micros: 0,
            total_query_micros: 0,
            outcome: None, // open
        });
        s.transactions.push(crate::tap::TxnStats {
            txn: Some("b".into()),
            conn: None,
            app: None,
            pool: None,
            statement_count: 1,
            error_count: 0,
            distinct_fingerprints: 1,
            last_fingerprint: None,
            first_ts_unix_micros: 0,
            last_ts_unix_micros: 0,
            span_micros: 0,
            total_query_micros: 0,
            outcome: Some(crate::tap::TxnOutcome::Commit),
        });
        let stats = summary_stats(&s);
        assert_eq!(stats.txn_count, 2);
        assert_eq!(stats.txn_open_count, 1);
    }

    #[test]
    fn render_markdown_includes_summary_block_at_top() {
        let s = empty_snapshot();
        let md = render_markdown(&s);
        assert!(md.contains("## Summary"), "summary block missing:\n{md}");
        // Summary appears BEFORE the section divider.
        let summary_pos = md.find("## Summary").unwrap();
        let divider_pos = md.find("---\n\n##").unwrap();
        assert!(
            summary_pos < divider_pos,
            "Summary must come before the lint section"
        );
    }

    #[test]
    fn render_html_includes_all_six_sections() {
        let s = empty_snapshot();
        let html = render_html(&s);
        for section in &[
            "Summary",
            "Schema lint findings",
            "Tap hotspots",
            "Per-caller rollup",
            "Transactions",
            "N+1 findings",
            "Baseline diff",
        ] {
            assert!(
                html.contains(&format!("<h2>{section}")),
                "HTML missing section heading {section:?}; got:\n{html}"
            );
        }
    }

    #[test]
    fn render_html_summary_renders_total_events_sum() {
        let mut s = empty_snapshot();
        s.hotspots.push(Hotspot {
            fingerprint: "a".into(),
            example_sql: "a".into(),
            count: 7,
            error_count: 0,
            total_micros: 0,
            p50_micros: 0,
            p95_micros: 0,
            p99_micros: 0,
            distinct_callers: 0,
            last_caller: None,
            last_app: None,
        });
        let html = render_html(&s);
        // Summary inline: "<strong>7</strong> query events"
        assert!(
            html.contains("<strong>7</strong> query events"),
            "summary inline count missing; got:\n{html}"
        );
    }

    #[test]
    fn report_transactions_section_includes_pool_column() {
        let mut s = empty_snapshot();
        s.transactions.push(crate::tap::TxnStats {
            txn: Some("p-1#1".into()),
            conn: Some("c-1".into()),
            app: None,
            pool: Some("primary".into()),
            statement_count: 3,
            error_count: 0,
            distinct_fingerprints: 1,
            last_fingerprint: None,
            first_ts_unix_micros: 0,
            last_ts_unix_micros: 100,
            span_micros: 100,
            total_query_micros: 30,
            outcome: Some(crate::tap::TxnOutcome::Commit),
        });
        let md = render_markdown(&s);
        assert!(md.contains("| Pool |"), "markdown header missing Pool column:\n{md}");
        assert!(md.contains("| primary |"), "markdown row missing pool value:\n{md}");
        let html = render_html(&s);
        assert!(html.contains("<th>Pool</th>"), "HTML header missing Pool column:\n{html}");
        assert!(html.contains("<td>primary</td>"), "HTML row missing pool value:\n{html}");
    }

    #[test]
    fn md_escape_handles_pipes_backticks_and_newlines() {
        assert_eq!(md_escape("a|b"), "a\\|b");
        assert_eq!(md_escape("inline `code`"), "inline \\`code\\`");
        assert_eq!(md_escape("line1\nline2"), "line1 line2");
    }

    #[test]
    fn md_escape_neutralises_html_tags() {
        // GitHub / GitLab / VS Code preview all pass HTML
        // through Markdown. A malicious tap fingerprint
        // landing in the report must NOT render as live HTML.
        assert_eq!(
            md_escape("<script>alert(1)</script>"),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
        // Ampersand also needs escaping so a `&amp;` doesn't
        // round-trip into a different entity.
        assert_eq!(md_escape("a&b"), "a&amp;b");
        // SQL stays readable — no `<` `>` `&` in normal SQL.
        let sql = "SELECT * FROM users WHERE id = ?";
        assert_eq!(md_escape(sql), sql);
    }

    #[test]
    fn md_escape_leaves_normal_text_unchanged() {
        let s = "SELECT * FROM users WHERE id = ?";
        assert_eq!(md_escape(s), s);
    }

    #[test]
    fn html_escape_includes_single_quote() {
        assert_eq!(html_escape("it's"), "it&#39;s");
        assert_eq!(
            html_escape("<a href='x'>"),
            "&lt;a href=&#39;x&#39;&gt;"
        );
    }

    #[test]
    fn render_markdown_strips_script_from_user_supplied_fingerprint() {
        // End-to-end check: a malicious lint finding (the
        // most attacker-influenced field, since lint
        // detail/code can come from extension data) must not
        // round-trip as live HTML through the rendered report.
        let mut s = empty_snapshot();
        s.lint_findings.push(LintFinding {
            severity: Severity::High,
            code: "<script>alert(1)</script>".into(),
            title: "title".into(),
            object: "obj".into(),
            detail: String::new(),
            suggestion: None,
        });
        let md = render_markdown(&s);
        assert!(
            !md.contains("<script>"),
            "raw <script> must NOT appear in markdown output:\n{md}"
        );
        assert!(
            md.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "expected entity-escaped form; got:\n{md}"
        );
    }
}
