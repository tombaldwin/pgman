//! Rendering. `draw` is the single entry point, called once per frame.

use crate::app::{App, ConnState, Mode};
use crate::grid::{self, Grid};
use crate::splash;
use crate::theme::Theme;

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState};
use ratatui::Frame;

const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub fn draw(f: &mut Frame, app: &mut App) {
    if app.splash_visible {
        draw_splash(f, app);
        return;
    }
    let area = f.area();
    // Dynamic editor height: grow with the buffer up to a cap, with a min so
    // the focused empty editor still has a visible content line.
    let editor_lines = app.editor_buffer.matches('\n').count() + 1;
    let editor_height: u16 = (editor_lines as u16 + 2).clamp(3, 12);
    let chunks = Layout::vertical([
        Constraint::Length(1),             // header
        Constraint::Length(editor_height), // editor pane (border + lines + border)
        Constraint::Min(0),                // results grid
        Constraint::Length(1),             // footer
    ])
    .split(area);
    draw_header(f, chunks[0], app);
    draw_editor(f, chunks[1], app);
    draw_body(f, chunks[2], app);
    draw_footer(f, chunks[3], app);
    if app.mode == Mode::Help {
        draw_help(f, area, &app.theme);
    }
    if app.mode == Mode::Confirm {
        draw_confirm(f, area, app);
    }
    if app.mode == Mode::LogPick {
        draw_log_pick(f, area, app);
    }
}

/// Theme colour for a sprite pixel — `None` for empty (transparent).
fn pixel_color(px: splash::Pixel, theme: &Theme) -> Option<Color> {
    use splash::Pixel;
    match px {
        Pixel::Empty => None,
        Pixel::Outline => Some(theme.elephant_outline),
        Pixel::Body => Some(theme.elephant_body),
        Pixel::EarShade => Some(theme.elephant_shade),
        Pixel::Eye => Some(theme.elephant_eye),
        Pixel::Pupil => Some(theme.elephant_pupil),
        Pixel::Cheek => Some(theme.elephant_cheek),
        Pixel::Tusk => Some(theme.elephant_tusk),
    }
}

fn draw_splash(f: &mut Frame, app: &App) {
    let theme = &app.theme;
    // The pixel sprite is a fixed-shape block: render it left-aligned inside a
    // centred rect so it keeps its shape. Each sprite row is authored centred
    // within its grid, so left-aligning the block keeps the elephant centred
    // while the trunk's curl stays intentionally off-centre.
    let grid = splash::frame();
    let rows_n = grid.len() as u16;
    let cols_n = grid.iter().map(Vec::len).max().unwrap_or(0) as u16;
    let area = f.area();

    // Render at the largest integer scale (up to 3x) that fits the terminal,
    // leaving room for the labels below — a bigger terminal gets a bigger
    // elephant.
    let mut scale: usize = 1;
    for s in [3u16, 2] {
        if rows_n * s + 4 <= area.height && cols_n * 2 * s <= area.width {
            scale = s as usize;
            break;
        }
    }

    let pixel = "██".repeat(scale);
    let gap = " ".repeat(scale * 2);
    let mut lines: Vec<Line> = Vec::with_capacity(grid.len() * scale);
    for row in &grid {
        let spans: Vec<Span> = row
            .iter()
            .map(|&px| match pixel_color(px, theme) {
                Some(c) => Span::styled(pixel.clone(), Style::default().fg(c)),
                None => Span::raw(gap.clone()),
            })
            .collect();
        let line = Line::from(spans);
        for _ in 0..scale {
            lines.push(line.clone());
        }
    }
    let art_h = lines.len() as u16;
    let art_w = cols_n * 2 * scale as u16;

    let width = art_w.max(13);
    let block = centered(area, width, art_h + 3);
    let rows = Layout::vertical([
        Constraint::Length(art_h),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(block);
    f.render_widget(Paragraph::new(Text::from(lines)), rows[0]);
    f.render_widget(
        Paragraph::new(Span::styled(
            "pgman",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center),
        rows[2],
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            "press any key",
            Style::default().fg(theme.muted),
        ))
        .alignment(Alignment::Center),
        rows[3],
    );
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let db = app
        .dsn
        .as_ref()
        .map(|d| d.dbname.clone())
        .unwrap_or_else(|| "—".to_string());
    let (state, state_style) = match &app.conn_state {
        ConnState::Disconnected => {
            ("disconnected".to_string(), Style::default().fg(theme.muted))
        }
        ConnState::Connecting => {
            let sp = SPINNER[app.anim_tick % SPINNER.len()];
            (
                format!("{sp} connecting"),
                Style::default().fg(theme.health_yellow),
            )
        }
        ConnState::Connected { server_version } => (
            format!("connected · pg {server_version}"),
            Style::default().fg(theme.health_green),
        ),
        ConnState::Failed(_) => ("connection failed".to_string(), Style::default().fg(theme.health_red)),
    };
    let mut spans = vec![
        Span::styled(
            " pgman ",
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("│ ", Style::default().fg(theme.border_idle)),
        Span::styled(db, Style::default().fg(theme.text)),
        Span::raw("  "),
        Span::styled(state, state_style),
    ];
    if app.tx_open || app.mode == Mode::TxDecision {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            " TX ",
            Style::default()
                .bg(theme.health_yellow)
                .fg(theme.row_alt_bg)
                .add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_body(f: &mut Frame, area: Rect, app: &mut App) {
    match &app.conn_state {
        ConnState::Failed(err) => {
            let p = Paragraph::new(format!("connection failed:\n\n{err}"))
                .style(Style::default().fg(app.theme.health_red))
                .block(bordered(&app.theme, "error"));
            f.render_widget(p, area);
        }
        ConnState::Connecting => {
            f.render_widget(
                Paragraph::new("connecting…")
                    .style(Style::default().fg(app.theme.muted))
                    .block(bordered(&app.theme, "pgman")),
                area,
            );
        }
        ConnState::Disconnected => {
            f.render_widget(
                Paragraph::new("no connection — start pgman with --dsn postgres://…")
                    .style(Style::default().fg(app.theme.muted))
                    .block(bordered(&app.theme, "pgman")),
                area,
            );
        }
        ConnState::Connected { .. } => {
            if app.grid.is_empty() {
                f.render_widget(
                    Paragraph::new("(no rows)")
                        .style(Style::default().fg(app.theme.muted))
                        .block(bordered(&app.theme, "result")),
                    area,
                );
            } else {
                draw_grid(f, area, &app.grid, &mut app.grid_state, &app.theme);
            }
        }
    }
}

fn draw_grid(
    f: &mut Frame,
    area: Rect,
    grid: &Grid,
    state: &mut TableState,
    theme: &Theme,
) {
    let widths = grid::column_widths(grid, 48);
    let header = Row::new(grid.columns.iter().map(|c| Cell::from(c.clone()))).style(
        Style::default()
            .fg(theme.title)
            .add_modifier(Modifier::BOLD),
    );
    let rows: Vec<Row> = grid
        .rows
        .iter()
        .map(|r| {
            Row::new(r.iter().enumerate().map(|(i, c)| {
                let w = widths.get(i).copied().unwrap_or(0);
                Cell::from(grid::truncate_cell(c, w))
            }))
        })
        .collect();
    let constraints: Vec<Constraint> =
        widths.iter().map(|w| Constraint::Length(*w as u16)).collect();
    let table = Table::new(rows, constraints)
        .header(header)
        .column_spacing(2)
        .row_highlight_style(Style::default().bg(theme.row_selected_bg))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border_idle))
                .title(Span::styled(
                    format!(" result · {} row(s) ", grid.row_count()),
                    Style::default().fg(theme.title),
                )),
        );
    f.render_stateful_widget(table, area, state);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    // TxDecision is its own prominent prompt — it pre-empts the normal
    // status/error/hint priority because the user must answer.
    if app.mode == Mode::TxDecision {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " TX OPEN ",
                    Style::default()
                        .bg(theme.health_yellow)
                        .fg(theme.row_alt_bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(
                    "y = commit · n / esc = rollback",
                    Style::default().fg(theme.health_yellow),
                ),
            ])),
            area,
        );
        return;
    }
    // Priority: query error > status (e.g. "EXPLAIN ok · 4 rows") > hints.
    let line = if let Some(err) = &app.last_error {
        Line::from(vec![
            Span::styled(" ⚠ ", Style::default().fg(theme.health_red)),
            Span::styled(err.clone(), Style::default().fg(theme.health_red)),
        ])
    } else if let Some(status) = &app.last_status {
        let sp = SPINNER[app.anim_tick % SPINNER.len()];
        let prefix = if app.query_running {
            format!(" {sp} ")
        } else {
            " ".to_string()
        };
        Line::from(Span::styled(
            format!("{prefix}{status}"),
            Style::default().fg(theme.health_green),
        ))
    } else {
        let hints = match app.mode {
            Mode::Help => "esc / ?  close help",
            Mode::Editor => {
                "ctrl-r run · ctrl-e EXPLAIN · ctrl-a ANALYZE · ctrl-l log · ctrl-d dbunit · esc"
            }
            Mode::LogPick => "↑↓ / j/k navigate · enter load · esc cancel",
            // TxDecision is handled above with a return — this arm is unreachable.
            Mode::TxDecision => "y = commit · n / esc = rollback",
            Mode::Confirm => "y run · n / esc cancel",
            Mode::Normal => "q quit · ? help · e editor · j/k scroll · g/G top/bottom",
        };
        Line::from(Span::styled(
            format!(" {hints}"),
            Style::default().fg(theme.muted),
        ))
    };
    f.render_widget(Paragraph::new(line), area);
}

/// SQL editor pane — always visible, focused in `Mode::Editor`. Multi-line
/// buffer; the cursor renders as a reverse-video character on its line.
fn draw_editor(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let focused = app.mode == Mode::Editor;
    let border_color = if focused {
        theme.border_active
    } else {
        theme.border_idle
    };
    let title_text = if focused {
        match app.history_pos {
            None => "editor".to_string(),
            Some(i) => format!("editor · history {}/{}", i + 1, app.history.len()),
        }
    } else {
        "editor (e to focus)".to_string()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            format!(" {title_text} "),
            Style::default().fg(theme.title),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let buf = &app.editor_buffer;
    let (cur_line, cur_col) = crate::app::cursor_position(buf, app.editor_cursor);
    let text_color = if focused { theme.text } else { theme.muted };

    // Unfocused, empty buffer — show a hint instead of an empty pane.
    if !focused && buf.is_empty() {
        let lines = vec![Line::from(vec![
            Span::styled("> ", Style::default().fg(theme.muted)),
            Span::styled(
                "(empty — press e to focus)",
                Style::default().fg(theme.muted),
            ),
        ])];
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
        return;
    }

    let mut lines: Vec<Line> = Vec::new();
    for (i, line_text) in buf.split('\n').enumerate() {
        let prompt = if i == 0 { "> " } else { "  " };
        let mut spans: Vec<Span> =
            vec![Span::styled(prompt.to_string(), Style::default().fg(theme.muted))];

        if focused && i == cur_line {
            // Find byte offset of `cur_col` chars into this line.
            let byte_at_col = line_text
                .char_indices()
                .nth(cur_col)
                .map(|(b, _)| b)
                .unwrap_or(line_text.len());
            let before = line_text[..byte_at_col].to_string();
            let (cursor_char, after) = if byte_at_col < line_text.len() {
                let mut next = byte_at_col + 1;
                while next < line_text.len() && !line_text.is_char_boundary(next) {
                    next += 1;
                }
                (
                    line_text[byte_at_col..next].to_string(),
                    line_text[next..].to_string(),
                )
            } else {
                (" ".to_string(), String::new())
            };
            spans.push(Span::styled(before, Style::default().fg(text_color)));
            spans.push(Span::styled(
                cursor_char,
                Style::default().add_modifier(Modifier::REVERSED),
            ));
            spans.push(Span::styled(after, Style::default().fg(text_color)));
        } else {
            spans.push(Span::styled(
                line_text.to_string(),
                Style::default().fg(text_color),
            ));
        }
        lines.push(Line::from(spans));
    }

    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// Modal for a guarded run. Shows the statement, the safety classification,
/// and asks y/n.
fn draw_confirm(f: &mut Frame, area: Rect, app: &App) {
    let Some(pending) = &app.pending_run else {
        return;
    };
    let theme = &app.theme;
    let detail = match &pending.summary {
        Some(s) => s.clone(),
        None => format!("{:?}", pending.decision.kind),
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
fn draw_log_pick(f: &mut Frame, area: Rect, app: &App) {
    use crate::query::reconstruct::Source;
    let theme = &app.theme;
    let max_preview = 80usize;
    let lines: Vec<Line> = app
        .log_picks
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
            let prefix = if i == app.log_pick_index { "▶ " } else { "  " };
            let style = if i == app.log_pick_index {
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
        .collect();

    let title = format!(
        " log picks · {}/{} ",
        app.log_pick_index + 1,
        app.log_picks.len()
    );
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2)).max(3);
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

fn draw_help(f: &mut Frame, area: Rect, theme: &Theme) {
    let lines = vec![
        Line::from(Span::styled(
            "pgman — keys",
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled("  grid", Style::default().fg(theme.accent))),
        Line::from("    q / esc       quit"),
        Line::from("    ?             toggle this help"),
        Line::from("    e / i / tab   focus editor"),
        Line::from("    j / k  ↑ ↓    move selection"),
        Line::from("    g / G         first / last row"),
        Line::from(""),
        Line::from(Span::styled("  editor", Style::default().fg(theme.accent))),
        Line::from("    ctrl-r / F5   run the statement (through safety guards)"),
        Line::from("    ctrl-e / F6   EXPLAIN  (never executes)"),
        Line::from("    ctrl-a / F7   EXPLAIN ANALYZE  (DML wrapped in rollback tx)"),
        Line::from("    ctrl-l / F8   parse buffer as log → pick a reconstructed query"),
        Line::from("    ctrl-d / F9   read buffer as DBUnit fixture path → load apply script"),
        Line::from("    enter         insert newline"),
        Line::from("    ↑ ↓ ← →       move cursor (col remembered across lines)"),
        Line::from("    home / end    start / end of current line"),
        Line::from("    ctrl-p / -n   prev / next history entry"),
        Line::from("    ctrl-u        clear the buffer"),
        Line::from("    esc           back to grid"),
        Line::from(""),
        Line::from(Span::styled("  confirm", Style::default().fg(theme.accent))),
        Line::from("    y             run the guarded statement"),
        Line::from("    n / esc       cancel"),
        Line::from(""),
        Line::from(Span::styled("  tx open", Style::default().fg(theme.accent))),
        Line::from("    y             commit the transaction"),
        Line::from("    n / esc       roll back"),
        Line::from(""),
        Line::from(Span::styled("  log pick", Style::default().fg(theme.accent))),
        Line::from("    ↑ ↓ / j / k   navigate"),
        Line::from("    enter         load selected query into the editor"),
        Line::from("    esc / q       cancel"),
    ];
    let popup = centered(area, 60, lines.len() as u16 + 2);
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border_active))
                .style(Style::default().fg(theme.text)),
        ),
        popup,
    );
}

/// A `width`×`height` rectangle centred within `area` (clamped to fit).
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// A titled, idle-bordered block.
fn bordered(theme: &Theme, title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_idle))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(theme.title),
        ))
}
