use super::*;

/// JDBC-tap event monitor (`F4` from anywhere). Dispatches
/// to the recency list (L1) or the hotspots grouped view
/// (L2) depending on `app.tap_nav.view`. Shift-G toggles between
/// them; `c` clears the ring; `q`/esc close.
pub(super) fn draw_tap_monitor(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = &app.theme;
    let popup = centered_pct(area, 92, 80);
    f.render_widget(Clear, popup);
    // Compact, and last: the producer's own drop counter can be any
    // u64, and a panel too narrow for the whole title loses trailing
    // segments first — so the drop count goes before the key hints do.
    // The monitor's status line carries it too.
    let dropped = app.tap_health.dropped_events_total;
    let dropped_suffix = if dropped > 0 {
        format!(" · {} dropped", compact_count(dropped))
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
        "JDBC tap — {} · {} · view: {view_label}{sort_suffix} · v cycle · Shift-B baseline · s sort · c clear · q close{dropped_suffix}",
        count_label(app.tap_health.query_count, "query", "queries"),
        count_label(app.tap_health.heartbeat_count, "heartbeat", "heartbeats"),
    );
    // Room between the block's two corner glyphs, less what the title's
    // decoration and padding add to its text. Without this the border
    // cut the title mid-word (`… Shift-B base┐`) — and still did by the
    // decoration's width while only the padding was taken off.
    let title_budget = (popup.width.saturating_sub(2) as usize)
        .saturating_sub(crate::ui::chrome::panel_title_overhead(theme, "JDBC tap"));
    let block = crate::ui::chrome::panel_block(theme)
        .border_style(Style::default().fg(theme.border_active))
        .title(crate::ui::chrome::panel_title(
            theme,
            &format!(" {} ", fit_title(&title, title_budget)),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    // The focused-event overlay (`Enter` on a List row) takes the
    // panel's interior; the list keeps its place underneath.
    if app.tap_nav.event_detail.is_some() {
        draw_tap_event_detail(f, inner, app);
        return;
    }
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
                    "A tap client is connected (heartbeats received) but the JVM hasn't fired any queries yet.",
                    Style::default().fg(theme.muted),
                )),
            ]
        } else {
            // No JAR connection seen. Render the setup hint —
            // the operator wants to know "how do I light this
            // panel up?" The OTel path works today; the
            // pgman-tap JAR path is the higher-context option
            // once that JAR ships.
            tap_setup_hint_lines(theme, &app.tap_listeners)
        };
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
        return;
    }

    // Cap cursor against the list len so a recent eviction
    // doesn't park us past the end.
    let cursor = app.tap_nav.events_cursor.min(app.tap_events.len() - 1);
    // The focused query's bound parameters sit under the list. They
    // are the one part of an event a row has no room for, and the
    // reason to look at a single statement rather than its hotspot.
    // Dropped on a panel too short to keep a few list rows above it.
    // The rows are reserved whatever the focused event's kind, so the
    // list's height doesn't jump as the cursor crosses a COMMIT row.
    let inner = if inner.height >= TAP_DETAIL_MIN_PANEL_H {
        let [list, rest] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(TAP_DETAIL_H)]).areas(inner);
        let summary = app.tap_events[cursor].params_summary();
        draw_tap_params_detail(f, rest, theme, summary.as_ref());
        list
    } else {
        inner
    };
    let visible_h = inner.height as usize;
    let scroll = scroll_offset(cursor, visible_h);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(
            "  {:>10}  {:>9}  {:<TAP_APP_COL$}  {}",
            "duration", "rows", "app", "sql / kind"
        )
        .to_uppercase(),
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    )));
    let inner_w = inner.width as usize;
    let sql_col = inner_w.saturating_sub(2 + 10 + 2 + 9 + 2 + TAP_APP_COL + 2);
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
        let app_name = tap_app_cell(e.app.as_deref().unwrap_or(""));
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
        let line = format!("  {dur:>10}  {rows:>9}  {app_name}  {body}");
        lines.push(Line::from(Span::styled(line, style)));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// Display columns of the List view's app column.
const TAP_APP_COL: usize = 20;

/// The List view's app cell: `app` cut and padded to exactly
/// [`TAP_APP_COL`] display columns. Counting chars instead let a CJK
/// app name paint twice as wide and shove its row's SQL right of every
/// other row's. Pure / testable.
pub(crate) fn tap_app_cell(app: &str) -> String {
    let mut cell = crate::grid::truncate_cell(app, TAP_APP_COL);
    let pad = TAP_APP_COL.saturating_sub(display_width(&cell));
    cell.extend(std::iter::repeat_n(' ', pad));
    cell
}

/// Rows the List view's params detail takes: a rule, then the values
/// over up to two lines.
const TAP_DETAIL_H: u16 = 3;
/// Shortest List-view panel that still gets the params detail: the
/// header and four event rows stay above it.
const TAP_DETAIL_MIN_PANEL_H: u16 = TAP_DETAIL_H + 5;
/// Indent of the values' continuation line.
const TAP_DETAIL_INDENT: &str = "    ";

/// The focused event's bound parameters, under the List view. A label
/// that says when some are the producer's `[redacted]` marker — in the
/// warning colour, so a masked value is never taken for the real one.
/// `None` (a commit / rollback row) draws the rule and says so.
fn draw_tap_params_detail(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    summary: Option<&crate::tap::ParamsSummary>,
) {
    let width = area.width as usize;
    let mut lines = vec![params_detail_rule(theme, width)];
    let Some(summary) = summary else {
        lines.push(Line::from(Span::styled(
            "  no parameters on a commit / rollback",
            Style::default().fg(theme.muted),
        )));
        f.render_widget(Paragraph::new(Text::from(lines)), area);
        return;
    };
    let label_style = if summary.redacted {
        Style::default()
            .fg(theme.health_yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.muted)
    };
    let label = format!("  {}  ", summary.label);
    let (first, rest) = params_detail_split(display_width(&label), &summary.body, width);
    let value = Style::default().fg(theme.text);
    lines.push(Line::from(vec![
        Span::styled(label, label_style),
        Span::styled(first, value),
    ]));
    if let Some(rest) = rest {
        lines.push(Line::from(Span::styled(
            format!("{TAP_DETAIL_INDENT}{rest}"),
            value,
        )));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), area);
}

/// The rule over the params strip, naming the key that opens the whole
/// event: the footer that would say so is the monitor's status while
/// it is open. A bare rule where the words don't fit.
fn params_detail_rule(theme: &Theme, width: usize) -> Line<'static> {
    const LEAD: &str = "── ";
    const HINT: &str = "enter: the whole event";
    let rule = Style::default().fg(theme.border_idle);
    let used = display_width(LEAD) + HINT.len() + 1;
    if width < used + 2 {
        return Line::from(Span::styled("─".repeat(width), rule));
    }
    Line::from(vec![
        Span::styled(LEAD, rule),
        Span::styled(HINT, Style::default().fg(theme.muted)),
        Span::styled(format!(" {}", "─".repeat(width - used)), rule),
    ])
}

/// Lay `body` out after a `label_w`-column label on a `width`-column
/// row: what fits on the first line, and the rest on an indented
/// second line, cut with `…` if it still doesn't fit — so a long
/// parameter list says it was cut rather than stopping mid-value.
/// By display width, so a wide glyph never pushes a line past `width`.
/// Pure.
fn params_detail_split(label_w: usize, body: &str, width: usize) -> (String, Option<String>) {
    let first_w = width.saturating_sub(label_w);
    let (first, marker) = crate::grid::truncate_cell_parts(body, first_w);
    if marker.is_empty() {
        return (first, None);
    }
    // `truncate_cell_parts` keeps a prefix of `body`.
    let rest = body[first.len()..].trim_start();
    let second_w = width.saturating_sub(TAP_DETAIL_INDENT.len());
    (first, Some(crate::grid::truncate_cell(rest, second_w)))
}

/// Width of the focused-event overlay's label column, `root cause`
/// and its gap included.
const EVENT_DETAIL_LABEL_W: usize = 12;
/// Left margin of every overlay row.
const EVENT_DETAIL_MARGIN: &str = "  ";

/// How a field of the focused-event overlay is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetailTone {
    Plain,
    /// Not sent by the producer (`—`).
    Muted,
    /// A failed statement's kind and its cause chain.
    Error,
    /// Values some of which are the producer's `[redacted]` marker.
    Warning,
}

/// One labelled field of the focused-event overlay. `value` may hold
/// line breaks; each line is wrapped under the label's hanging indent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DetailField {
    pub label: &'static str,
    pub value: String,
    pub tone: DetailTone,
}

/// `ts_unix_micros` as `2026-10-23T09:14:02.118204Z`. Pure.
fn format_unix_micros_utc(micros: u64) -> String {
    let secs = crate::app::format_unix_secs_utc(micros / 1_000_000);
    let whole = secs.strip_suffix('Z').unwrap_or(&secs);
    format!("{whole}.{:06}Z", micros % 1_000_000)
}

/// Everything the tap stored about one event, as the overlay lists
/// it: the parts the List row and the params strip have no room for —
/// the whole SQL, `pool` / `conn` / `txn`, the cause chain (root cause
/// last) and the caller stack (innermost first). A field the producer
/// can send but didn't reads `—`. Values from the application have
/// their control characters made visible, line breaks kept. Pure.
pub(crate) fn event_detail_fields(e: &crate::tap::TapEvent) -> Vec<DetailField> {
    use crate::grid::show_controls;
    use crate::tap::{TapKind, TxnOutcome};
    fn field(label: &'static str, value: String, tone: DetailTone) -> DetailField {
        DetailField { label, value, tone }
    }
    fn sent(label: &'static str, v: Option<String>) -> DetailField {
        match v {
            Some(v) => field(label, v, DetailTone::Plain),
            None => field(label, "—".into(), DetailTone::Muted),
        }
    }
    let one_line = |v: &Option<String>| v.as_deref().map(|v| show_controls(v, true).into_owned());
    let is_query = matches!(e.kind, TapKind::Query);
    let kind = match (e.kind, e.txn_outcome) {
        (TapKind::Query, _) if e.is_error() => "query · failed",
        (TapKind::Query, _) => "query",
        (TapKind::TxnBoundary, Some(TxnOutcome::Commit)) => "commit",
        (TapKind::TxnBoundary, Some(TxnOutcome::Rollback)) => "rollback",
        (TapKind::TxnBoundary, None) => "txn boundary",
        (TapKind::Heartbeat, _) => "heartbeat",
    };
    let kind_tone = if e.is_error() {
        DetailTone::Error
    } else {
        DetailTone::Plain
    };
    let mut out = vec![
        field("kind", kind.into(), kind_tone),
        field(
            if is_query { "started" } else { "at" },
            format_unix_micros_utc(e.ts_unix_micros),
            DetailTone::Plain,
        ),
    ];
    if is_query {
        out.push(sent("duration", e.duration_micros.map(format_duration)));
        out.push(sent("rows", e.rows.map(|r| r.to_string())));
    }
    out.push(sent("app", one_line(&e.app)));
    out.push(sent("pool", one_line(&e.pool)));
    out.push(sent("conn", one_line(&e.conn)));
    out.push(sent("txn", one_line(&e.txn)));
    if !is_query {
        return out;
    }
    out.push(sent(
        "sql",
        e.sql
            .as_deref()
            .map(|s| show_controls(s, false).into_owned()),
    ));
    if let Some(summary) = e.params_summary() {
        let value = match e.params.as_deref() {
            Some(values) if !values.is_empty() => values
                .iter()
                .enumerate()
                .map(|(i, v)| format!("{}: {}", i + 1, show_controls(v, true)))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => summary.body,
        };
        let tone = if summary.redacted {
            DetailTone::Warning
        } else {
            DetailTone::Plain
        };
        out.push(field(summary.label, value, tone));
    }
    let causes = e.error.as_deref().unwrap_or_default();
    for (i, cause) in causes.iter().enumerate() {
        let label = if i == 0 {
            "error"
        } else if i + 1 == causes.len() {
            "root cause"
        } else {
            "caused by"
        };
        out.push(field(
            label,
            show_controls(cause, false).into_owned(),
            DetailTone::Error,
        ));
    }
    out.push(match e.caller.as_deref() {
        Some(frames) if !frames.is_empty() => field(
            "caller",
            frames
                .iter()
                .map(|f| show_controls(f, true).into_owned())
                .collect::<Vec<_>>()
                .join("\n"),
            DetailTone::Plain,
        ),
        _ => field("caller", "— (not captured)".into(), DetailTone::Muted),
    });
    out
}

/// One row of the focused-event overlay: the label column (margin,
/// label, padding — or blank under a wrapped value) and the value text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DetailRow {
    pub label: String,
    pub value: String,
    pub tone: DetailTone,
}

/// The overlay's rows at `width` columns: each field's label, then its
/// value wrapped by words under a hanging indent, one row per wrapped
/// line, each with the field's tone. Pure.
pub(crate) fn event_detail_rows(fields: &[DetailField], width: usize) -> Vec<DetailRow> {
    let indent = EVENT_DETAIL_MARGIN.len() + EVENT_DETAIL_LABEL_W;
    let value_w = width.saturating_sub(indent).max(1);
    let mut rows = Vec::new();
    for f in fields {
        let mut first = true;
        // A label too wide for its column (`params (some redacted)`)
        // takes a row of its own, the value under it.
        if f.label.len() >= EVENT_DETAIL_LABEL_W {
            rows.push(DetailRow {
                label: format!("{EVENT_DETAIL_MARGIN}{}", f.label),
                value: String::new(),
                tone: f.tone,
            });
            first = false;
        }
        for line in f.value.split('\n') {
            // A blank line in a value (in the SQL) still takes a row,
            // so the value reads as it was sent.
            let wrapped = if line.trim().is_empty() {
                vec![String::new()]
            } else {
                wrap_words(line, value_w)
            };
            for piece in wrapped {
                let label = if first { f.label } else { "" };
                first = false;
                rows.push(DetailRow {
                    label: format!("{EVENT_DETAIL_MARGIN}{label:<w$}", w = EVENT_DETAIL_LABEL_W),
                    value: piece,
                    tone: f.tone,
                });
            }
        }
    }
    rows
}

/// The focused-event overlay, over the tap monitor's interior: a
/// bordered box titled with its keys and, when it scrolls, which rows
/// are showing. Writes `max_scroll` back for the key handler.
fn draw_tap_event_detail(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = &app.theme;
    let Some(detail) = app.tap_nav.event_detail.as_mut() else {
        return;
    };
    f.render_widget(Clear, area);
    let fields = event_detail_fields(&detail.event);
    let rows = event_detail_rows(&fields, area.width.saturating_sub(2) as usize);
    let visible = area.height.saturating_sub(2) as usize;
    detail.max_scroll = rows.len().saturating_sub(visible);
    detail.scroll = detail.scroll.min(detail.max_scroll);
    let scroll = detail.scroll;
    let position = if rows.len() > visible {
        format!(
            " · rows {}–{} of {}",
            scroll + 1,
            (scroll + visible).min(rows.len()),
            rows.len()
        )
    } else {
        String::new()
    };
    let title = format!("event — j/k scroll · esc back{position}");
    let budget = (area.width.saturating_sub(2) as usize)
        .saturating_sub(crate::ui::chrome::panel_title_overhead(theme, "event"));
    let block = crate::ui::chrome::panel_block(theme)
        .border_style(Style::default().fg(theme.border_active))
        .title(crate::ui::chrome::panel_title(
            theme,
            &format!(" {} ", fit_title(&title, budget)),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines: Vec<Line> = rows
        .into_iter()
        .skip(scroll)
        .take(visible)
        .map(|row| {
            let value_style = match row.tone {
                DetailTone::Plain => Style::default().fg(theme.text),
                DetailTone::Muted => Style::default().fg(theme.muted),
                DetailTone::Error => Style::default().fg(theme.health_red),
                DetailTone::Warning => Style::default().fg(theme.health_yellow),
            };
            // As in the params strip: a label that says some values are
            // masked is a warning, so `[redacted]` is never taken for one.
            let label_colour = if row.tone == DetailTone::Warning {
                theme.health_yellow
            } else {
                theme.muted
            };
            Line::from(vec![
                Span::styled(
                    row.label,
                    Style::default()
                        .fg(label_colour)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(row.value, value_style),
            ])
        })
        .collect();
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
        )
        .to_uppercase(),
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
        )
        .to_uppercase(),
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
        )
        .to_uppercase(),
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
        )
        .to_uppercase(),
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
        )
        .to_uppercase(),
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
        )
        .to_uppercase(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_detail_split_keeps_a_short_body_on_one_line() {
        let (first, rest) = params_detail_split(10, "1: 2107", 40);
        assert_eq!(first, "1: 2107");
        assert_eq!(rest, None);
    }

    #[test]
    fn params_detail_split_continues_then_marks_the_cut() {
        let body = (1..=40)
            .map(|i| format!("{i}: value-{i}"))
            .collect::<Vec<_>>()
            .join(" · ");
        let (first, rest) = params_detail_split(10, &body, 40);
        let rest = rest.expect("a second line");
        assert!(body.starts_with(&first), "{first:?}");
        assert!(display_width(&first) <= 30, "{first:?}");
        assert!(rest.ends_with('…'), "the cut must show: {rest:?}");
        assert!(
            display_width(&rest) <= 40 - TAP_DETAIL_INDENT.len(),
            "{rest:?}"
        );
        // The second line picks up where the first stopped.
        let joined = format!("{first}{}", rest.trim_end_matches('…'));
        let compact: String = body.split_whitespace().collect();
        let got: String = joined.split_whitespace().collect();
        assert!(compact.starts_with(&got), "{joined:?}");
    }

    fn failed_query() -> crate::tap::TapEvent {
        crate::tap::TapEvent {
            v: 1,
            kind: crate::tap::TapKind::Query,
            ts_unix_micros: 1_000_000_007,
            received_at_unix_micros: 0,
            app: Some("svc\tA".into()),
            pool: None,
            conn: Some("primary-3".into()),
            txn: Some("primary-3#9".into()),
            sql: Some("SELECT 1\nFROM t".into()),
            params: Some(vec!["7".into(), "[redacted]".into()]),
            params_redacted: true,
            duration_micros: Some(1_500),
            rows: None,
            error: Some(vec![
                "DataAccessException: outer".into(),
                "HibernateException: middle".into(),
                "PSQLException: root".into(),
            ]),
            caller: Some(vec![
                "com.example.Repo.find:10".into(),
                "com.example.Service.run:20".into(),
                "com.example.Controller.get:30".into(),
            ]),
            dropped_events_total: None,
            txn_outcome: None,
        }
    }

    fn by_label<'a>(fields: &'a [DetailField], label: &str) -> Vec<&'a DetailField> {
        fields.iter().filter(|f| f.label == label).collect()
    }

    /// The parts a List row has no room for: the whole cause chain,
    /// labelled down to its root, and every caller frame in order.
    #[test]
    fn event_detail_fields_carry_the_cause_chain_and_the_whole_stack() {
        let fields = event_detail_fields(&failed_query());
        let labels: Vec<&str> = fields.iter().map(|f| f.label).collect();
        assert_eq!(
            labels,
            [
                "kind",
                "started",
                "duration",
                "rows",
                "app",
                "pool",
                "conn",
                "txn",
                "sql",
                "params (some redacted)",
                "error",
                "caused by",
                "root cause",
                "caller",
            ]
        );
        assert_eq!(fields[0].value, "query · failed");
        assert_eq!(fields[0].tone, DetailTone::Error);
        assert_eq!(fields[1].value, "1970-01-01T00:16:40.000007Z");
        assert_eq!(
            by_label(&fields, "root cause")[0].value,
            "PSQLException: root"
        );
        assert!(fields
            .iter()
            .filter(|f| matches!(f.label, "error" | "caused by" | "root cause"))
            .all(|f| f.tone == DetailTone::Error));
        assert_eq!(
            by_label(&fields, "caller")[0].value,
            "com.example.Repo.find:10\ncom.example.Service.run:20\ncom.example.Controller.get:30"
        );
        assert_eq!(by_label(&fields, "txn")[0].value, "primary-3#9");
        // Not sent: reads `—`, muted.
        let pool = by_label(&fields, "pool")[0];
        assert_eq!((pool.value.as_str(), pool.tone), ("—", DetailTone::Muted));
        let rows = by_label(&fields, "rows")[0];
        assert_eq!((rows.value.as_str(), rows.tone), ("—", DetailTone::Muted));
        // One parameter per line, the redaction label in the warning tone.
        let params = by_label(&fields, "params (some redacted)")[0];
        assert_eq!(params.value, "1: 7\n2: [redacted]");
        assert_eq!(params.tone, DetailTone::Warning);
        // The SQL keeps its line break; the app's tab is made visible.
        assert_eq!(by_label(&fields, "sql")[0].value, "SELECT 1\nFROM t");
        assert_eq!(by_label(&fields, "app")[0].value, "svc⇥A");
    }

    #[test]
    fn event_detail_fields_name_a_lone_cause_error_and_mark_a_missing_stack() {
        let mut e = failed_query();
        e.error = Some(vec!["PSQLException: only".into()]);
        e.caller = None;
        let fields = event_detail_fields(&e);
        assert_eq!(by_label(&fields, "error").len(), 1);
        assert!(by_label(&fields, "root cause").is_empty());
        assert_eq!(by_label(&fields, "caller")[0].value, "— (not captured)");
        // A success has no error rows at all, and reads `query`.
        e.error = None;
        let fields = event_detail_fields(&e);
        assert_eq!(fields[0].value, "query");
        assert!(by_label(&fields, "error").is_empty());
    }

    #[test]
    fn event_detail_fields_of_a_commit_carry_no_query_fields() {
        let mut e = failed_query();
        e.kind = crate::tap::TapKind::TxnBoundary;
        e.txn_outcome = Some(crate::tap::TxnOutcome::Rollback);
        let fields = event_detail_fields(&e);
        let labels: Vec<&str> = fields.iter().map(|f| f.label).collect();
        assert_eq!(labels, ["kind", "at", "app", "pool", "conn", "txn"]);
        assert_eq!(fields[0].value, "rollback");
    }

    /// Values wrap under a hanging indent, by display width; a label
    /// wider than its column gets a row of its own.
    #[test]
    fn event_detail_rows_hang_values_under_the_label_column() {
        let fields = vec![
            DetailField {
                label: "root cause",
                value: "alpha beta gamma delta epsilon zeta".into(),
                tone: DetailTone::Error,
            },
            DetailField {
                label: "params (some redacted)",
                value: "1: 東京都\n2: [redacted]".into(),
                tone: DetailTone::Warning,
            },
        ];
        let rows = event_detail_rows(&fields, 30);
        let text: Vec<String> = rows
            .iter()
            .map(|r| format!("{}{}", r.label, r.value))
            .collect();
        assert_eq!(
            text,
            [
                "  root cause  alpha beta gamma",
                "              delta epsilon",
                "              zeta",
                "  params (some redacted)",
                "              1: 東京都",
                "              2: [redacted]",
            ]
        );
        assert!(text.iter().all(|r| display_width(r) <= 30), "{text:?}");
        assert_eq!(rows[0].tone, DetailTone::Error);
        assert_eq!(rows[3].label, "  params (some redacted)");
        assert_eq!(rows[3].value, "");
        assert_eq!(rows[5].tone, DetailTone::Warning);
    }

    #[test]
    fn params_detail_rule_names_enter_where_it_fits_and_is_bare_otherwise() {
        let theme = Theme::default();
        let text = |w| -> String {
            params_detail_rule(&theme, w)
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        };
        assert_eq!(text(30), "── enter: the whole event ────");
        assert_eq!(display_width(&text(30)), 30);
        assert_eq!(text(27), "─".repeat(27));
        assert_eq!(text(0), "");
    }

    #[test]
    fn params_detail_split_measures_wide_glyphs_by_display_width() {
        let body = "1: 東京都千代田区丸の内一丁目九番二号東京都千代田区丸の内";
        let (first, rest) = params_detail_split(4, body, 20);
        assert!(display_width(&first) <= 16, "{first:?}");
        assert!(display_width(&rest.unwrap()) <= 16);
    }
}
