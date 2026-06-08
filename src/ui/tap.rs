use super::*;

/// JDBC-tap event monitor (`F4` from anywhere). Dispatches
/// to the recency list (L1) or the hotspots grouped view
/// (L2) depending on `app.tap_nav.view`. Shift-G toggles between
/// them; `c` clears the ring; `q`/esc close.
pub(super) fn draw_tap_monitor(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let popup = centered_pct(area, 92, 80);
    f.render_widget(Clear, popup);
    let dropped = app.tap_health.dropped_events_total;
    let dropped_suffix = if dropped > 0 {
        format!(" · {dropped} dropped")
    } else {
        String::new()
    };
    let view_label = match app.tap_nav.view {
        crate::app::TapView::List => "list",
        crate::app::TapView::Hotspots => "hotspots",
        crate::app::TapView::Callers => "callers",
        crate::app::TapView::Transactions => "transactions",
        crate::app::TapView::Pools => "pools",
        crate::app::TapView::NplusOne => "N+1",
        crate::app::TapView::Baseline => "baseline",
    };
    let sort_suffix = if matches!(
        app.tap_nav.view,
        crate::app::TapView::Hotspots | crate::app::TapView::Callers
    ) {
        format!(" · sort: {}", app.tap_nav.sort.label())
    } else {
        String::new()
    };
    let title = format!(
        " JDBC tap — {} query · {} heartbeat{dropped_suffix} · view: {view_label}{sort_suffix} · v cycle · Shift-B baseline · s sort · c clear · q close ",
        app.tap_health.query_count, app.tap_health.heartbeat_count,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_active))
        .title(Span::styled(title, Style::default().fg(theme.title)));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    match app.tap_nav.view {
        crate::app::TapView::Hotspots => draw_tap_monitor_hotspots(f, inner, app),
        crate::app::TapView::Callers => draw_tap_monitor_callers(f, inner, app),
        crate::app::TapView::Transactions => draw_tap_monitor_txns(f, inner, app),
        crate::app::TapView::Pools => draw_tap_monitor_pools(f, inner, app),
        crate::app::TapView::NplusOne => draw_tap_monitor_nplus1(f, inner, app),
        crate::app::TapView::Baseline => draw_tap_monitor_baseline(f, inner, app),
        crate::app::TapView::List => draw_tap_monitor_list(f, inner, app),
    }
}

fn draw_tap_monitor_list(f: &mut Frame, inner: Rect, app: &App) {
    let theme = &app.theme;
    if app.tap_events.is_empty() {
        let lines = if app.tap_health.heartbeat_count > 0 {
            // pgman-tap JAR is connected but the JVM hasn't
            // fired any queries — short, no setup hint needed.
            vec![
                Line::from(Span::styled(
                    "no tap events yet",
                    Style::default().fg(theme.muted),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "pgman-tap is connected (heartbeats received) but the JVM hasn't fired any queries yet.",
                    Style::default().fg(theme.muted),
                )),
            ]
        } else {
            // No JAR connection seen. Render the setup hint —
            // the operator wants to know "how do I light this
            // panel up?" The OTel path works today; the
            // pgman-tap JAR path is the higher-context option
            // once that JAR ships.
            tap_setup_hint_lines(theme)
        };
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
        return;
    }

    let visible_h = inner.height as usize;
    // Cap cursor against the list len so a recent eviction
    // doesn't park us past the end.
    let cursor = app.tap_nav.events_cursor.min(app.tap_events.len() - 1);
    let scroll = scroll_offset(cursor, visible_h);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(
            "  {:>10}  {:>9}  {:<20}  {}",
            "duration", "rows", "app", "sql / kind"
        ),
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    )));
    let inner_w = inner.width as usize;
    let sql_col = inner_w.saturating_sub(2 + 10 + 2 + 9 + 2 + 20 + 2);
    for (i, e) in app
        .tap_events
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_h.saturating_sub(1))
    {
        let is_focus = i == cursor;
        let style = if is_focus {
            Style::default()
                .fg(theme.text)
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD)
        } else if e.is_error() {
            Style::default().fg(theme.health_red)
        } else {
            Style::default().fg(theme.text)
        };
        let dur = e.duration_micros.map(format_duration).unwrap_or_default();
        let rows = e.rows.map(|r| r.to_string()).unwrap_or_default();
        let app_name = e
            .app
            .as_deref()
            .map(|s| s.chars().take(20).collect::<String>())
            .unwrap_or_default();
        let body = match e.kind {
            crate::tap::TapKind::Query => e.sql_preview(sql_col),
            crate::tap::TapKind::TxnBoundary => match e.txn_outcome {
                Some(crate::tap::TxnOutcome::Commit) => {
                    format!("[COMMIT] {}", e.txn.as_deref().unwrap_or(""))
                }
                Some(crate::tap::TxnOutcome::Rollback) => {
                    format!("[ROLLBACK] {}", e.txn.as_deref().unwrap_or(""))
                }
                None => "[txn boundary]".into(),
            },
            // Heartbeats never land here (filtered upstream).
            crate::tap::TapKind::Heartbeat => "[heartbeat]".into(),
        };
        let line = format!("  {dur:>10}  {rows:>9}  {app_name:<20}  {body}");
        lines.push(Line::from(Span::styled(line, style)));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// L2 hotspots view — groups the ring by SQL fingerprint and
/// renders one row per bucket with count, p50/p95/p99 latency,
/// and the most-recent caller frame.
fn draw_tap_monitor_hotspots(f: &mut Frame, inner: Rect, app: &App) {
    let theme = &app.theme;
    let hotspots = app.current_hotspots();
    if hotspots.is_empty() {
        let lines = vec![
            Line::from(Span::styled(
                "no hotspots yet — waiting for query events",
                Style::default().fg(theme.muted),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Shift-G to switch back to the recency list.",
                Style::default().fg(theme.muted),
            )),
        ];
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
        return;
    }
    let visible_h = inner.height as usize;
    let cursor = app.tap_nav.hotspots_cursor.min(hotspots.len() - 1);
    let scroll = scroll_offset(cursor, visible_h);
    // Header row first.
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(
            "  {:>6}  {:>5}  {:>9}  {:>9}  {:>9}  {}",
            "calls", "err", "p50", "p95", "p99", "fingerprint · last caller"
        ),
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    )));
    let inner_w = inner.width as usize;
    // 2 + 6 + 2 + 5 + 2 + 9*3 + 2*3 + 2 = 50 (give or take); rest is for the
    // fingerprint + caller column.
    let body_col = inner_w.saturating_sub(2 + 6 + 2 + 5 + 2 + 9 + 2 + 9 + 2 + 9 + 2);
    for (i, h) in hotspots
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_h.saturating_sub(1))
    {
        let is_focus = i == cursor;
        let style = if is_focus {
            Style::default()
                .fg(theme.text)
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD)
        } else if h.error_count > 0 {
            Style::default().fg(theme.health_red)
        } else {
            Style::default().fg(theme.text)
        };
        let body = match &h.last_caller {
            Some(c) => format!(
                "{} · {}",
                short_fingerprint(&h.fingerprint, body_col / 2),
                c
            ),
            None => short_fingerprint(&h.fingerprint, body_col),
        };
        let line = format!(
            "  {count:>6}  {err:>5}  {p50:>9}  {p95:>9}  {p99:>9}  {body}",
            count = h.count,
            err = h.error_count,
            p50 = format_duration(h.p50_micros),
            p95 = format_duration(h.p95_micros),
            p99 = format_duration(h.p99_micros),
        );
        lines.push(Line::from(Span::styled(line, style)));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// L2 per-caller rollup view — groups the ring by innermost
/// caller frame (`caller[0]`) and renders one row per app
/// code path with count / errors / p50/p95/p99 / distinct
/// fingerprint count / last fingerprint preview. Surfaces
/// "which `@Service` method owns the DB time?" — the
/// leverage point for refactors.
fn draw_tap_monitor_callers(f: &mut Frame, inner: Rect, app: &App) {
    let theme = &app.theme;
    let groups = app.current_callers();
    if groups.is_empty() {
        let lines = vec![
            Line::from(Span::styled(
                "no caller frames yet — waiting for query events",
                Style::default().fg(theme.muted),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Tap events without a caller frame appear in the <unknown> bucket once they arrive.",
                Style::default().fg(theme.muted),
            )),
        ];
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
        return;
    }
    let visible_h = inner.height as usize;
    let cursor = app.tap_nav.callers_cursor.min(groups.len() - 1);
    let scroll = scroll_offset(cursor, visible_h);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(
            "  {:>6}  {:>5}  {:>9}  {:>9}  {:>4}  {}",
            "calls", "err", "p50", "p95", "fps", "caller · last fingerprint"
        ),
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    )));
    let inner_w = inner.width as usize;
    let body_col = inner_w.saturating_sub(2 + 6 + 2 + 5 + 2 + 9 + 2 + 9 + 2 + 4 + 2);
    for (i, g) in groups
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_h.saturating_sub(1))
    {
        let is_focus = i == cursor;
        let style = if is_focus {
            Style::default()
                .fg(theme.text)
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD)
        } else if g.error_count > 0 {
            Style::default().fg(theme.health_red)
        } else {
            Style::default().fg(theme.text)
        };
        let caller = short_fingerprint(&g.caller, body_col / 2);
        let last_fp = g
            .last_fingerprint
            .as_deref()
            .map(|fp| short_fingerprint(fp, body_col / 2))
            .unwrap_or_default();
        let body = if last_fp.is_empty() {
            caller
        } else {
            format!("{caller} · {last_fp}")
        };
        let line = format!(
            "  {count:>6}  {err:>5}  {p50:>9}  {p95:>9}  {fps:>4}  {body}",
            count = g.count,
            err = g.error_count,
            p50 = format_duration(g.p50_micros),
            p95 = format_duration(g.p95_micros),
            fps = g.distinct_fingerprints,
        );
        lines.push(Line::from(Span::styled(line, style)));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// L2 baseline-diff view — shows what changed since the
/// operator captured a baseline with Shift-B. Each row is
/// one fingerprint that's new, regressed (≥2× p95), or
/// disappeared. Operators get instant "did my deploy break
/// anything?" without opening a separate tool.
fn draw_tap_monitor_baseline(f: &mut Frame, inner: Rect, app: &App) {
    let theme = &app.theme;
    let Some(baseline) = app.tap_baseline.as_ref() else {
        let lines = vec![
            Line::from(Span::styled(
                "no baseline captured yet",
                Style::default().fg(theme.muted),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Press Shift-B from any tap view to freeze the current hotspots.",
                Style::default().fg(theme.muted),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Then iterate (deploy, refactor, retune) and come back to this view:",
                Style::default().fg(theme.muted),
            )),
            Line::from(Span::styled(
                "  · new fingerprints highlighted in green",
                Style::default().fg(theme.muted),
            )),
            Line::from(Span::styled(
                "  · ≥2× p95 regressions highlighted in red",
                Style::default().fg(theme.muted),
            )),
            Line::from(Span::styled(
                "  · disappeared fingerprints in yellow",
                Style::default().fg(theme.muted),
            )),
        ];
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
        return;
    };
    let diffs = app.current_baseline_diff();
    let captured_age = baseline_age_label(baseline.captured_at_unix_micros);
    // Show drops-since-capture in a third header line when
    // non-zero: those events would have shaped the diff but
    // were never seen by current_hotspots. Without this the
    // baseline view silently misreports "no regression" on the
    // very burst shape (thundering herd) most likely to need
    // it.
    let mut header_lines: Vec<Line> = vec![
        Line::from(Span::styled(
            format!(
                "baseline captured {captured_age} · {} fingerprint(s) · {} event(s) at capture",
                baseline.hotspots.len(),
                baseline.captured_event_count
            ),
            Style::default().fg(theme.muted),
        )),
        Line::from(Span::styled(
            format!(
                "current ring: {} event(s) · {} changed fingerprint(s) (Shift-B recaptures)",
                app.tap_events.len(),
                diffs.len()
            ),
            Style::default().fg(theme.muted),
        )),
    ];
    if let Some(delta) = app.baseline_listener_drops_since_capture() {
        if delta > 0 {
            header_lines.push(Line::from(Span::styled(
                format!(
                    "⚠ {delta} event(s) dropped at listener since capture — diff below is a subsample"
                ),
                Style::default().fg(theme.health_yellow),
            )));
        }
    }
    header_lines.push(Line::from(""));
    if diffs.is_empty() {
        let mut lines = header_lines;
        lines.push(Line::from(Span::styled(
            "no changes since baseline — nothing new, no regressions, no disappearances.",
            Style::default().fg(theme.health_green),
        )));
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
        return;
    }
    let visible_h = inner.height as usize;
    let header_h = header_lines.len();
    let table_h = visible_h.saturating_sub(header_h + 1);
    let cursor = app.tap_nav.baseline_cursor.min(diffs.len() - 1);
    let scroll = scroll_offset(cursor, table_h);
    let mut lines: Vec<Line> = header_lines;
    lines.push(Line::from(Span::styled(
        format!(
            "  {:<11}  {:>6}  {:>6}  {:>9}  {:>9}  {}",
            "change", "Δcalls", "calls", "Δp95", "p95", "fingerprint"
        ),
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    )));
    let inner_w = inner.width as usize;
    let body_col = inner_w.saturating_sub(2 + 11 + 2 + 6 + 2 + 6 + 2 + 9 + 2 + 9 + 2);
    for (i, d) in diffs
        .iter()
        .enumerate()
        .skip(scroll)
        .take(table_h.saturating_sub(1))
    {
        let is_focus = i == cursor;
        let row_style = if is_focus {
            Style::default()
                .fg(theme.text)
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            match d.kind {
                crate::tap::DiffKind::Regressed => Style::default().fg(theme.health_red),
                crate::tap::DiffKind::New => Style::default().fg(theme.health_green),
                crate::tap::DiffKind::Disappeared => Style::default().fg(theme.health_yellow),
                crate::tap::DiffKind::Unchanged => Style::default().fg(theme.text),
            }
        };
        let label = match d.kind {
            crate::tap::DiffKind::Regressed => "regressed",
            crate::tap::DiffKind::New => "new",
            crate::tap::DiffKind::Disappeared => "disappeared",
            crate::tap::DiffKind::Unchanged => "unchanged",
        };
        let delta_calls = signed_delta(d.current_count as i64 - d.baseline_count as i64);
        let delta_p95 = if d.baseline_p95_micros == 0 {
            "—".to_string()
        } else {
            let factor = d.current_p95_micros as f64 / d.baseline_p95_micros as f64;
            format!("{factor:.1}×")
        };
        let line = format!(
            "  {label:<11}  {delta:>6}  {calls:>6}  {dp95:>9}  {p95:>9}  {body}",
            delta = delta_calls,
            calls = d.current_count,
            dp95 = delta_p95,
            p95 = format_duration(d.current_p95_micros),
            body = short_fingerprint(&d.fingerprint, body_col),
        );
        lines.push(Line::from(Span::styled(line, row_style)));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// Pretty-print a signed delta with explicit + sign for
/// growth — the baseline view leans hard on these numbers
/// so the +/- prefix matters.
fn signed_delta(d: i64) -> String {
    match d.cmp(&0) {
        std::cmp::Ordering::Greater => format!("+{d}"),
        _ => d.to_string(),
    }
}

/// "Xs ago" / "Xm ago" / "Xh ago" label for the baseline
/// capture timestamp. Capped at hours — older baselines are
/// almost always stale and the operator should recapture.
fn baseline_age_label(captured_unix_micros: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0);
    if captured_unix_micros == 0 || now <= captured_unix_micros {
        return "just now".into();
    }
    let secs = (now - captured_unix_micros) / 1_000_000;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else {
        format!("{}h ago", secs / 3600)
    }
}

/// L2 transactions view — one row per synthetic `txn` id
/// (or per `conn` for autocommit traffic), surfaces
/// long-held open transactions and the "47 SELECTs + 1
/// COMMIT" N+1 shape at the txn level. Open transactions
/// in `health_yellow` (likely diagnostic target),
/// rollbacks in `health_red`, commits in default colour.
fn draw_tap_monitor_txns(f: &mut Frame, inner: Rect, app: &App) {
    let theme = &app.theme;
    let txns = app.current_txns();
    if txns.is_empty() {
        let lines = vec![
            Line::from(Span::styled(
                "no transactions observed yet",
                Style::default().fg(theme.muted),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Transactions appear once the JAR emits events tagged with a `txn` id, or once",
                Style::default().fg(theme.muted),
            )),
            Line::from(Span::styled(
                "autocommit traffic groups by connection. Heartbeats don't count.",
                Style::default().fg(theme.muted),
            )),
        ];
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
        return;
    }
    let visible_h = inner.height as usize;
    let cursor = app.tap_nav.txns_cursor.min(txns.len() - 1);
    let scroll = scroll_offset(cursor, visible_h);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(
            "  {:<10}  {:>6}  {:>5}  {:>10}  {:>10}  {:<12}  {}",
            "state", "stmts", "fps", "span", "db-time", "pool", "txn / conn · last sql"
        ),
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    )));
    let inner_w = inner.width as usize;
    let body_col = inner_w.saturating_sub(2 + 10 + 2 + 6 + 2 + 5 + 2 + 10 + 2 + 10 + 2 + 12 + 2);
    for (i, t) in txns
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_h.saturating_sub(1))
    {
        let is_focus = i == cursor;
        let state_label = match t.outcome {
            None => "open",
            Some(crate::tap::TxnOutcome::Commit) => "commit",
            Some(crate::tap::TxnOutcome::Rollback) => "rollback",
        };
        let row_style = if is_focus {
            Style::default()
                .fg(theme.text)
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            match t.outcome {
                None => Style::default().fg(theme.health_yellow),
                Some(crate::tap::TxnOutcome::Rollback) => Style::default().fg(theme.health_red),
                Some(crate::tap::TxnOutcome::Commit) => Style::default().fg(theme.text),
            }
        };
        let id_label = match t.txn.as_deref() {
            Some(id) => id.to_string(),
            None => format!("(autocommit · {})", t.conn.as_deref().unwrap_or("?")),
        };
        let last_fp = t.last_fingerprint.as_deref().unwrap_or("");
        let body = format!(
            "{} · {}",
            short_fingerprint(&id_label, body_col / 2),
            short_fingerprint(last_fp, body_col / 2)
        );
        let pool_label = short_fingerprint(t.pool.as_deref().unwrap_or("—"), 12);
        let line = format!(
            "  {state:<10}  {stmts:>6}  {fps:>5}  {span:>10}  {dbt:>10}  {pool:<12}  {body}",
            state = state_label,
            stmts = t.statement_count,
            fps = t.distinct_fingerprints,
            pool = pool_label,
            span = format_duration(t.span_micros),
            dbt = format_duration(t.total_query_micros),
        );
        lines.push(Line::from(Span::styled(line, row_style)));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// L2 pool-saturation gauge — groups the ring by connection-
/// pool name and renders one row per pool with distinct-
/// connection breadth, peak in-flight concurrency, query
/// volume / errors, total busy time, and p95 latency.
/// Surfaces "is this pool running hot?" and the classic
/// read-replica misrouting (a write-heavy pool named
/// `replica`). The configured HikariCP max isn't shown yet —
/// it waits on the JAR shipping `pool-max` in its heartbeat.
fn draw_tap_monitor_pools(f: &mut Frame, inner: Rect, app: &App) {
    let theme = &app.theme;
    let pools = app.current_pools();
    if pools.is_empty() {
        let lines = vec![
            Line::from(Span::styled(
                "no pools observed yet — waiting for query events",
                Style::default().fg(theme.muted),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Pools appear once query events carry a `pool` name (HikariCP poolName).",
                Style::default().fg(theme.muted),
            )),
            Line::from(Span::styled(
                "Untagged traffic groups under <unknown>.",
                Style::default().fg(theme.muted),
            )),
        ];
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
        return;
    }
    // The header row consumes one line, so the scrollable body is
    // `inner.height - 1` rows. Anchor the scroll on that height, else
    // the focused last pool lands one row past the visible window.
    let body_h = (inner.height as usize).saturating_sub(1);
    let cursor = app.tap_nav.pools_cursor.min(pools.len() - 1);
    let scroll = scroll_offset(cursor, body_h);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(
            "  {:>5}  {:>5}  {:>6}  {:>5}  {:>10}  {:>9}  {}",
            "conns", "peak", "calls", "err", "busy", "p95", "pool · app"
        ),
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    )));
    let inner_w = inner.width as usize;
    let body_col = inner_w.saturating_sub(2 + 5 + 2 + 5 + 2 + 6 + 2 + 5 + 2 + 10 + 2 + 9 + 2);
    for (i, p) in pools.iter().enumerate().skip(scroll).take(body_h) {
        let is_focus = i == cursor;
        let style = if is_focus {
            Style::default()
                .fg(theme.text)
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD)
        } else if p.error_count > 0 {
            Style::default().fg(theme.health_red)
        } else {
            Style::default().fg(theme.text)
        };
        let body = match &p.last_app {
            Some(a) => format!("{} · {}", short_fingerprint(&p.pool, body_col / 2), a),
            None => short_fingerprint(&p.pool, body_col),
        };
        let line = format!(
            "  {conns:>5}  {peak:>5}  {calls:>6}  {err:>5}  {busy:>10}  {p95:>9}  {body}",
            conns = p.distinct_conns,
            peak = p.peak_concurrent,
            calls = p.query_count,
            err = p.error_count,
            busy = format_duration(p.total_micros),
            p95 = format_duration(p.p95_micros),
        );
        lines.push(Line::from(Span::styled(line, style)));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// L2 N+1 findings view — bursts of `(txn, fingerprint)`
/// fired ≥5 times inside 200ms. Surfaces the most-recent
/// caller frame so the operator can jump to the offending
/// app code.
fn draw_tap_monitor_nplus1(f: &mut Frame, inner: Rect, app: &App) {
    let theme = &app.theme;
    let findings = app.current_nplus1();
    if findings.is_empty() {
        let lines = vec![
            Line::from(Span::styled(
                "no N+1 bursts detected",
                Style::default().fg(theme.muted),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "A finding fires when 5+ events with the same fingerprint land in one transaction within 200ms.",
                Style::default().fg(theme.muted),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "v to cycle back to the list or hotspots view.",
                Style::default().fg(theme.muted),
            )),
        ];
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
        return;
    }
    let visible_h = inner.height as usize;
    let cursor = app.tap_nav.nplus1_cursor.min(findings.len() - 1);
    let scroll = scroll_offset(cursor, visible_h);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(
            "  {:>6}  {:>10}  {:<18}  {}",
            "calls", "span", "txn / conn", "caller · fingerprint"
        ),
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    )));
    let inner_w = inner.width as usize;
    let body_col = inner_w.saturating_sub(2 + 6 + 2 + 10 + 2 + 18 + 2);
    for (i, fnd) in findings
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_h.saturating_sub(1))
    {
        let is_focus = i == cursor;
        let style = if is_focus {
            Style::default()
                .fg(theme.text)
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            // N+1 findings are warnings by nature; render in
            // the health-yellow palette so they stand out from
            // the recency list / hotspots views.
            Style::default().fg(theme.health_yellow)
        };
        let group = fnd
            .txn
            .clone()
            .or_else(|| fnd.conn.clone())
            .unwrap_or_else(|| "—".into());
        let caller = fnd.last_caller.as_deref().unwrap_or("?");
        let body = format!(
            "{} · {}",
            caller,
            short_fingerprint(
                &fnd.fingerprint,
                body_col.saturating_sub(caller.chars().count() + 3)
            )
        );
        let line = format!(
            "  {count:>6}  {span:>10}  {group:<18}  {body}",
            count = fnd.count,
            span = format_duration(fnd.span_micros),
            group = short_fingerprint(&group, 18),
        );
        lines.push(Line::from(Span::styled(line, style)));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// Collapse + truncate a SQL fingerprint for one-line render.
fn short_fingerprint(fp: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let chars: Vec<char> = fp.chars().collect();
    if chars.len() <= width {
        return fp.to_string();
    }
    let kept: String = chars.into_iter().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}
