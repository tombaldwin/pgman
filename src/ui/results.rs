use super::*;

pub(super) fn draw_grid(f: &mut Frame, area: Rect, app: &mut App) {
    let grid = &app.grid;
    let theme = &app.theme;
    let mut widths = grid::column_widths(grid, 48);
    // The sort marker (` ▲` / ` ▼`) is appended to the header cell
    // BEFORE width clamping, so columns hosting the sort key need
    // two extra chars of room. Without this the marker would be
    // truncated off and the operator would think nothing happened
    // when they pressed `s`.
    if let Some((col, _)) = app.grid_view.sort {
        if let Some(w) = widths.get_mut(col) {
            *w = (*w + 2).min(48);
        }
    }

    // Header: bold the column under the cursor; append a ▲ / ▼ to
    // whichever column is currently the sort key.
    let header_cells: Vec<Cell> = grid
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let sort_marker = match app.grid_view.sort {
                Some((col, true)) if col == i => " ▲",
                Some((col, false)) if col == i => " ▼",
                _ => "",
            };
            let text = format!("{c}{sort_marker}");
            let mut style = Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD);
            // Focused column reverses to make column nav visible at
            // a glance. App mode doesn't matter — h/l is only useful
            // in Normal mode but the indicator persists so the
            // operator knows "the sort key will target this column".
            if i == app.grid_view.col_cursor {
                style = style.add_modifier(Modifier::REVERSED);
            }
            Cell::from(text).style(style)
        })
        .collect();
    let header = Row::new(header_cells);

    // Walk only the visible rows (post-filter, post-sort). When no
    // filter has ever been applied, `grid_view.visible_rows` was
    // initialised to `0..rows.len()` so this branch handles the
    // unfiltered path too.
    let rows: Vec<Row> = app
        .grid_view
        .visible_rows
        .iter()
        .filter_map(|&i| grid.rows.get(i))
        .map(|r| {
            Row::new(r.iter().enumerate().map(|(i, c)| {
                let w = widths.get(i).copied().unwrap_or(0);
                let (kept, marker) = grid::truncate_cell_parts(c, w);
                if marker.is_empty() {
                    Cell::from(kept)
                } else {
                    // Style the `…` truncation marker with `accent`
                    // so the operator sees the cell has more behind
                    // it (RowDetail / CellDetail reveals the rest).
                    Cell::from(Line::from(vec![
                        Span::raw(kept),
                        Span::styled(
                            marker,
                            Style::default()
                                .fg(theme.accent)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]))
                }
            }))
        })
        .collect();
    let constraints: Vec<Constraint> = widths
        .iter()
        .map(|w| Constraint::Length(*w as u16))
        .collect();
    let visible = app.grid_view.visible_rows.len();
    let total = grid.row_count();
    let cap = if grid.truncated {
        format!(" · capped at {}", crate::grid::MAX_ROWS)
    } else {
        String::new()
    };
    let title = if app.grid_view.filter.is_some() && visible != total {
        format!(" result · {visible}/{total} row(s) (filtered){cap} ")
    } else {
        format!(" result · {total} row(s){cap} ")
    };
    let table = Table::new(rows, constraints)
        .header(header)
        .column_spacing(2)
        .row_highlight_style(Style::default().bg(theme.row_selected_bg))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border_idle))
                .title(Span::styled(title, Style::default().fg(theme.title))),
        );
    f.render_stateful_widget(table, area, &mut app.grid_state);
}

/// Expanded view of the selected row — one labelled value per column, with
/// long values wrapped to fit the popup width. Inspired by psql's `\x`
/// expanded-display mode. j/k moves a field cursor; `y` yanks the focused
/// value to the system clipboard.
pub(super) fn draw_row_detail(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = &app.theme;
    let Some(idx) = app.selected_grid_row_idx() else {
        return;
    };
    let Some(row) = app.grid.rows.get(idx).cloned() else {
        return;
    };

    let popup = centered_pct(area, 80, 80);
    // Inside the borders + uniform(1) padding.
    let inner_width = popup.width.saturating_sub(4) as usize;
    let inner_height = popup.height.saturating_sub(4);

    // Label column = widest column name, capped so a runaway name doesn't
    // squeeze the value column off-screen.
    let label_max = 32usize;
    let label_width = app
        .grid
        .columns
        .iter()
        .map(|c| c.chars().count())
        .max()
        .unwrap_or(0)
        .min(label_max);
    let sep = " │ ";
    let label_plus_sep = label_width + sep.len();
    let value_width = inner_width.saturating_sub(label_plus_sep).max(1);

    let layout = build_field_layout(&app.grid.columns, &row, label_width, value_width);
    // Update the field-cursor bound so the key handler can clamp against
    // what's actually rendered. Also push the clamped focus *back* to
    // app state — otherwise after the grid shrinks (e.g. QueryOk replaces
    // a wide row with a narrow one while RowDetail is open) the visual
    // highlight clamps but `yank_focused_field` / `open_cell_detail`
    // still see the pre-clamp index, silently no-op-ing.
    app.row_detail.field_count = layout.len();
    let focus = app.row_detail.field.min(layout.len().saturating_sub(1));
    app.row_detail.field = focus;

    let label_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let sep_style = Style::default().fg(theme.border_idle);
    let value_style = Style::default().fg(theme.text);
    let null_style = Style::default()
        .fg(theme.muted)
        .add_modifier(Modifier::ITALIC);
    let focus_bg = theme.row_selected_bg;

    let mut lines: Vec<Line> = Vec::new();
    let mut field_line_counts: Vec<u16> = Vec::with_capacity(layout.len());
    for (field_idx, field) in layout.iter().enumerate() {
        let is_focus = field_idx == focus;
        let value_span_style_base = if field.is_empty {
            null_style
        } else {
            value_style
        };
        let (label_style_eff, sep_style_eff, value_style_eff) = if is_focus {
            (
                label_style.bg(focus_bg),
                sep_style.bg(focus_bg),
                value_span_style_base.bg(focus_bg),
            )
        } else {
            (label_style, sep_style, value_span_style_base)
        };
        let count = field.values.len() as u16;
        field_line_counts.push(count);
        for (i, vline) in field.values.iter().enumerate() {
            let label_text = if i == 0 {
                field.label.clone()
            } else {
                // Continuation rows: blank in the label column so the eye
                // tracks values that wrap across multiple lines.
                " ".repeat(label_width)
            };
            // Pad the value out to the full content width so the focus
            // highlight extends across the whole row, not just the text.
            let padded_value = format!("{:<width$}", vline, width = value_width);
            lines.push(Line::from(vec![
                Span::styled(label_text, label_style_eff),
                Span::styled(sep, sep_style_eff),
                Span::styled(padded_value, value_style_eff),
            ]));
        }
    }

    let total_lines = lines.len() as u16;
    let max_scroll = total_lines.saturating_sub(inner_height);
    app.row_detail.max_scroll = max_scroll;
    // Auto-scroll so the focused field is visible, then clamp.
    let effective_scroll = auto_scroll_to_field(
        &field_line_counts,
        focus,
        app.row_detail.scroll,
        inner_height,
        max_scroll,
    );
    app.row_detail.scroll = effective_scroll;

    let title = format!(
        " row {} of {} · field {}/{} ",
        idx + 1,
        app.grid.row_count(),
        focus + 1,
        layout.len().max(1)
    );
    f.render_widget(Clear, popup);
    let body = Paragraph::new(Text::from(lines))
        .scroll((effective_scroll, 0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border_active))
                .padding(Padding::uniform(1))
                .title(Span::styled(title, Style::default().fg(theme.title))),
        );
    f.render_widget(body, popup);

    // Same scroll-indicator pattern as draw_help — overlay the first/last
    // visible body rows when content extends past the viewport.
    let hint_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    if effective_scroll > 0 {
        let row_rect = Rect {
            x: popup.x + 2,
            y: popup.y + 2,
            width: popup.width.saturating_sub(4),
            height: 1,
        };
        f.render_widget(Clear, row_rect);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("↑ {} more above", effective_scroll),
                hint_style,
            ))),
            row_rect,
        );
    }
    if effective_scroll < max_scroll {
        let row_rect = Rect {
            x: popup.x + 2,
            y: popup.y + popup.height.saturating_sub(3),
            width: popup.width.saturating_sub(4),
            height: 1,
        };
        f.render_widget(Clear, row_rect);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("↓ {} more below", max_scroll - effective_scroll),
                hint_style,
            ))),
            row_rect,
        );
    }
}

/// Per-cell zoom: a focused view of `(row_detail_field)` from the
/// currently-selected row. Larger popup than RowDetail so a big JSON
/// value gets actual space; scroll independently with j/k/g/G/PageUp/Down.
pub(super) fn draw_cell_detail(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = &app.theme;
    let Some(idx) = app.selected_grid_row_idx() else {
        return;
    };
    let Some(row) = app.grid.rows.get(idx).cloned() else {
        return;
    };
    let field = app.row_detail.field;
    let column = app.grid.columns.get(field).cloned().unwrap_or_default();
    let value = row.get(field).cloned().unwrap_or_default();

    // Nest inside the row-detail popup so the zoom reads as drilling in,
    // not a new context. 90% of the screen so big JSON gets room.
    let popup = centered_pct(area, 90, 90);
    let inner_width = popup.width.saturating_sub(4) as usize; // borders + uniform(1) pad
    let inner_height = popup.height.saturating_sub(4);
    let is_empty = value.is_empty();

    let is_json = !app.cell_detail.json_rows.is_empty();
    let body_lines: Vec<Line> = if is_json {
        render_json_tree(app, inner_width)
    } else if is_empty {
        vec![Line::from(Span::styled(
            "(empty)",
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        ))]
    } else {
        wrap_value(&value, inner_width)
            .into_iter()
            .map(|l| Line::from(Span::styled(l, Style::default().fg(theme.text))))
            .collect()
    };

    let total_lines = body_lines.len() as u16;
    let max_scroll = total_lines.saturating_sub(inner_height);
    app.cell_detail.max_scroll = max_scroll;
    let effective_scroll = if is_json {
        // Keep the focused tree row visible — auto-scroll like the
        // grid does for its cursor.
        let cursor = app.cell_detail.json_cursor as u16;
        let h = inner_height.max(1);
        let scroll = if cursor < app.cell_detail.scroll {
            cursor
        } else if cursor >= app.cell_detail.scroll + h {
            cursor + 1 - h
        } else {
            app.cell_detail.scroll
        };
        let scroll = scroll.min(max_scroll);
        app.cell_detail.scroll = scroll;
        scroll
    } else {
        app.cell_detail.scroll.min(max_scroll)
    };

    let lines: Vec<Line> = body_lines;

    let title = if is_json {
        format!(
            " {} · row {} of {} · field {}/{} · JSON ",
            column,
            idx + 1,
            app.grid.row_count(),
            field + 1,
            app.row_detail.field_count.max(1)
        )
    } else {
        format!(
            " {} · row {} of {} · field {}/{} ",
            column,
            idx + 1,
            app.grid.row_count(),
            field + 1,
            app.row_detail.field_count.max(1)
        )
    };
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll((effective_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.border_active))
                    .padding(Padding::uniform(1))
                    .title(Span::styled(title, Style::default().fg(theme.title))),
            ),
        popup,
    );
    // Reuse the same up/down "more" indicators as the help / row-detail
    // overlays for visual consistency.
    let hint_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    if effective_scroll > 0 {
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

/// Result-diff overlay (`Mode::ResultDiff`). Renders A-vs-B as a
/// grouped list: removed rows, then changed rows (with per-cell
/// old→new deltas), then added rows. Unchanged rows are only
/// counted in the header.
pub(super) fn draw_result_diff(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let popup = centered_pct(area, 92, 80);
    f.render_widget(Clear, popup);
    let Some(state) = app.diff.active.as_ref() else {
        return;
    };
    let diff = &state.diff;
    let key_desc = match &state.key {
        crate::query::row_diff::RowKey::Columns(cols) => {
            let names: Vec<&str> = cols
                .iter()
                .map(|&i| state.a.columns.get(i).map(String::as_str).unwrap_or("?"))
                .collect();
            format!("key: {}", names.join(", "))
        }
        crate::query::row_diff::RowKey::FullRow => "key: full row".to_string(),
    };
    let title = format!(
        " Result diff — A: {a} ({an} rows) vs B: {b} ({bn} rows) · {key} · r re-pin · c clear · q close ",
        a = state.a.label,
        an = state.a.rows.len(),
        b = state.b_label,
        bn = state.b_rows.len(),
        key = key_desc,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_active))
        .title(Span::styled(title, Style::default().fg(theme.title)));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    // Summary line, always shown.
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            format!("+{} added", diff.added.len()),
            Style::default().fg(theme.health_green),
        ),
        Span::styled("   ", Style::default()),
        Span::styled(
            format!("-{} removed", diff.removed.len()),
            Style::default().fg(theme.health_red),
        ),
        Span::styled("   ", Style::default()),
        Span::styled(
            format!("~{} changed", diff.changed.len()),
            Style::default().fg(theme.health_yellow),
        ),
        Span::styled(
            format!("   ={} unchanged", diff.unchanged),
            Style::default().fg(theme.muted),
        ),
    ]));

    if diff.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "no differences — A and B match.",
            Style::default().fg(theme.health_green),
        )));
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
        return;
    }

    // Entry rows are removed, then changed, then added — matching the
    // cursor model in `diff_row_count`. Format only the visible window
    // rather than every diff row each frame: a large diff (a batch
    // UPDATE against its baseline) can carry thousands of rows, and the
    // overlay only ever shows `visible_h` of them.
    let row_w = inner.width.saturating_sub(4) as usize;
    let render_row =
        |row: &[String]| -> String { crate::grid::truncate_cell(&row.join(" | "), row_w) };
    let nr = diff.removed.len();
    let nc = diff.changed.len();
    let total = nr + nc + diff.added.len();
    // Map a flat entry index to its (style, text), touching only the
    // one underlying row it names.
    let fmt_entry = |flat: usize| -> (Style, String) {
        if flat < nr {
            let body = state
                .a
                .rows
                .get(diff.removed[flat])
                .map(|r| render_row(r))
                .unwrap_or_default();
            (Style::default().fg(theme.health_red), format!("- {body}"))
        } else if flat < nr + nc {
            let ch = &diff.changed[flat - nr];
            let deltas: Vec<String> = ch
                .cells
                .iter()
                .map(|c| {
                    let col = state
                        .a
                        .columns
                        .get(c.col)
                        .map(String::as_str)
                        .unwrap_or("?");
                    format!("{col}: {} → {}", c.old, c.new)
                })
                .collect();
            let text = format!("~ [{}] {}", ch.key.join(", "), deltas.join("  ·  "));
            (
                Style::default().fg(theme.health_yellow),
                crate::grid::truncate_cell(&text, row_w),
            )
        } else {
            let body = state
                .b_rows
                .get(diff.added[flat - nr - nc])
                .map(|r| render_row(r))
                .unwrap_or_default();
            (Style::default().fg(theme.health_green), format!("+ {body}"))
        }
    };

    // Reserve the summary line; scroll the entry list under it.
    let visible_h = (inner.height as usize).saturating_sub(2);
    let cursor = app.diff.cursor.min(total.saturating_sub(1));
    let scroll = scroll_offset(cursor, visible_h);
    lines.push(Line::from(""));
    for flat in scroll..(scroll + visible_h).min(total) {
        let (base, text) = fmt_entry(flat);
        let style = if flat == cursor {
            Style::default()
                .fg(theme.text)
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            base
        };
        lines.push(Line::from(Span::styled(format!("  {text}"), style)));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}
