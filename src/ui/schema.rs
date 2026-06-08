use super::*;

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

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_active))
        .title(Span::styled(
            " schema browser — q / esc close ",
            Style::default().fg(theme.title),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let split = Layout::default()
        .direction(ratatui::layout::Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(inner);
    let left = split[0];
    let right = split[1];

    // Left: scrollable tree.
    let visible_h = left.height as usize;
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
        };
        lines.push(Line::from(Span::styled(text, style)));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), left);

    // Right: details for the focused row.
    let mut right_lines: Vec<Line> = Vec::new();
    match rows.get(app.schema_browser.cursor) {
        Some(SchemaBrowserRow::Schema {
            name, table_count, ..
        }) => {
            right_lines.push(Line::from(Span::styled(
                format!("schema: {name}"),
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
            )));
            right_lines.push(Line::from(""));
            right_lines.push(Line::from(format!("{table_count} table(s)")));
            right_lines.push(Line::from(""));
            right_lines.push(Line::from(Span::styled(
                "enter to expand — then arrow / j/k into the tables",
                Style::default().fg(theme.muted),
            )));
        }
        Some(SchemaBrowserRow::Column {
            schema,
            table,
            name,
        }) => {
            right_lines.push(Line::from(Span::styled(
                format!("{schema}.{table}.{name}"),
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
            )));
            right_lines.push(Line::from(""));
            let meta = app
                .schema_cache
                .columns_meta_by_table
                .get(&(schema.clone(), table.clone()))
                .and_then(|v| v.iter().find(|m| m.name == *name));
            match meta {
                Some(m) if !m.type_name.is_empty() => {
                    right_lines.push(Line::from(Span::styled(
                        format!("type:  {}", m.type_name),
                        Style::default().fg(theme.text),
                    )));
                    right_lines.push(Line::from(Span::styled(
                        format!(
                            "nullable: {}",
                            if m.not_null { "NO (NOT NULL)" } else { "YES" }
                        ),
                        Style::default().fg(theme.text),
                    )));
                }
                _ => {
                    right_lines.push(Line::from(Span::styled(
                        "column · type info unavailable (older cache?)",
                        Style::default().fg(theme.muted),
                    )));
                }
            }
        }
        Some(SchemaBrowserRow::Constraint {
            schema,
            table,
            name,
        }) => {
            right_lines.push(Line::from(Span::styled(
                format!("{schema}.{table} · {name}"),
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
            )));
            right_lines.push(Line::from(""));
            right_lines.push(Line::from(Span::styled(
                "unique / primary-key constraint",
                Style::default().fg(theme.muted),
            )));
        }
        Some(SchemaBrowserRow::Table { schema, name, .. }) => {
            right_lines.push(Line::from(Span::styled(
                format!("{schema}.{name}"),
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
            )));
            right_lines.push(Line::from(""));
            // Size info (third-pass fetch). Missing entry =
            // permission gap on pg_relation_size; render nothing.
            if let Some(sz) = app
                .schema_cache
                .table_sizes
                .get(&(schema.clone(), name.clone()))
            {
                right_lines.push(Line::from(Span::styled(
                    format!(
                        "size: total {}  ·  heap {}",
                        crate::query::schema::format_bytes(sz.total_bytes),
                        crate::query::schema::format_bytes(sz.table_bytes),
                    ),
                    Style::default().fg(theme.muted),
                )));
                right_lines.push(Line::from(""));
            }
            // Columns from the cache (ordered by attnum).
            let cols = app
                .schema_cache
                .columns_by_table
                .get(&(schema.clone(), name.clone()))
                .cloned()
                .unwrap_or_default();
            right_lines.push(Line::from(Span::styled(
                format!("columns ({})", cols.len()),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )));
            for c in &cols {
                right_lines.push(Line::from(format!("  · {c}")));
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
                right_lines.push(Line::from(Span::styled(
                    format!("constraints ({})", cons.len()),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                )));
                for c in cons {
                    right_lines.push(Line::from(format!("  · {}", c.name)));
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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_active))
        .title(Span::styled(
            " schema wizard — y yank suggestion · r refresh · q close ",
            Style::default().fg(theme.title),
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

    let split = Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(7)])
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
        let object = if finding.object.chars().count() > 48 {
            let mut s: String = finding.object.chars().take(47).collect();
            s.push('…');
            s
        } else {
            finding.object.clone()
        };
        let spans = vec![
            Span::styled("  ", base_style),
            Span::styled(
                format!("{:<5}", finding.severity.label()),
                base_style.fg(sev_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {:<7}  ", finding.code), base_style),
            Span::styled(format!("{:<48}  ", object), base_style),
            Span::styled(finding.title.clone(), base_style),
        ];
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), top);

    // Detail strip for the focused finding.
    let focused = &app.schema_lint.findings[app
        .schema_lint
        .cursor
        .min(app.schema_lint.findings.len() - 1)];
    let mut detail_lines: Vec<Line> = Vec::new();
    detail_lines.push(Line::from(Span::styled(
        format!("  {} · {}", focused.code, focused.title),
        Style::default()
            .fg(theme.title)
            .add_modifier(Modifier::BOLD),
    )));
    detail_lines.push(Line::from(Span::styled(
        format!("  object: {}", focused.object),
        Style::default().fg(theme.muted),
    )));
    for chunk in wrap_value(&focused.detail, detail.width.saturating_sub(4) as usize) {
        detail_lines.push(Line::from(Span::styled(
            format!("  {chunk}"),
            Style::default().fg(theme.text),
        )));
    }
    if let Some(s) = &focused.suggestion {
        detail_lines.push(Line::from(Span::styled(
            format!("  suggest: {s}"),
            Style::default().fg(theme.accent),
        )));
    }
    f.render_widget(Paragraph::new(Text::from(detail_lines)), detail);
}
