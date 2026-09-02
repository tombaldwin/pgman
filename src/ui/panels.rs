use super::*;

/// "About pgman" overlay — same info as the splash but reachable any time
/// from Normal mode (`A`). Renders the elephant at scale 1 so the popup
/// stays a compact card no matter the terminal size.
pub(super) fn draw_about(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let grid = splash::frame(app.anim_tick);
    let rows_n = grid.len() as u16;
    let cols_n = grid.iter().map(Vec::len).max().unwrap_or(0) as u16;
    let art_w = cols_n * 2;

    let pixel = "██";
    let gap = "  ";
    let mut art_lines: Vec<Line> = Vec::with_capacity(grid.len());
    for row in &grid {
        let spans: Vec<Span> = row
            .iter()
            .map(|&px| match pixel_color(px, theme) {
                Some(c) => Span::styled(pixel, Style::default().fg(c)),
                None => Span::raw(gap),
            })
            .collect();
        art_lines.push(Line::from(spans));
    }

    let mut lines: Vec<Line> = art_lines;
    lines.push(Line::from(""));
    let version_line = if crate::RELEASE_DATE.is_empty() {
        format!("pgman {} · beta", env!("CARGO_PKG_VERSION"))
    } else {
        format!(
            "pgman {} · beta · {}",
            env!("CARGO_PKG_VERSION"),
            crate::RELEASE_DATE
        )
    };
    lines.push(Line::from(Span::styled(
        version_line,
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )));
    // Install channel + update-availability detail. The channel is
    // always known (pure, path-based detection); the update line
    // only appears once the crates.io check has actually landed —
    // "up to date" needs a completed check to say honestly, and
    // showing nothing beforehand is more honest than a guess.
    let channel = crate::update_check::detect_install_channel();
    lines.push(Line::from(Span::styled(
        format!("installed via {}", channel.label()),
        Style::default().fg(theme.muted),
    )));
    match (&app.update_available, app.update_check_done) {
        (Some(update), _) => {
            lines.push(Line::from(Span::styled(
                format!(
                    "update available: {} — {}",
                    update.version,
                    channel.upgrade_command()
                ),
                Style::default().fg(theme.accent),
            )));
        }
        (None, true) => {
            lines.push(Line::from(Span::styled(
                "up to date",
                Style::default().fg(theme.muted),
            )));
        }
        (None, false) => {}
    }
    lines.push(Line::from(Span::styled(
        env!("CARGO_PKG_DESCRIPTION"),
        Style::default().fg(theme.text),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "by Tom Baldwin / Polymorphism Ltd",
        Style::default().fg(theme.muted),
    )));
    lines.push(Line::from(Span::styled(
        "license: MIT OR Apache-2.0",
        Style::default().fg(theme.muted),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "esc / enter / A to close",
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::ITALIC),
    )));

    // Card sized to fit the elephant art width (or the longest text
    // line — the update-command line can run past the art width,
    // especially for a Checkout install's `git … && cargo …` line)
    // plus a comfortable border / padding budget, tall enough for
    // every line below it.
    let _ = rows_n; // sprite already accounted for inside `lines`
    let text_w = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let width = (art_w.max(text_w) + 4).max(48).min(area.width);
    let height = (lines.len() as u16 + 4).min(area.height);
    let popup = centered(area, width, height);
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.border_active))
                    .padding(Padding::uniform(1))
                    .title(Span::styled(" about ", Style::default().fg(theme.title))),
            ),
        popup,
    );
}

/// Modal for a guarded run. Shows the statement, the safety classification,
/// and asks y/n.
pub(super) fn draw_confirm(f: &mut Frame, area: Rect, app: &App) {
    let Some(pending) = &app.pending_run else {
        return;
    };
    let theme = &app.theme;
    let detail = match &pending.summary {
        Some(s) => s.clone(),
        None => pending.decision.kind.describe(),
    };
    let wrap_note = if pending.decision.wrap_in_tx {
        " · will wrap in transaction"
    } else {
        ""
    };
    // For batch runs the SQL can be long — show the first 8 lines and an
    // ellipsis. Single statements show as-is.
    let sql_preview = if pending.is_batch {
        let total = pending.sql.lines().count();
        let preview: String = pending.sql.lines().take(8).collect::<Vec<_>>().join("\n");
        if total > 8 {
            format!("{preview}\n…")
        } else {
            preview
        }
    } else {
        pending.sql.clone()
    };
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "Confirm",
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("{} ({detail}){wrap_note}", pending.kind.label()),
            Style::default().fg(theme.accent),
        )),
        Line::from(""),
    ];
    for sql_line in sql_preview.lines() {
        lines.push(Line::from(Span::styled(
            sql_line.to_string(),
            Style::default().fg(theme.text),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "y = run · n / esc = cancel",
        Style::default().fg(theme.muted),
    )));
    let h = (lines.len() as u16 + 2).min(area.height);
    let widest_line = sql_preview
        .lines()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(40);
    let w = ((widest_line.max(40) + 4) as u16).min(area.width);
    let popup = centered(area, w, h);
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.health_yellow))
                    .style(Style::default().fg(theme.text))
                    .title(Span::styled(
                        " confirm ",
                        Style::default().fg(theme.health_yellow),
                    )),
            )
            .wrap(ratatui::widgets::Wrap { trim: true }),
        popup,
    );
}

/// Log-import picker: lists reconstructed queries (`hibernate` / `pglog`
/// sources), highlights the selection.
pub(super) fn draw_log_pick(f: &mut Frame, area: Rect, app: &App) {
    use crate::app::LogPickView;
    use crate::query::reconstruct::Source;
    let theme = &app.theme;
    let max_preview = 80usize;
    // One-line triage summary above the picker rows. Surfaces N+1
    // hotspots that the per-row list buries.
    let summary = crate::query::nplus1::summarize(&app.log_pick.picks);
    let mut lines: Vec<Line> = Vec::new();
    let view_label = match app.log_pick.view {
        LogPickView::AllQueries => "all queries",
        LogPickView::Clusters => "N+1 clusters",
    };
    lines.push(Line::from(Span::styled(
        format!(
            "  {} · view: {} (press `c` to toggle)",
            summary.one_line(),
            view_label
        ),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )));
    if let (LogPickView::AllQueries, Some(top)) = (app.log_pick.view, summary.top_cluster.as_ref())
    {
        // In the all-queries view, surface the top cluster's leader
        // SQL up top. The clusters view shows the same info as a
        // first-class row, so this leader header would be redundant
        // there.
        let mut preview: String = top
            .example
            .chars()
            .take(max_preview)
            .collect::<String>()
            .replace('\n', " ");
        if top.example.chars().count() > max_preview {
            preview.push('…');
        }
        lines.push(Line::from(Span::styled(
            format!("  leader (×{}): {}", top.count, preview),
            Style::default().fg(theme.muted),
        )));
    }
    lines.push(Line::from(""));
    let row_lines: Vec<Line> = match app.log_pick.view {
        LogPickView::AllQueries => app
            .log_pick
            .picks
            .iter()
            .enumerate()
            .map(|(i, q)| {
                let source = match q.source {
                    Source::HibernateLog => "hibernate",
                    Source::PostgresLog => "pglog",
                    Source::JdbcPaste => "jdbc",
                };
                let mut preview: String = q
                    .runnable_sql
                    .chars()
                    .take(max_preview)
                    .collect::<String>()
                    .replace('\n', " ");
                if q.runnable_sql.chars().count() > max_preview {
                    preview.push('…');
                }
                let prefix = if i == app.log_pick.index {
                    "▶ "
                } else {
                    "  "
                };
                let style = if i == app.log_pick.index {
                    Style::default()
                        .bg(theme.row_selected_bg)
                        .fg(theme.text)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text)
                };
                Line::from(Span::styled(
                    format!("{prefix}[{source:>9}] {preview}"),
                    style,
                ))
            })
            .collect(),
        LogPickView::Clusters => app
            .log_pick
            .clusters
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut preview: String = c
                    .example
                    .chars()
                    .take(max_preview)
                    .collect::<String>()
                    .replace('\n', " ");
                if c.example.chars().count() > max_preview {
                    preview.push('…');
                }
                let prefix = if i == app.log_pick.index {
                    "▶ "
                } else {
                    "  "
                };
                let style = if i == app.log_pick.index {
                    Style::default()
                        .bg(theme.row_selected_bg)
                        .fg(theme.text)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text)
                };
                Line::from(Span::styled(
                    format!("{prefix}×{:<4} {preview}", c.count),
                    style,
                ))
            })
            .collect(),
    };
    lines.extend(row_lines);

    let total = app.log_pick_visible_len();
    let title = format!(
        " log picks · {}/{} ",
        if total == 0 {
            0
        } else {
            app.log_pick.index + 1
        },
        total,
    );
    let h = (lines.len() as u16 + 2)
        .min(area.height.saturating_sub(2))
        .max(3);
    let w = 100u16.min(area.width.saturating_sub(2));
    let popup = centered(area, w, h);
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border_active))
                .style(Style::default().fg(theme.text))
                .title(Span::styled(title, Style::default().fg(theme.title))),
        ),
        popup,
    );
}

pub(super) fn draw_conn_pick(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    // Find the widest origin tag so the DSN column lines up.
    let origin_width = app
        .conn_pick
        .picks
        .iter()
        .map(|p| p.origin.len())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = app
        .conn_pick
        .picks
        .iter()
        .enumerate()
        .map(|(i, pick)| {
            let prefix = if i == app.conn_pick.index {
                "▶ "
            } else {
                "  "
            };
            let style = if i == app.conn_pick.index {
                Style::default()
                    .bg(theme.row_selected_bg)
                    .fg(theme.text)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text)
            };
            // Trailing space pads the row to the popup width so the row
            // background fills the line for the selected entry.
            let body = format!(
                "{prefix}[{origin:>w$}] {name:<24} {dsn}",
                origin = pick.origin,
                w = origin_width,
                name = pick.name,
                dsn = pick.dsn.redacted(),
            );
            Line::from(Span::styled(body, style))
        })
        .collect();

    let title = format!(
        " pick a connection · {}/{} ",
        app.conn_pick.index + 1,
        app.conn_pick.picks.len()
    );
    let h = (lines.len() as u16 + 2)
        .min(area.height.saturating_sub(2))
        .max(3);
    let w = 100u16.min(area.width.saturating_sub(2));
    let popup = centered(area, w, h);
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border_active))
                .style(Style::default().fg(theme.text))
                .title(Span::styled(title, Style::default().fg(theme.title))),
        ),
        popup,
    );
}

pub(super) fn draw_help(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = &app.theme;
    let popup = centered_pct(area, 70, 70);
    // Body content width: popup minus borders (2) and the uniform(1)
    // padding (2) on the horizontal axis — the same budget `inner_height`
    // below uses on the vertical axis. Long descriptions get wrapped to
    // this width ourselves, with a hanging indent to the description
    // column, rather than left to `Paragraph`'s `Wrap` — which has no
    // notion of where the description starts and so wraps continuations
    // to column 0.
    let inner_width = popup.width.saturating_sub(4) as usize;
    let (raw_lines, raw_anchors) = help_body(theme);
    let (lines, anchors) = wrap_help_lines(raw_lines, raw_anchors, inner_width);
    // If we have a captured help_origin, pre-scroll to the matching
    // section the first time draw runs (`help_scroll` is reset to 0
    // by `open_help_from`; we detect that as "anchor not applied
    // yet" and set it once, then clear the origin).
    if let Some(origin) = app.help.origin {
        if app.help.scroll == 0 {
            if let Some(anchor) = App::help_anchor_for(origin) {
                if let Some(&row) = anchors.get(anchor) {
                    app.help.scroll = row;
                }
            }
        }
        // Consume the origin AFTER we've used it to position the
        // scroll. Subsequent draws (j/k navigation) shouldn't snap
        // back to the anchor.
        app.help.origin = None;
    }
    f.render_widget(Clear, popup);
    // Body height = popup height minus borders (top + bottom) minus padding
    // (uniform(1) — top + bottom). That's the visible row budget for clamping
    // the scroll offset.
    let total_lines = lines.len() as u16;
    let inner_height = popup.height.saturating_sub(4);
    let max_scroll = total_lines.saturating_sub(inner_height);
    app.help.max_scroll = max_scroll;
    let effective_scroll = app.help.scroll.min(max_scroll);

    let help = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((effective_scroll, 0))
        .style(Style::default().fg(theme.text))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border_active))
                .padding(Padding::uniform(1)),
        );
    f.render_widget(help, popup);

    // Scroll indicators: emit "↑ N more above" on the top inner row and
    // "↓ N more below" on the bottom inner row when content extends past the
    // viewport. Rendered AFTER the body so they overlay its first / last
    // visible row.
    let hint_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    if effective_scroll > 0 {
        // Overlay the first visible body row: skip border + top padding.
        let row = Rect {
            x: popup.x + 2,
            y: popup.y + 2,
            width: popup.width.saturating_sub(4),
            height: 1,
        };
        f.render_widget(Clear, row);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("↑ {} more above", effective_scroll),
                hint_style,
            ))),
            row,
        );
    }
    if effective_scroll < max_scroll {
        // Overlay the last visible body row: above bottom padding + border.
        let row = Rect {
            x: popup.x + 2,
            y: popup.y + popup.height.saturating_sub(3),
            width: popup.width.saturating_sub(4),
            height: 1,
        };
        f.render_widget(Clear, row);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("↓ {} more below", max_scroll - effective_scroll),
                hint_style,
            ))),
            row,
        );
    }
}

/// EXPLAIN tree modal. Flattens the plan via `App::flattened_explain_rows`,
/// renders each node as one line: `[▶/▼] indent · node_type (relation as
/// alias) · stats`. The hottest node (highest `actual_total_time` or,
/// without ANALYZE, `total_cost`) gets a red accent so the bottleneck
/// is visible at a glance.
pub(super) fn draw_explain_tree(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let rows = app.flattened_explain_rows();
    if rows.is_empty() {
        return;
    }
    // Identify the hottest node by max hot_score across all rows.
    let hottest_path = rows
        .iter()
        .filter_map(|r| r.hot_score.map(|s| (s, r.path.clone())))
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, p)| p);

    let popup = centered_pct(area, 88, 80);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_active))
        .title(Span::styled(
            " EXPLAIN plan — q / esc close ",
            Style::default().fg(theme.title),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    // Scroll so the cursor row stays in view.
    let visible_h = inner.height as usize;
    let scroll = scroll_offset(app.explain.cursor, visible_h);

    let mut lines: Vec<Line> = Vec::with_capacity(rows.len().saturating_sub(scroll).min(visible_h));
    for (i, row) in rows.iter().enumerate().skip(scroll).take(visible_h) {
        let is_focus = i == app.explain.cursor;
        let is_hottest = hottest_path
            .as_ref()
            .map(|p| p == &row.path)
            .unwrap_or(false);
        let indent = "  ".repeat(row.depth);
        let marker = if !row.has_children {
            "·"
        } else if row.collapsed {
            "▶"
        } else {
            "▼"
        };
        let mut header = format!("{indent}{marker} {}", row.node_type);
        if let Some(rel) = &row.relation {
            header.push_str(" on ");
            header.push_str(rel);
            if let Some(alias) = &row.alias {
                if alias != rel {
                    header.push(' ');
                    header.push_str(alias);
                }
            }
        }
        // Compact per-row stats. ANALYZE timing takes priority; cost
        // is the fallback. Rows: actual when present, else planned.
        let mut stats = String::new();
        if let Some(t) = row.actual_total_time {
            stats.push_str(&format!(" · {:.2}ms", t));
        } else if let Some(c) = row.total_cost {
            stats.push_str(&format!(" · cost {:.0}", c));
        }
        if let Some(r) = row.actual_rows {
            stats.push_str(&format!(" · {:.0} rows", r));
        } else if let Some(r) = row.plan_rows {
            stats.push_str(&format!(" · ~{:.0} rows", r));
        }

        let body_style = if is_focus {
            Style::default()
                .fg(theme.text)
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD)
        } else if is_hottest {
            Style::default()
                .fg(theme.health_red)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        let stats_style = if is_focus {
            body_style
        } else {
            Style::default().fg(theme.muted)
        };
        lines.push(Line::from(vec![
            Span::styled(header, body_style),
            Span::styled(stats, stats_style),
        ]));
        // Show extras (Filter, Index Cond, …) under the focused row
        // only — clutter explodes if every row's extras render.
        if is_focus {
            for (k, v) in &row.extras {
                lines.push(Line::from(vec![
                    Span::styled(format!("{indent}    "), Style::default()),
                    Span::styled(format!("{k}: "), Style::default().fg(theme.muted)),
                    Span::styled(v.clone(), Style::default().fg(theme.accent)),
                ]));
            }
        }
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// Slow-query top-N panel. Top section: one-line summary per
/// stored statement, sorted by total exec time desc. Bottom
/// section: full SQL for the focused row + key shortcuts.
pub(super) fn draw_slow_queries(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let popup = centered_pct(area, 92, 80);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_active))
        .title(Span::styled(
            " slow queries — pg_stat_statements — r refresh · enter copy · q close ",
            Style::default().fg(theme.title),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    if app.slow_queries.rows.is_empty() {
        // Either still loading, or genuinely no rows. last_status
        // carries the right phrasing either way; render it inside
        // the popup so the operator doesn't have to look down at
        // the footer.
        let msg = app.last_status.clone().unwrap_or_else(|| "no rows".into());
        f.render_widget(
            Paragraph::new(Text::from(msg)).style(Style::default().fg(theme.muted)),
            inner,
        );
        return;
    }

    let split = Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(8)])
        .split(inner);
    let list_area = split[0];
    let detail_area = split[1];

    // Top: rows as `total_ms  mean_ms  calls  rows  query`.
    let visible_h = list_area.height as usize;
    let scroll = scroll_offset(app.slow_queries.cursor, visible_h);
    let mut lines: Vec<Line> = Vec::new();
    // Header row.
    lines.push(Line::from(Span::styled(
        format!(
            "  {:>10}  {:>9}  {:>8}  {:>8}  {}",
            "total ms", "mean ms", "calls", "rows", "query"
        ),
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    )));
    for (i, row) in app
        .slow_queries
        .rows
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_h.saturating_sub(1))
    {
        let is_focus = i == app.slow_queries.cursor;
        let style = if is_focus {
            Style::default()
                .fg(theme.text)
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        // Truncate query to fit; the full text is shown in the
        // detail pane below.
        let one_line: String = row
            .query
            .chars()
            .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
            .collect();
        let line = format!(
            "  {:>10.2}  {:>9.2}  {:>8}  {:>8}  {}",
            row.total_ms, row.mean_ms, row.calls, row.rows, one_line
        );
        lines.push(Line::from(Span::styled(line, style)));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), list_area);

    // Bottom: full SQL for the focused row. The divider above it is
    // drawn by hand (rather than as a `Block::TOP` border inside
    // `detail_area`) so its ends land on the outer border's column and
    // overwrite it with `├` / `┤` — a `Borders::TOP`-only block has no
    // left/right border to join with, so it can only ever draw a plain
    // `─…─` that leaves the outer `│`s untouched at each end.
    let divider_area = Rect {
        x: detail_area.x.saturating_sub(1),
        y: detail_area.y,
        width: detail_area.width + 2,
        height: 1.min(detail_area.height),
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            divider_line(divider_area.width as usize),
            Style::default().fg(theme.border_idle),
        ))),
        divider_area,
    );
    let detail_content_area = Rect {
        x: detail_area.x,
        y: detail_area.y + divider_area.height,
        width: detail_area.width,
        height: detail_area.height.saturating_sub(divider_area.height),
    };
    let focused_sql = app
        .slow_queries
        .rows
        .get(app.slow_queries.cursor)
        .map(|r| r.query.clone())
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(focused_sql)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(theme.text)),
        detail_content_area,
    );
}

/// Active-sessions + locks panel. Rows ordered by blocked-first;
/// the renderer flags blockers in the same colour as the hottest-
/// node highlight in the EXPLAIN tree (red) so visual scanning
/// matches between the two.
pub(super) fn draw_sessions(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let popup = centered_pct(area, 92, 80);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_active))
        .title(Span::styled(
            " active sessions — pg_stat_activity — r refresh · q close ",
            Style::default().fg(theme.title),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    if app.sessions.rows.is_empty() {
        let msg = app.last_status.clone().unwrap_or_else(|| "no rows".into());
        f.render_widget(
            Paragraph::new(Text::from(msg)).style(Style::default().fg(theme.muted)),
            inner,
        );
        return;
    }

    let visible_h = inner.height as usize;
    let scroll = scroll_offset(app.sessions.cursor, visible_h);
    // Size the state column from the (abbreviated) states actually shown
    // in this viewport, so a row with a long state value (e.g. "idle in
    // transaction") doesn't shear the columns after it out of alignment
    // with the fixed-width `{:>10}` the header used to hardcode.
    let state_w = sessions_state_col_width(
        app.sessions
            .rows
            .iter()
            .skip(scroll)
            .take(visible_h.saturating_sub(1))
            .map(|r| r.state.as_str()),
    );
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(
            "  {:>6}  {:>20}  {:>state_w$}  {:>8}  {:>8}  {}",
            "pid",
            "user/app",
            "state",
            "age(s)",
            "blocked",
            "query",
            state_w = state_w
        ),
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    )));
    for (i, row) in app
        .sessions
        .rows
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_h.saturating_sub(1))
    {
        let is_focus = i == app.sessions.cursor;
        let is_blocked = row.is_blocked();
        let style = if is_focus {
            Style::default()
                .fg(theme.text)
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD)
        } else if is_blocked {
            Style::default()
                .fg(theme.health_red)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        let user_app = if row.application.is_empty() {
            row.user.clone()
        } else {
            format!("{}/{}", row.user, row.application)
        };
        let one_line: String = row
            .query
            .chars()
            .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
            .collect();
        let blocked_disp = if is_blocked {
            row.blocked_by.as_str()
        } else {
            "-"
        };
        let line = format!(
            "  {:>6}  {:>20}  {:>state_w$}  {:>8.1}  {:>8}  {}",
            row.pid,
            user_app,
            abbreviate_state(&row.state),
            row.age_secs,
            blocked_disp,
            one_line,
            state_w = state_w
        );
        lines.push(Line::from(Span::styled(line, style)));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// Shorten well-known long `pg_stat_activity.state` values so the
/// sessions panel's state column stays narrow at typical terminal
/// widths. Unknown states pass through unchanged.
fn abbreviate_state(state: &str) -> &str {
    match state {
        "idle in transaction" => "idle in tx",
        "idle in transaction (aborted)" => "idle in tx (aborted)",
        "fastpath function call" => "fastpath",
        other => other,
    }
}

/// State column width for the sessions panel: the widest abbreviated
/// state among the rows actually shown, floored at the header label's
/// width ("state" = 5) and capped so one long-tailed state can't blow
/// out the rest of the row.
fn sessions_state_col_width<'a>(states: impl Iterator<Item = &'a str>) -> usize {
    const HEADER: usize = 5;
    const CAP: usize = 22;
    states
        .map(|s| abbreviate_state(s).chars().count())
        .max()
        .unwrap_or(0)
        .clamp(HEADER, CAP)
}

/// Saved-queries panel — list view with body preview for the
/// focused entry.
pub(super) fn draw_saved_queries(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let popup = centered_pct(area, 88, 80);
    f.render_widget(Clear, popup);
    // Title carries the live filter when searching, plus a
    // shown/total count so a narrowed list is obvious.
    let visible = app.visible_saved_indices();
    let total = app.saved_queries.entries.len();
    let title = match app.saved_ui.filter.as_ref().map(|t| t.text()) {
        Some(f) => format!(
            " saved queries — /{f}  ({}/{} shown) · enter load · esc clear ",
            visible.len(),
            total
        ),
        None => {
            " saved queries — enter load · r rename · d delete · / search · q close ".to_string()
        }
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_active))
        .title(Span::styled(title, Style::default().fg(theme.title)));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    if total == 0 {
        f.render_widget(
            Paragraph::new(Text::from(vec![
                Line::from(Span::styled(
                    "no saved queries",
                    Style::default().fg(theme.muted),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Ctrl-S in the editor saves the current buffer with a name.",
                    Style::default().fg(theme.muted),
                )),
            ])),
            inner,
        );
        return;
    }
    if visible.is_empty() {
        // Entries exist but the filter excludes them all.
        f.render_widget(
            Paragraph::new(Text::from(vec![Line::from(Span::styled(
                format!(
                    "no saved queries match '{}'",
                    app.saved_ui.filter.as_ref().map(|t| t.text()).unwrap_or("")
                ),
                Style::default().fg(theme.muted),
            ))])),
            inner,
        );
        return;
    }

    let split = Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(10)])
        .split(inner);
    let top = split[0];
    let detail = split[1];

    let cursor = app.saved_ui.cursor.min(visible.len() - 1);
    let visible_h = top.height as usize;
    let scroll = scroll_offset(cursor, visible_h);
    let mut lines: Vec<Line> = Vec::new();
    for (row, &entry_idx) in visible.iter().enumerate().skip(scroll).take(visible_h) {
        let q = &app.saved_queries.entries[entry_idx];
        let is_focus = row == cursor;
        let style = if is_focus {
            Style::default()
                .fg(theme.text)
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        // One-line preview of the body for context.
        let preview: String = q
            .body
            .chars()
            .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
            .take(80)
            .collect();
        let line = format!("  {:<28}  {preview}", q.name);
        lines.push(Line::from(Span::styled(line, style)));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), top);

    // Detail strip: focused query's full body, wrapped.
    let focused = &app.saved_queries.entries[visible[cursor]];
    let mut detail_lines: Vec<Line> = Vec::new();
    detail_lines.push(Line::from(Span::styled(
        format!("  {}", focused.name),
        Style::default()
            .fg(theme.title)
            .add_modifier(Modifier::BOLD),
    )));
    for chunk in wrap_value(&focused.body, detail.width.saturating_sub(4) as usize) {
        detail_lines.push(Line::from(Span::styled(
            format!("  {chunk}"),
            Style::default().fg(theme.text),
        )));
    }
    f.render_widget(Paragraph::new(Text::from(detail_lines)), detail);
}

/// Rename prompt for the focused saved query — a small input box
/// floating over the saved-queries panel. Mirrors the save-query
/// name prompt but pre-fills the current name for editing.
pub(super) fn draw_rename_prompt(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let w = 70u16.min(area.width.saturating_sub(2));
    let popup = centered(area, w, 5);
    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_active))
        .title(Span::styled(
            format!(
                " rename '{}' · enter save · esc cancel ",
                app.saved_ui.rename_from
            ),
            Style::default().fg(theme.title),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    let lines = vec![
        Line::from(Span::styled("new name:", Style::default().fg(theme.muted))),
        Line::from(Span::styled(
            app.saved_ui.rename_buf.text().to_string(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        )),
    ];
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
    let x = inner.x + app.saved_ui.rename_buf.cursor_col() as u16;
    let y = inner.y + 1;
    if x < inner.x + inner.width {
        f.set_cursor_position((x, y));
    }
}

/// Name-prompt overlay for `Ctrl-S` — small centred box; the
/// editor stays visible behind it so the operator can re-check
/// what they're about to save.
pub(super) fn draw_save_query_prompt(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let w = 70u16.min(area.width.saturating_sub(2));
    let h = 5u16;
    let popup = centered(area, w, h);
    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_active))
        .title(Span::styled(
            " save query · enter persist · esc cancel ",
            Style::default().fg(theme.title),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    let lines = vec![
        Line::from(Span::styled("name:", Style::default().fg(theme.muted))),
        Line::from(Span::styled(
            app.saved_ui.save_name.clone(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        )),
    ];
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
    // Place the terminal cursor at end of the typed name.
    let prefix = 0u16; // already on its own line, no leading indent here
    let x = inner.x + prefix + app.saved_ui.save_name.chars().count() as u16;
    let y = inner.y + 1; // second line of the popup body
    if x < inner.x + inner.width {
        f.set_cursor_position((x, y));
    }
}

/// `:param` value prompt shown when loading a parameterised saved
/// query. Renders one input box for the current placeholder, the
/// progress (`2/3`), and the values already entered so the
/// operator can see what they've filled.
pub(super) fn draw_param_prompt(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let Some(pp) = app.saved_ui.param_prompt.as_ref() else {
        return;
    };
    let w = 70u16.min(area.width.saturating_sub(2));
    // One line per already-entered value, plus header + current.
    let h = ((pp.values.len() as u16) + 6).min(area.height.saturating_sub(2));
    let popup = centered(area, w, h);
    f.render_widget(Clear, popup);
    let title = format!(
        " load '{}' · param {}/{} · enter next · esc cancel ",
        pp.query_name,
        (pp.idx + 1).min(pp.params.len()),
        pp.params.len(),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_active))
        .title(Span::styled(title, Style::default().fg(theme.title)));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::new();
    // Already-entered values, dimmed.
    for (name, val) in pp.params.iter().zip(pp.values.iter()) {
        lines.push(Line::from(vec![
            Span::styled(format!(":{name} = "), Style::default().fg(theme.muted)),
            Span::styled(val.clone(), Style::default().fg(theme.text)),
        ]));
    }
    // Current prompt.
    let current = pp.params.get(pp.idx).map(String::as_str).unwrap_or("");
    lines.push(Line::from(Span::styled(
        format!("value for :{current}"),
        Style::default().fg(theme.muted),
    )));
    lines.push(Line::from(Span::styled(
        pp.input.text().to_string(),
        Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
    )));
    let input_row = lines.len() as u16 - 1;
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
    // Cursor at its position within the current input.
    let x = inner.x + pp.input.cursor_col() as u16;
    let y = inner.y + input_row;
    if x < inner.x + inner.width && y < inner.y + inner.height {
        f.set_cursor_position((x, y));
    }
}

/// LISTEN/NOTIFY arrivals panel (`N` from Normal). Lists the
/// ring buffer of recent NOTIFY arrivals; j/k navigate, `y`
/// yanks the focused payload, `c` clears.
pub(super) fn draw_notifications(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let popup = centered_pct(area, 90, 80);
    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_active))
        .title(Span::styled(
            " NOTIFY arrivals — j/k · y yank · c clear · q close ",
            Style::default().fg(theme.title),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    if app.notifications.items.is_empty() {
        let lines = vec![
            Line::from(Span::styled(
                "no NOTIFY arrivals yet",
                Style::default().fg(theme.muted),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "subscribe via `LISTEN <channel>` in the editor; arrivals stream here automatically.",
                Style::default().fg(theme.muted),
            )),
        ];
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
        return;
    }

    let visible_h = inner.height as usize;
    let scroll = scroll_offset(app.notifications.cursor, visible_h);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("  {:<20}  {:>6}  {}", "channel", "pid", "payload"),
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    )));
    for (i, n) in app
        .notifications
        .items
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_h.saturating_sub(1))
    {
        let is_focus = i == app.notifications.cursor;
        let style = if is_focus {
            Style::default()
                .fg(theme.text)
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        let payload: String = n.payload.chars().take(80).collect();
        let line = format!("  {:<20}  {:>6}  {}", n.channel, n.pid, payload);
        lines.push(Line::from(Span::styled(line, style)));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// Rich error overlay (F2 after a query failure). Renders the
/// full server-side `DbError` fields in a labelled vertical
/// list. Read-only modal; closes on F2 / esc / q.
pub(super) fn draw_error_detail(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let popup = centered_pct(area, 85, 70);
    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.health_red))
        .title(Span::styled(
            " error detail — F2 / esc / q close ",
            Style::default().fg(theme.title),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let inner_width = inner.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::new();

    // Severity / code header.
    if let Some(detail) = &app.last_error_detail {
        let header = match (&detail.severity, &detail.code) {
            (Some(sev), Some(code)) => format!(" {sev} · SQLSTATE {code} "),
            (Some(sev), None) => format!(" {sev} "),
            (None, Some(code)) => format!(" SQLSTATE {code} "),
            _ => " ERROR ".to_string(),
        };
        lines.push(Line::from(Span::styled(
            header,
            Style::default()
                .bg(theme.health_red)
                .fg(theme.row_alt_bg)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
    }

    // Primary message.
    if let Some(msg) = &app.last_error {
        push_kv(&mut lines, theme, "message", msg, inner_width);
    }
    if let Some(detail) = &app.last_error_detail {
        if let Some(s) = &detail.detail {
            push_kv(&mut lines, theme, "detail", s, inner_width);
        }
        if let Some(s) = &detail.hint {
            push_kv(&mut lines, theme, "hint", s, inner_width);
        }
        if let Some(s) = &detail.r#where {
            push_kv(&mut lines, theme, "where", s, inner_width);
        }
        // Affected object — only render the lines that are
        // actually present.
        let mut affected: Vec<(&str, &str)> = Vec::new();
        if let Some(s) = &detail.schema {
            affected.push(("schema", s));
        }
        if let Some(s) = &detail.table {
            affected.push(("table", s));
        }
        if let Some(s) = &detail.column {
            affected.push(("column", s));
        }
        if let Some(s) = &detail.constraint {
            affected.push(("constraint", s));
        }
        if let Some(s) = &detail.data_type {
            affected.push(("type", s));
        }
        if !affected.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "affected object:",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )));
            for (k, v) in affected {
                push_kv(&mut lines, theme, k, v, inner_width);
            }
        }
    }
    if app.last_error_detail.is_none() && app.last_error.is_some() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "(no extended detail — non-server error or DbError fields empty)",
            Style::default().fg(theme.muted),
        )));
    }

    f.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        inner,
    );
}

/// Re-flow `help_body`'s lines to `width` columns, giving long
/// descriptions a hanging indent to the column where the description
/// starts (right after the key), instead of `Paragraph`'s `Wrap`
/// dumping the continuation at column 0. Returns the re-flowed lines
/// plus the `anchors` map remapped to the new row indices — headings
/// are never wrapped (they're short), so each still points at the
/// first (and only) row it now occupies.
fn wrap_help_lines(
    raw: Vec<Line<'static>>,
    anchors: std::collections::HashMap<&'static str, u16>,
    width: usize,
) -> (
    Vec<Line<'static>>,
    std::collections::HashMap<&'static str, u16>,
) {
    let mut out: Vec<Line<'static>> = Vec::with_capacity(raw.len());
    // `row_map[i]` = the new first row for original line `i`.
    let mut row_map: Vec<u16> = Vec::with_capacity(raw.len());
    for line in raw {
        row_map.push(out.len() as u16);
        let plain: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        if width == 0 || plain.chars().count() <= width {
            out.push(line);
            continue;
        }
        let style = line.spans.first().map(|s| s.style).unwrap_or_default();
        match description_split(&plain) {
            Some((prefix, desc)) => {
                let desc_col = prefix.chars().count();
                let wrapped = wrap_hanging(desc.trim_start(), desc_col, desc_col, width);
                for (i, chunk) in wrapped.into_iter().enumerate() {
                    let text = if i == 0 {
                        format!("{prefix}{chunk}")
                    } else {
                        chunk
                    };
                    out.push(Line::from(Span::styled(text, style)));
                }
            }
            None => {
                for chunk in wrap_hanging(&plain, 0, 0, width) {
                    out.push(Line::from(Span::styled(chunk, style)));
                }
            }
        }
    }
    let mut new_anchors = std::collections::HashMap::new();
    for (k, v) in anchors {
        if let Some(&mapped) = row_map.get(v as usize) {
            new_anchors.insert(k, mapped);
        }
    }
    (out, new_anchors)
}

/// Split a help-body line into `(prefix, description)` at the first
/// run of two-or-more spaces that follows the line's leading indent —
/// that gap is the column where a `key    description` row's
/// description text begins. `prefix` includes the gap, so its char
/// count is the column continuation lines should hang from. Returns
/// `None` when no such gap exists (headings, blank lines, one-word
/// rows) — those get wrapped with no hanging indent instead.
fn description_split(s: &str) -> Option<(String, String)> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() && chars[i] == ' ' {
        i += 1;
    }
    let mut j = i;
    while j < chars.len() {
        if chars[j] == ' ' {
            let run_start = j;
            while j < chars.len() && chars[j] == ' ' {
                j += 1;
            }
            if j - run_start >= 2 {
                let prefix: String = chars[..j].iter().collect();
                let desc: String = chars[j..].iter().collect();
                return Some((prefix, desc));
            }
        } else {
            j += 1;
        }
    }
    None
}

/// Word-wrap `text` to `width` columns with a hanging indent: the
/// first output line reserves `first_indent` columns for the wrap
/// decision (but carries no literal padding — the caller supplies its
/// own prefix, e.g. the help line's key), and continuation lines are
/// both budgeted against `cont_indent` and literally prefixed with
/// that many spaces so they line up under the description column. A
/// single word wider than its line's budget is kept whole rather than
/// dropped or split.
fn wrap_hanging(text: &str, first_indent: usize, cont_indent: usize, width: usize) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return vec![String::new()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in words {
        let indent = if out.is_empty() {
            first_indent
        } else {
            cont_indent
        };
        let budget = width.saturating_sub(indent).max(1);
        let wlen = word.chars().count();
        if cur.is_empty() {
            cur.push_str(word);
        } else if cur.chars().count() + 1 + wlen <= budget {
            cur.push(' ');
            cur.push_str(word);
        } else {
            out.push(std::mem::take(&mut cur));
            cur.push_str(word);
        }
    }
    out.push(cur);
    for (i, line) in out.iter_mut().enumerate() {
        if i > 0 {
            *line = format!("{}{}", " ".repeat(cont_indent), line);
        }
    }
    out
}

/// A horizontal divider `width` columns wide, using `├`/`┤` at the
/// ends so it joins an outer border when drawn across its two border
/// columns. Degrades gracefully for pathologically narrow widths.
fn divider_line(width: usize) -> String {
    match width {
        0 => String::new(),
        1 => "├".to_string(),
        n => format!("├{}┤", "─".repeat(n - 2)),
    }
}

/// Helper: push a `label: value` line pair, wrapping the value
/// to `width`. Label is muted, value is wrapped onto continuation
/// lines indented under the label.
fn push_kv(
    lines: &mut Vec<Line<'static>>,
    theme: &crate::theme::Theme,
    label: &'static str,
    value: &str,
    width: usize,
) {
    let prefix = format!("{label:>11}: ");
    let value_width = width.saturating_sub(prefix.chars().count()).max(20);
    let wrapped = wrap_value(value, value_width);
    let mut first = true;
    for chunk in wrapped {
        let p = if first {
            first = false;
            prefix.clone()
        } else {
            " ".repeat(prefix.chars().count())
        };
        lines.push(Line::from(vec![
            Span::styled(p, Style::default().fg(theme.muted)),
            Span::styled(chunk, Style::default().fg(theme.text)),
        ]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_hanging_line_that_fits_is_unchanged() {
        assert_eq!(wrap_hanging("quit", 18, 18, 80), vec!["quit".to_string()]);
    }

    #[test]
    fn wrap_hanging_wraps_at_a_word_boundary() {
        // width 20, first_indent 0: "quit (esc here is a" (19 chars) is
        // the widest prefix that fits — the next word ("no-op)") would
        // push it past 20, so it starts a new line.
        let got = wrap_hanging("quit (esc here is a no-op)", 0, 4, 20);
        assert_eq!(got[0], "quit (esc here is a");
        assert_eq!(got.len(), 2);
        assert_eq!(got[1], "    no-op)");
    }

    #[test]
    fn wrap_hanging_continuation_carries_the_indent() {
        let got = wrap_hanging("one two three four five", 0, 6, 10);
        assert!(got.len() > 1);
        for line in &got[1..] {
            assert!(line.starts_with("      "), "line {line:?} missing indent");
        }
    }

    #[test]
    fn wrap_hanging_single_long_word_is_not_lost() {
        let got = wrap_hanging("supercalifragilisticexpialidocious", 0, 4, 10);
        assert_eq!(got, vec!["supercalifragilisticexpialidocious".to_string()]);
    }

    #[test]
    fn wrap_hanging_empty_text_yields_one_empty_line() {
        assert_eq!(wrap_hanging("", 0, 4, 10), vec![String::new()]);
    }

    #[test]
    fn description_split_finds_the_gap_after_the_key() {
        let (prefix, desc) = description_split("    q             quit now").unwrap();
        assert_eq!(prefix.chars().count(), 18);
        assert_eq!(desc, "quit now");
    }

    #[test]
    fn description_split_none_when_only_single_spaces() {
        assert_eq!(description_split("a b c d"), None);
    }

    #[test]
    fn abbreviate_state_shortens_known_long_states() {
        assert_eq!(abbreviate_state("idle in transaction"), "idle in tx");
        assert_eq!(
            abbreviate_state("idle in transaction (aborted)"),
            "idle in tx (aborted)"
        );
        assert_eq!(abbreviate_state("fastpath function call"), "fastpath");
    }

    #[test]
    fn abbreviate_state_passes_through_unknown_states() {
        assert_eq!(abbreviate_state("active"), "active");
        assert_eq!(abbreviate_state("idle"), "idle");
    }

    #[test]
    fn sessions_state_col_width_floors_at_the_header_width() {
        // Both states are shorter than "state" (5 chars) — the column
        // still needs to fit the header label.
        assert_eq!(sessions_state_col_width(["idle", ""].into_iter()), 5);
    }

    #[test]
    fn sessions_state_col_width_grows_for_a_long_abbreviated_state() {
        // "idle in transaction" -> "idle in tx" (10 chars) — wider than
        // the 5-char header floor.
        assert_eq!(
            sessions_state_col_width(["active", "idle in transaction"].into_iter()),
            10
        );
    }

    #[test]
    fn sessions_state_col_width_caps_at_22() {
        assert_eq!(
            sessions_state_col_width(
                ["a very extremely long and unusual custom state value"].into_iter()
            ),
            22
        );
    }

    #[test]
    fn divider_line_joins_at_both_ends() {
        assert_eq!(divider_line(6), "├────┤");
    }

    #[test]
    fn divider_line_minimum_widths_degrade_gracefully() {
        assert_eq!(divider_line(0), "");
        assert_eq!(divider_line(1), "├");
        assert_eq!(divider_line(2), "├┤");
    }

    #[test]
    fn wrap_help_lines_keeps_short_lines_untouched_and_remaps_anchors() {
        let raw = vec![
            Line::from(Span::raw("heading")),
            Line::from(Span::raw(
                "    q             a description long enough to need wrapping across lines",
            )),
        ];
        let mut anchors = std::collections::HashMap::new();
        anchors.insert("sect", 0u16);
        let (lines, new_anchors) = wrap_help_lines(raw, anchors, 30);
        assert_eq!(lines[0].spans[0].content.as_ref(), "heading");
        assert!(lines.len() > 2, "expected the long line to wrap");
        assert_eq!(new_anchors.get("sect"), Some(&0));
        // Every continuation line of the wrapped row is indented to the
        // description column (18), not column 0.
        for line in &lines[2..] {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                text.starts_with(&" ".repeat(18)),
                "continuation not hanging-indented: {text:?}"
            );
        }
    }
}
