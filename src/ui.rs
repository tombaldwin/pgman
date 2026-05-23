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
    let chunks = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Length(3), // editor pane (border + 1 line + border)
        Constraint::Min(0),    // results grid
        Constraint::Length(1), // footer
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
    let line = Line::from(vec![
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
    ]);
    f.render_widget(Paragraph::new(line), area);
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
            Mode::Editor => "F5 run · F6 EXPLAIN · F7 EXPLAIN ANALYZE · ctrl-u clear · esc",
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

/// SQL editor pane — always visible, focused in `Mode::Editor`.
fn draw_editor(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let focused = app.mode == Mode::Editor;
    let border_color = if focused {
        theme.border_active
    } else {
        theme.border_idle
    };
    let title = if focused {
        " editor "
    } else {
        " editor (e to focus) "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(title.to_string(), Style::default().fg(theme.title)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let buf = &app.editor_buffer;
    let cursor = app.editor_cursor;
    let mut spans: Vec<Span> = vec![Span::styled("> ", Style::default().fg(theme.muted))];

    if focused {
        let before = buf[..cursor].to_string();
        let (cursor_char, after) = if cursor < buf.len() {
            let mut next = cursor + 1;
            while next < buf.len() && !buf.is_char_boundary(next) {
                next += 1;
            }
            (buf[cursor..next].to_string(), buf[next..].to_string())
        } else {
            (" ".to_string(), String::new())
        };
        spans.push(Span::styled(before, Style::default().fg(theme.text)));
        spans.push(Span::styled(
            cursor_char,
            Style::default().add_modifier(Modifier::REVERSED),
        ));
        spans.push(Span::styled(after, Style::default().fg(theme.text)));
    } else {
        let text = if buf.is_empty() {
            "(empty — press e to focus)".to_string()
        } else {
            buf.clone()
        };
        spans.push(Span::styled(text, Style::default().fg(theme.muted)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), inner);
}

/// Modal for a guarded run. Shows the statement, the safety classification,
/// and asks y/n.
fn draw_confirm(f: &mut Frame, area: Rect, app: &App) {
    let Some(pending) = &app.pending_run else {
        return;
    };
    let theme = &app.theme;
    let kind_label = format!("{:?}", pending.decision.kind);
    let wrap_note = if pending.decision.wrap_in_tx {
        " · will wrap in transaction"
    } else {
        ""
    };
    let lines = vec![
        Line::from(Span::styled(
            "Confirm",
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("{} ({kind_label}){wrap_note}", pending.kind.label()),
            Style::default().fg(theme.accent),
        )),
        Line::from(""),
        Line::from(Span::styled(
            pending.sql.clone(),
            Style::default().fg(theme.text),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "y = run · n / esc = cancel",
            Style::default().fg(theme.muted),
        )),
    ];
    let h = (lines.len() as u16 + 2).min(area.height);
    let w = ((pending.sql.chars().count().max(40) + 4) as u16).min(area.width);
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
        Line::from("    F5  / enter   run the statement (through safety guards)"),
        Line::from("    F6            EXPLAIN  (never executes)"),
        Line::from("    F7            EXPLAIN ANALYZE  (DML wrapped in rollback tx)"),
        Line::from("    ctrl-u        clear the buffer"),
        Line::from("    esc           back to grid"),
        Line::from(""),
        Line::from(Span::styled("  confirm", Style::default().fg(theme.accent))),
        Line::from("    y             run the guarded statement"),
        Line::from("    n / esc       cancel"),
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
