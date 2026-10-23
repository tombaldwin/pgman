use super::*;

/// Fit `text` to `width` display columns, marking a cut with `…`
/// styled in `theme.accent` (bold) — the same truncation `grid.rs`
/// gives the results table, so a clipped tree/detail row reads the
/// same way a clipped grid cell does instead of just running off the
/// edge of its pane.
fn clamped_line(text: &str, style: Style, width: usize, theme: &Theme) -> Line<'static> {
    let (kept, marker) = grid::truncate_cell_parts(text, width);
    if marker.is_empty() {
        Line::from(Span::styled(kept, style))
    } else {
        Line::from(vec![
            Span::styled(kept, style),
            Span::styled(marker, style.fg(theme.accent).add_modifier(Modifier::BOLD)),
        ])
    }
}

/// What kind of index `ix` is, in a word or two. Pure.
pub(crate) fn index_kind(ix: &crate::query::schema::IndexMeta) -> &'static str {
    match (ix.primary, ix.unique) {
        (true, _) => "primary key",
        (false, true) => "unique",
        (false, false) => "index",
    }
}

/// Schema browser modal. Two panes inside a centered overlay:
/// the left holds the schema → table tree, the right holds the
/// columns / constraints for the focused table (or a one-line
/// summary for a focused schema). Static — driven entirely by the
/// schema cache; no live queries.
pub(super) fn draw_schema_browser(f: &mut Frame, area: Rect, app: &App) {
    use crate::app::SchemaBrowserRow;
    let theme = &app.theme;
    let rows = app.flattened_schema_browser();
    let popup = centered_pct(area, 88, 80);
    f.render_widget(Clear, popup);

    let block = crate::ui::chrome::panel_block(theme)
        .border_style(Style::default().fg(theme.border_active))
        .title(crate::ui::chrome::panel_title(
            theme,
            " schema browser — q / esc close ",
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    // Split tree (left) / details (right). A straight 40/60 percentage
    // split leaves the tree unusably narrow at 80 columns (~27 cells,
    // not enough for a nested column row before it starts eating the
    // right pane's border) — so the tree gets a fixed floor and the
    // details pane takes whatever's left, mirroring how ebman sizes
    // its table columns from a computed minimum rather than a bare
    // percentage.
    const TREE_MIN_WIDTH: u16 = 28;
    let left_width = ((inner.width as u32 * 40 / 100) as u16)
        .max(TREE_MIN_WIDTH)
        .min(inner.width);
    let right_width = inner.width.saturating_sub(left_width);
    let left = Rect {
        x: inner.x,
        y: inner.y,
        width: left_width,
        height: inner.height,
    };
    let right = Rect {
        x: inner.x + left_width,
        y: inner.y,
        width: right_width,
        height: inner.height,
    };

    // Left: scrollable tree.
    let visible_h = left.height as usize;
    // Reserve a one-column gutter at the tree pane's right edge: the
    // two panes sit flush against each other with no border between
    // them, so a row that fills its pane exactly would otherwise
    // touch the detail pane's text with nothing to show the two are
    // separate.
    let tree_width = left.width.saturating_sub(1) as usize;
    let scroll = scroll_offset(app.schema_browser.cursor, visible_h);
    let mut lines: Vec<Line> = Vec::new();
    for (i, row) in rows.iter().enumerate().skip(scroll).take(visible_h) {
        let is_focus = i == app.schema_browser.cursor;
        let style = if is_focus {
            Style::default()
                .fg(theme.text)
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        let text = match row {
            SchemaBrowserRow::Schema {
                name,
                expanded,
                table_count,
            } => {
                let marker = if *expanded { "▼" } else { "▶" };
                format!("{marker} {name}  ({table_count})")
            }
            SchemaBrowserRow::Table {
                name,
                expanded,
                column_count,
                constraint_count,
                ..
            } => {
                let marker = if *expanded { "▼" } else { "▶" };
                format!("  {marker} {name}  ({column_count} col, {constraint_count} cons)")
            }
            SchemaBrowserRow::Column {
                schema,
                table,
                name,
            } => {
                // Render `· id : integer NOT NULL` when the cache
                // has type metadata (post-fetch); fall back to
                // `· name` when it doesn't.
                let meta = app
                    .schema_cache
                    .columns_meta_by_table
                    .get(&(schema.clone(), table.clone()))
                    .and_then(|v| v.iter().find(|m| m.name == *name));
                match meta {
                    Some(m) if !m.type_name.is_empty() => {
                        let nn = if m.not_null { " NOT NULL" } else { "" };
                        format!("      · {name} : {}{nn}", m.type_name)
                    }
                    _ => format!("      · {name}"),
                }
            }
            SchemaBrowserRow::Constraint { name, .. } => format!("      ◆ {name}"),
            SchemaBrowserRow::Index { name, .. } => format!("      ⌁ {name}"),
            SchemaBrowserRow::Partitions {
                count, expanded, ..
            } => {
                let marker = if *expanded { "▼" } else { "▶" };
                format!(
                    "    {marker} {count} partition{}",
                    if *count == 1 { "" } else { "s" }
                )
            }
            SchemaBrowserRow::Partition { name, .. } => format!("        · {name}"),
        };
        lines.push(clamped_line(&text, style, tree_width, theme));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), left);

    // Right: details for the focused row.
    let detail_width = right.width as usize;
    let mut right_lines: Vec<Line> = Vec::new();
    match rows.get(app.schema_browser.cursor) {
        Some(SchemaBrowserRow::Schema {
            name, table_count, ..
        }) => {
            right_lines.push(clamped_line(
                &format!("schema: {name}"),
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
                detail_width,
                theme,
            ));
            right_lines.push(Line::from(""));
            // Under a filter the count is of the tables it left.
            let count = match app.schema_browser.filter.as_deref() {
                Some(pat) if !pat.is_empty() => format!("{table_count} table(s) matching /{pat}"),
                _ => format!("{table_count} table(s)"),
            };
            right_lines.push(clamped_line(
                &count,
                Style::default().fg(theme.text),
                detail_width,
                theme,
            ));
            right_lines.push(Line::from(""));
            right_lines.push(clamped_line(
                "enter to expand — then arrow / j/k into the tables",
                Style::default().fg(theme.muted),
                detail_width,
                theme,
            ));
        }
        Some(SchemaBrowserRow::Column {
            schema,
            table,
            name,
        }) => {
            right_lines.push(clamped_line(
                &format!("{schema}.{table}.{name}"),
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
                detail_width,
                theme,
            ));
            right_lines.push(Line::from(""));
            let meta = app
                .schema_cache
                .columns_meta_by_table
                .get(&(schema.clone(), table.clone()))
                .and_then(|v| v.iter().find(|m| m.name == *name));
            match meta {
                Some(m) if !m.type_name.is_empty() => {
                    right_lines.push(clamped_line(
                        &format!("type:  {}", m.type_name),
                        Style::default().fg(theme.text),
                        detail_width,
                        theme,
                    ));
                    right_lines.push(clamped_line(
                        &format!(
                            "nullable: {}",
                            if m.not_null { "NO (NOT NULL)" } else { "YES" }
                        ),
                        Style::default().fg(theme.text),
                        detail_width,
                        theme,
                    ));
                }
                _ => {
                    right_lines.push(clamped_line(
                        "column · type info unavailable (older cache?)",
                        Style::default().fg(theme.muted),
                        detail_width,
                        theme,
                    ));
                }
            }
        }
        Some(SchemaBrowserRow::Constraint {
            schema,
            table,
            name,
        }) => {
            right_lines.push(clamped_line(
                &format!("{schema}.{table} · {name}"),
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
                detail_width,
                theme,
            ));
            right_lines.push(Line::from(""));
            right_lines.push(clamped_line(
                "unique / primary-key constraint",
                Style::default().fg(theme.muted),
                detail_width,
                theme,
            ));
        }
        Some(SchemaBrowserRow::Index {
            schema,
            table,
            name,
        }) => {
            right_lines.push(clamped_line(
                &format!("{schema}.{table} · {name}"),
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
                detail_width,
                theme,
            ));
            right_lines.push(Line::from(""));
            let meta = app
                .schema_cache
                .indexes_by_table
                .get(&(schema.clone(), table.clone()))
                .and_then(|v| v.iter().find(|m| m.name == *name));
            if let Some(m) = meta {
                right_lines.push(clamped_line(
                    index_kind(m),
                    Style::default().fg(theme.text),
                    detail_width,
                    theme,
                ));
                right_lines.push(Line::from(""));
                // The definition is the one thing here worth reading in
                // full, so it wraps instead of being cut.
                for chunk in wrap_value(&m.definition, detail_width.max(1)) {
                    right_lines.push(Line::from(Span::styled(
                        chunk,
                        Style::default().fg(theme.muted),
                    )));
                }
            }
        }
        Some(SchemaBrowserRow::Partitions {
            schema,
            table,
            count,
            ..
        }) => {
            right_lines.push(clamped_line(
                &format!("{schema}.{table}"),
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
                detail_width,
                theme,
            ));
            right_lines.push(Line::from(""));
            right_lines.push(clamped_line(
                &format!("partitioned into {count}"),
                Style::default().fg(theme.text),
                detail_width,
                theme,
            ));
            right_lines.push(Line::from(""));
            right_lines.push(clamped_line(
                "enter to list them",
                Style::default().fg(theme.muted),
                detail_width,
                theme,
            ));
        }
        Some(SchemaBrowserRow::Partition {
            schema,
            table,
            name,
        }) => {
            right_lines.push(clamped_line(
                &format!("{schema}.{name}"),
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
                detail_width,
                theme,
            ));
            right_lines.push(Line::from(""));
            right_lines.push(clamped_line(
                &format!("a partition of {table}"),
                Style::default().fg(theme.text),
                detail_width,
                theme,
            ));
            if let Some(sz) = app
                .schema_cache
                .table_sizes
                .get(&(schema.clone(), name.clone()))
            {
                right_lines.push(clamped_line(
                    &format!(
                        "size: {} (total {})",
                        crate::query::schema::format_bytes(sz.table_bytes),
                        crate::query::schema::format_bytes(sz.total_bytes)
                    ),
                    Style::default().fg(theme.text),
                    detail_width,
                    theme,
                ));
            }
            right_lines.push(Line::from(""));
            right_lines.push(clamped_line(
                "s SELECT · i INSERT · p preview rows · d DDL",
                Style::default().fg(theme.muted),
                detail_width,
                theme,
            ));
        }
        Some(SchemaBrowserRow::Table { schema, name, .. }) => {
            right_lines.push(clamped_line(
                &format!("{schema}.{name}"),
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
                detail_width,
                theme,
            ));
            right_lines.push(Line::from(""));
            // Size info (third-pass fetch). Missing entry =
            // permission gap on pg_relation_size; render nothing.
            if let Some(sz) = app
                .schema_cache
                .table_sizes
                .get(&(schema.clone(), name.clone()))
            {
                right_lines.push(clamped_line(
                    &format!(
                        "size: total {}  ·  heap {}",
                        crate::query::schema::format_bytes(sz.total_bytes),
                        crate::query::schema::format_bytes(sz.table_bytes),
                    ),
                    Style::default().fg(theme.muted),
                    detail_width,
                    theme,
                ));
                right_lines.push(Line::from(""));
            }
            // Columns from the cache (ordered by attnum).
            let cols = app
                .schema_cache
                .columns_by_table
                .get(&(schema.clone(), name.clone()))
                .cloned()
                .unwrap_or_default();
            right_lines.push(clamped_line(
                &format!("columns ({})", cols.len()),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
                detail_width,
                theme,
            ));
            for c in &cols {
                right_lines.push(clamped_line(
                    &format!("  · {c}"),
                    Style::default().fg(theme.text),
                    detail_width,
                    theme,
                ));
            }
            // Constraints for this table.
            let cons: Vec<&crate::query::schema::ConstraintMeta> = app
                .schema_cache
                .constraints
                .iter()
                .filter(|c| {
                    c.schema.eq_ignore_ascii_case(schema) && c.table.eq_ignore_ascii_case(name)
                })
                .collect();
            if !cons.is_empty() {
                right_lines.push(Line::from(""));
                right_lines.push(clamped_line(
                    &format!("constraints ({})", cons.len()),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                    detail_width,
                    theme,
                ));
                for c in cons {
                    right_lines.push(clamped_line(
                        &format!("  · {}", c.name),
                        Style::default().fg(theme.text),
                        detail_width,
                        theme,
                    ));
                }
            }
            let indexes = app
                .schema_cache
                .indexes_by_table
                .get(&(schema.clone(), name.clone()))
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            if !indexes.is_empty() {
                right_lines.push(Line::from(""));
                right_lines.push(clamped_line(
                    &format!("indexes ({})", indexes.len()),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                    detail_width,
                    theme,
                ));
                for ix in indexes {
                    right_lines.push(clamped_line(
                        &format!("  · {}  {}", ix.name, index_kind(ix)),
                        Style::default().fg(theme.text),
                        detail_width,
                        theme,
                    ));
                }
            }
        }
        None => {
            right_lines.push(Line::from(Span::styled(
                "no schemas loaded",
                Style::default().fg(theme.muted),
            )));
        }
    }
    f.render_widget(Paragraph::new(Text::from(right_lines)), right);
}

/// Schema-lint panel (the "wizard" — `W` from Normal). Top half:
/// scrollable list of findings, severity-coloured. Bottom half:
/// detail strip for the focused finding with its full `detail`
/// text and any SQL suggestion.
pub(super) fn draw_schema_lint(f: &mut Frame, area: Rect, app: &App) {
    use crate::query::lint::Severity;
    let theme = &app.theme;
    let popup = centered_pct(area, 92, 80);
    f.render_widget(Clear, popup);
    let block = crate::ui::chrome::panel_block(theme)
        .border_style(Style::default().fg(theme.border_active))
        .title(crate::ui::chrome::panel_title(
            theme,
            " schema wizard — y yank suggestion · r refresh · q close ",
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    if app.schema_lint.findings.is_empty() {
        let msg = app
            .last_status
            .clone()
            .unwrap_or_else(|| "no findings — schema looks clean".into());
        f.render_widget(
            Paragraph::new(Text::from(msg)).style(Style::default().fg(theme.muted)),
            inner,
        );
        return;
    }

    // Detail strip for the focused finding: 7 rows for a lint finding;
    // an advisor finding's evidence and setting get what they need, up
    // to all but the list's 5.
    let focused = &app.schema_lint.findings[app
        .schema_lint
        .cursor
        .min(app.schema_lint.findings.len() - 1)];
    let detail_lines = lint_detail_lines(focused, inner.width.saturating_sub(4) as usize, theme);
    let detail_h = (detail_lines.len() as u16)
        .max(7)
        .min(inner.height.saturating_sub(5).max(7));
    let split = Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(detail_h)])
        .split(inner);
    let top = split[0];
    let detail = split[1];

    let visible_h = top.height as usize;
    let scroll = scroll_offset(app.schema_lint.cursor, visible_h);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(
            "  {:<5}  {:<7}  {:<48}  {}",
            "SEV", "CODE", "object", "title"
        ),
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    )));
    for (i, finding) in app
        .schema_lint
        .findings
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_h.saturating_sub(1))
    {
        let is_focus = i == app.schema_lint.cursor;
        let sev_color = match finding.severity {
            Severity::High => theme.health_red,
            Severity::Medium => theme.health_yellow,
            Severity::Low => theme.muted,
        };
        let base_style = if is_focus {
            Style::default()
                .fg(theme.text)
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        // `object` is a fixed 48-column field; `title` takes whatever's
        // left of the row. Both get cut with `…` (via the same
        // truncator the grid uses) rather than a raw character clip —
        // a cut with no marker reads as if the text just ends there.
        let object = crate::grid::truncate_cell(&finding.object, 48);
        // Fixed prefix width before `title` starts: "  " (2) + sev (5)
        // + "  {code:<7}  " (11) + "{object:<48}  " (50).
        const PREFIX_WIDTH: usize = 2 + 5 + 11 + 50;
        let title_width = (top.width as usize).saturating_sub(PREFIX_WIDTH);
        let title = crate::grid::truncate_cell(&finding.title, title_width);
        let spans = vec![
            Span::styled("  ", base_style),
            Span::styled(
                format!("{:<5}", finding.severity.label()),
                base_style.fg(sev_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {:<7}  ", finding.code), base_style),
            Span::styled(format!("{:<48}  ", object), base_style),
            Span::styled(title, base_style),
        ];
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), top);

    f.render_widget(Paragraph::new(Text::from(detail_lines)), detail);
}

/// The wizard's detail strip for `finding`, `width` columns of text.
/// A lint finding: title, object, detail, suggestion. An advisor
/// finding puts what to do first, so it shows on a small terminal —
/// the setting and the suggestion — then the evidence behind it (the
/// window and the numbers), the reasoning, the RDS / Aurora line, and
/// the prose. Its object is already in the list row. Pure.
pub(crate) fn lint_detail_lines(
    finding: &crate::query::lint::Finding,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let styled = |text: String, style: Style| Line::from(Span::styled(text, style));
    let text = Style::default().fg(theme.text);
    let muted = Style::default().fg(theme.muted);
    let accent = Style::default().fg(theme.accent);
    out.push(styled(
        format!("  {} · {}", finding.code, finding.title),
        Style::default()
            .fg(theme.title)
            .add_modifier(Modifier::BOLD),
    ));
    let Some(advice) = finding.advice.as_deref() else {
        out.push(styled(format!("  object: {}", finding.object), muted));
        for chunk in wrap_value(&finding.detail, width) {
            out.push(styled(format!("  {chunk}"), text));
        }
        if let Some(s) = &finding.suggestion {
            out.push(styled(format!("  suggest: {s}"), accent));
        }
        return out;
    };
    // Hanging indent: the first row carries the label, the rest line
    // up under the text.
    let wrapped = |label: &str, body: &str, style: Style, out: &mut Vec<Line<'static>>| {
        let pad = " ".repeat(2 + label.chars().count());
        for (i, row) in wrap_words(body, width.saturating_sub(label.chars().count()))
            .into_iter()
            .enumerate()
        {
            let lead = if i == 0 {
                format!("  {label}")
            } else {
                pad.clone()
            };
            out.push(styled(format!("{lead}{row}"), style));
        }
    };
    if let Some(ch) = &advice.change {
        let restart = if ch.restart {
            "needs a restart"
        } else {
            "no restart"
        };
        wrapped(
            "setting:  ",
            &format!("{} {} → {} ({restart})", ch.name, ch.current, ch.suggested),
            text,
            &mut out,
        );
    }
    if let Some(s) = &finding.suggestion {
        wrapped("suggest:  ", s, accent, &mut out);
    }
    wrapped("evidence: ", &advice.window, muted, &mut out);
    for e in &advice.evidence {
        wrapped("  ", e, text, &mut out);
    }
    if let Some(ch) = &advice.change {
        wrapped("because:  ", &ch.reasoning, text, &mut out);
        wrapped("RDS:      ", &ch.rds, muted, &mut out);
    }
    wrapped("why:      ", &finding.detail, text, &mut out);
    out
}
