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
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(area);
    draw_header(f, chunks[0], app);
    draw_body(f, chunks[1], app);
    draw_footer(f, chunks[2], app);
    if app.mode == Mode::Help {
        draw_help(f, area, &app.theme);
    }
}

/// Theme colour for a sprite pixel — `None` for empty (transparent).
fn pixel_color(px: splash::Pixel, theme: &Theme) -> Option<Color> {
    match px {
        splash::Pixel::Empty => None,
        splash::Pixel::Body => Some(theme.title),  // a blue elephant — Postgres
        splash::Pixel::Eye => Some(theme.accent),  // bright amber eye
        splash::Pixel::Tusk => Some(theme.text),   // near-white ivory
    }
}

fn draw_splash(f: &mut Frame, app: &App) {
    let theme = &app.theme;
    // The pixel sprite is a fixed-shape block: render it left-aligned inside a
    // centred rect so it keeps its shape. Each sprite row is authored centred
    // within its grid, so left-aligning the block keeps the elephant centred
    // while the trunk's curl stays intentionally off-centre.
    let grid = splash::frame(app.splash_tick);
    let lines: Vec<Line> = grid
        .iter()
        .map(|row| {
            Line::from(
                row.iter()
                    .map(|&px| match pixel_color(px, theme) {
                        Some(c) => Span::styled("██", Style::default().fg(c)),
                        None => Span::raw("  "),
                    })
                    .collect::<Vec<Span>>(),
            )
        })
        .collect();
    let art_h = lines.len() as u16;
    let art_w = grid.iter().map(Vec::len).max().unwrap_or(0) as u16 * 2;

    let width = art_w.max(13);
    let block = centered(f.area(), width, art_h + 3);
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
            let sp = SPINNER[app.splash_tick % SPINNER.len()];
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
    let hints = match app.mode {
        Mode::Help => "esc / ?  close help",
        Mode::Normal => "q quit · ? help · j/k scroll · g/G top/bottom",
    };
    f.render_widget(
        Paragraph::new(Span::styled(
            format!(" {hints}"),
            Style::default().fg(app.theme.muted),
        )),
        area,
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
        Line::from("  q / esc      quit"),
        Line::from("  ?            toggle this help"),
        Line::from("  j / k  ↑ ↓   move selection"),
        Line::from("  g / G        first / last row"),
        Line::from(""),
        Line::from(Span::styled(
            "  M0 scaffold — the query editor arrives in M2",
            Style::default().fg(theme.muted),
        )),
    ];
    let popup = centered(area, 46, lines.len() as u16 + 2);
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
