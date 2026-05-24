//! Rendering. `draw` is the single entry point, called once per frame.

use crate::app::{App, ConnState, Mode};
use crate::grid;
use crate::query::complete::Candidate;
use crate::splash;
use crate::theme::Theme;

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, Clear, Padding, Paragraph, Row, Table, Wrap};
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
    // Completion popup sits over the top of the body, anchored just under
    // the editor — only when a cycle is active in Editor mode.
    if app.mode == Mode::Editor && app.completion.is_some() {
        draw_completion_popup(f, chunks[1], chunks[2], app);
    }
    if app.mode == Mode::Help {
        draw_help(f, area, app);
    }
    if app.mode == Mode::Confirm {
        draw_confirm(f, area, app);
    }
    if app.mode == Mode::LogPick {
        draw_log_pick(f, area, app);
    }
    if app.mode == Mode::ConnPick {
        draw_conn_pick(f, area, app);
    }
    if app.mode == Mode::RowDetail {
        draw_row_detail(f, area, app);
    }
    if app.mode == Mode::CellDetail {
        // RowDetail underneath stays drawn so the visual "zoom" reads
        // like a nested overlay rather than a context switch.
        draw_row_detail(f, area, app);
        draw_cell_detail(f, area, app);
    }
    if app.mode == Mode::About {
        draw_about(f, area, app);
    }
    if app.mode == Mode::ExplainTree {
        draw_explain_tree(f, area, app);
    }
    if app.mode == Mode::SchemaBrowser {
        draw_schema_browser(f, area, app);
    }
    if app.mode == Mode::SlowQueries {
        draw_slow_queries(f, area, app);
    }
    if app.mode == Mode::Sessions {
        draw_sessions(f, area, app);
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
    let grid = splash::frame(app.anim_tick);
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

/// "About pgman" overlay — same info as the splash but reachable any time
/// from Normal mode (`A`). Renders the elephant at scale 1 so the popup
/// stays a compact card no matter the terminal size.
fn draw_about(f: &mut Frame, area: Rect, app: &App) {
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
    lines.push(Line::from(
        Span::styled(
            format!("pgman {}", env!("CARGO_PKG_VERSION")),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
    ));
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

    // Card sized to fit the elephant art width plus a comfortable border /
    // padding budget, and tall enough for all the text lines below it.
    let _ = rows_n; // sprite already accounted for inside `lines`
    let width = (art_w + 4).max(48).min(area.width);
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
                    .title(Span::styled(
                        " about ",
                        Style::default().fg(theme.title),
                    )),
            ),
        popup,
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
        ConnState::Failed(err) => draw_connection_failed(f, area, app, err),
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
                draw_grid(f, area, app);
            }
        }
    }
}

/// Render the post-failure body: target DSN, where it came from, the full
/// error chain, and an actionable hint when we recognise the failure mode.
fn draw_connection_failed(f: &mut Frame, area: Rect, app: &App, err: &str) {
    let theme = &app.theme;
    let label = Style::default().fg(theme.muted);
    let value = Style::default().fg(theme.text);
    let red = Style::default().fg(theme.health_red);

    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        "connection failed",
        Style::default()
            .fg(theme.health_red)
            .add_modifier(Modifier::BOLD),
    ))];
    lines.push(Line::from(""));

    if let Some(dsn) = &app.dsn {
        lines.push(Line::from(vec![
            Span::styled("  target  ", label),
            Span::styled(dsn.redacted(), value),
        ]));
    }
    if let Some(origin) = &app.dsn_origin {
        lines.push(Line::from(vec![
            Span::styled("  source  ", label),
            Span::styled(origin.clone(), value),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("  error", label)));
    for chunk in err.split('\n') {
        lines.push(Line::from(vec![
            Span::styled("    ", label),
            Span::styled(chunk.to_string(), red),
        ]));
    }

    if let Some(dsn) = &app.dsn {
        if let Some(hint) = crate::conn::connect_hint(err, dsn) {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("  hint", label)));
            lines.push(Line::from(vec![
                Span::styled("    ", label),
                Span::styled(
                    hint,
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }
    }

    lines.push(Line::from(""));
    let mut actions = String::from("  r retry");
    if app.data_source_picks.len() >= 2 {
        actions.push_str(" · p change connection");
    }
    actions.push_str(" · q quit · logs in ~/.cache/pgman/pgman.log");
    lines.push(Line::from(Span::styled(actions, label)));

    f.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(bordered(theme, "connection")),
        area,
    );
}

fn draw_grid(f: &mut Frame, area: Rect, app: &mut App) {
    let grid = &app.grid;
    let theme = &app.theme;
    let mut widths = grid::column_widths(grid, 48);
    // The sort marker (` ▲` / ` ▼`) is appended to the header cell
    // BEFORE width clamping, so columns hosting the sort key need
    // two extra chars of room. Without this the marker would be
    // truncated off and the operator would think nothing happened
    // when they pressed `s`.
    if let Some((col, _)) = app.grid_sort {
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
            let sort_marker = match app.grid_sort {
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
            if i == app.grid_col_cursor {
                style = style.add_modifier(Modifier::REVERSED);
            }
            Cell::from(text).style(style)
        })
        .collect();
    let header = Row::new(header_cells);

    // Walk only the visible rows (post-filter, post-sort). When no
    // filter has ever been applied, `grid_visible_rows` was
    // initialised to `0..rows.len()` so this branch handles the
    // unfiltered path too.
    let rows: Vec<Row> = app
        .grid_visible_rows
        .iter()
        .filter_map(|&i| grid.rows.get(i))
        .map(|r| {
            Row::new(r.iter().enumerate().map(|(i, c)| {
                let w = widths.get(i).copied().unwrap_or(0);
                Cell::from(grid::truncate_cell(c, w))
            }))
        })
        .collect();
    let constraints: Vec<Constraint> =
        widths.iter().map(|w| Constraint::Length(*w as u16)).collect();
    let visible = app.grid_visible_rows.len();
    let total = grid.row_count();
    let title = if app.grid_filter.is_some() && visible != total {
        format!(" result · {visible}/{total} row(s) (filtered) ")
    } else {
        format!(" result · {total} row(s) ")
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
        // While the connection is failed we override Normal-mode hints with
        // recovery shortcuts so the operator sees them at the bottom of the
        // screen too, not just on the failure card.
        let failed_normal = app.mode == Mode::Normal
            && matches!(app.conn_state, ConnState::Failed(_));
        // While we're still mid-connect, surface that — the Normal hints
        // would suggest j/k/scroll affordances against a grid that
        // doesn't exist yet, and `r retry` wouldn't fire (only Failed
        // accepts r).
        let connecting_normal = app.mode == Mode::Normal
            && matches!(app.conn_state, ConnState::Connecting);
        let hints: &str = if failed_normal {
            if app.data_source_picks.len() >= 2 {
                "r retry · p change connection · q quit · ? help"
            } else {
                "r retry · q quit · ? help"
            }
        } else if connecting_normal {
            "connecting… · q quit"
        } else {
            match app.mode {
                Mode::Help => "esc / ?  close help",
                Mode::Editor if app.completion.is_some() => {
                    // Popup is up — surface the keys that act on it.
                    // Typing narrows live; Tab cycles; Esc restores the
                    // pre-Tab text. Any other key implicitly commits.
                    "type to narrow · tab cycle · esc undo"
                }
                Mode::Editor if app.query_running => {
                    // The only key that does anything useful right
                    // now — surface it on its own so it's impossible
                    // to miss.
                    "ctrl-c cancel running query"
                }
                Mode::Editor => {
                    "F5 run · ctrl-r history · ctrl-e EXPLAIN · ctrl-a ANALYZE · tab complete · ctrl-l log · ctrl-d dbunit · esc"
                }
                Mode::HistorySearch => {
                    "type to search · ctrl-r next-older match · enter accept · esc cancel"
                }
                Mode::LogPick => "↑↓ / j/k navigate · enter load · esc cancel",
                Mode::ConnPick => "↑↓ / j/k navigate · enter connect · q quit",
            Mode::RowDetail => "↑↓ / j/k field · enter zoom · y yank · g/G first/last · esc close",
            Mode::CellDetail => "↑↓ / j/k scroll · y yank · g/G top/bottom · esc / enter back",
            Mode::About => "esc / enter / A close",
                // TxDecision is handled above with a return — this arm is unreachable.
                Mode::TxDecision => "y = commit · n / esc = rollback",
                Mode::Confirm => "y run · n / esc cancel",
                Mode::Normal => "q quit · ? help · e editor · S schema · T slow · L sessions · c change conn · s sort · / filter · Y export",
                Mode::GridFilter => "type to filter live · enter accept · esc clear",
                Mode::ExplainTree => "j/k navigate · enter expand/collapse · g/G top/bottom · q / esc close",
                Mode::SchemaBrowser => "j/k navigate · enter expand schema · g/G top/bottom · q / esc close",
                Mode::SlowQueries => "j/k navigate · enter copy to editor · r refresh · q / esc close",
                Mode::Sessions => "j/k navigate · r refresh · q / esc close",
            }
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
/// Given the current scroll offset, the cursor's line index, the total line
/// count, and the visible-row budget, return the scroll offset that keeps
/// the cursor visible while clamping to the valid range. Pure / testable.
///
/// Rules:
/// - cursor above viewport → scroll up to the cursor line
/// - cursor at-or-below the bottom of the viewport → scroll just enough
///   so the cursor sits on the last visible row
/// - otherwise hold; clamp to `total.saturating_sub(visible)` so we
///   don't reveal blank rows past the buffer's end
/// Walk the highlighter spans that overlap `[line_start, line_end)`,
/// emitting one styled ratatui `Span` per highlight segment. When
/// `cursor_byte_in_line` is `Some`, that single char inside the line
/// renders with the `REVERSED` modifier so the cursor stays visible
/// regardless of which syntax colour the underlying byte sits in.
fn push_highlighted_line<'a>(
    out: &mut Vec<Span<'a>>,
    buf: &'a str,
    spans: &[crate::query::highlight::Span],
    line_start: usize,
    line_end: usize,
    cursor_byte_in_line: Option<usize>,
    theme: &'a Theme,
) {
    use crate::query::highlight::TokenClass;
    let line_text = &buf[line_start..line_end];

    // No highlight spans (unclassified path, e.g. empty cache fallback
    // before we connect). Render plain.
    if spans.is_empty() {
        push_with_cursor(out, line_text, theme.text, cursor_byte_in_line);
        return;
    }

    let color_for = |c: TokenClass| -> ratatui::style::Color {
        match c {
            TokenClass::Keyword => theme.title,
            TokenClass::Function => theme.accent,
            TokenClass::String => theme.syn_string,
            TokenClass::Comment | TokenClass::Number => theme.muted,
            TokenClass::UnknownIdent => theme.syn_unknown,
            TokenClass::KnownIdent
            | TokenClass::Identifier
            | TokenClass::Operator
            | TokenClass::Whitespace => theme.text,
        }
    };

    let mut byte_in_line = 0usize;
    for s in spans {
        // Clip to the line range. Skip spans entirely outside.
        if s.end <= line_start {
            continue;
        }
        if s.start >= line_end {
            break;
        }
        let clip_start = s.start.max(line_start);
        let clip_end = s.end.min(line_end);
        if clip_start >= clip_end {
            continue;
        }
        let in_line_start = clip_start - line_start;
        let in_line_end = clip_end - line_start;
        if in_line_start > byte_in_line {
            // Defensive: a gap shouldn't happen (tokenize covers every
            // byte), but if one does, render it plain.
            push_with_cursor(
                out,
                &line_text[byte_in_line..in_line_start],
                theme.text,
                cursor_byte_in_line.map(|c| c.checked_sub(byte_in_line).unwrap_or(c)),
            );
        }
        let segment = &line_text[in_line_start..in_line_end];
        let color = color_for(s.class);
        // Offset the cursor position into this segment's coords if it
        // falls inside; otherwise pass None.
        let cursor_here = match cursor_byte_in_line {
            Some(c) if c >= in_line_start && c < in_line_end => Some(c - in_line_start),
            _ => None,
        };
        push_with_cursor(out, segment, color, cursor_here);
        byte_in_line = in_line_end;
    }
    // Trailing range with no spans (cursor at EOL on an empty line).
    if byte_in_line < line_text.len() {
        push_with_cursor(
            out,
            &line_text[byte_in_line..],
            theme.text,
            cursor_byte_in_line.and_then(|c| c.checked_sub(byte_in_line)),
        );
    }
    // Empty line with the cursor parked on it — show a single-space
    // REVERSED block so the cursor doesn't vanish.
    if line_text.is_empty() {
        if let Some(0) = cursor_byte_in_line {
            out.push(Span::styled(
                " ".to_string(),
                Style::default().add_modifier(Modifier::REVERSED),
            ));
        }
    }
}

/// Render `segment` in `color`. When `cursor_byte` is `Some(n)` and
/// `n` is inside the segment, split it into [before, cursor-char,
/// after] so the cursor char gets the REVERSED modifier while still
/// inheriting the segment's syntax colour for the before / after
/// portions.
fn push_with_cursor<'a>(
    out: &mut Vec<Span<'a>>,
    segment: &str,
    color: ratatui::style::Color,
    cursor_byte: Option<usize>,
) {
    if segment.is_empty() {
        return;
    }
    let style = Style::default().fg(color);
    match cursor_byte {
        Some(c) if c < segment.len() => {
            // Walk to the next char boundary so we slice cleanly past
            // a multi-byte codepoint.
            let mut next = c + 1;
            while next < segment.len() && !segment.is_char_boundary(next) {
                next += 1;
            }
            out.push(Span::styled(segment[..c].to_string(), style));
            out.push(Span::styled(
                segment[c..next].to_string(),
                style.add_modifier(Modifier::REVERSED),
            ));
            out.push(Span::styled(segment[next..].to_string(), style));
        }
        Some(_at_or_past_end) => {
            // Cursor sits at the very end of this segment — render
            // the segment as normal; the next push (or the empty-line
            // tail) handles the cursor.
            out.push(Span::styled(segment.to_string(), style));
        }
        None => {
            out.push(Span::styled(segment.to_string(), style));
        }
    }
}

pub(crate) fn clamp_editor_scroll(scroll: u16, cur_line: u16, total: u16, visible: u16) -> u16 {
    if visible == 0 {
        return 0;
    }
    let max_scroll = total.saturating_sub(visible);
    let mut s = scroll;
    if cur_line < s {
        s = cur_line;
    } else if visible > 0 && cur_line >= s.saturating_add(visible) {
        s = cur_line.saturating_sub(visible).saturating_add(1);
    }
    s.min(max_scroll)
}

fn draw_editor(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = &app.theme;
    let focused = app.mode == Mode::Editor;
    let border_color = if focused {
        theme.border_active
    } else {
        theme.border_idle
    };
    let total_lines = app.editor_buffer.matches('\n').count() + 1;
    let (cur_line_check, _) =
        crate::app::cursor_position(&app.editor_buffer, app.editor_cursor);
    let title_text = if focused {
        let base = match app.history_pos {
            None => "editor".to_string(),
            Some(i) => format!("editor · history {}/{}", i + 1, app.history.len()),
        };
        // Show line N/M when the buffer is long enough to actually scroll;
        // the visible window is bounded by the pane height (3-12).
        if total_lines > 10 {
            format!("{base} · line {}/{}", cur_line_check + 1, total_lines)
        } else {
            base
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

    // Lex + semantic-classify the buffer once per frame. Cheap
    // (single pass, no allocations beyond the span vec) and we'd
    // otherwise re-derive the same colour for every line render.
    // Unfocused panes get the muted text colour for everything —
    // syntax highlighting is for the active edit surface.
    let highlight_spans = if focused {
        let raw = crate::query::highlight::tokenize(buf);
        // Classify only when we have a schema cache to resolve against;
        // without one, identifiers fall back to the default text
        // colour rather than turning everything red.
        if app.schema_cache.is_empty() {
            raw
        } else {
            let from_before =
                crate::query::from_parse::parse_from_tables_resolved(buf, &app.schema_cache);
            let ctes = crate::query::clause::extract_ctes_resolved(buf, &app.schema_cache);
            crate::query::highlight::classify(
                raw,
                buf,
                &app.schema_cache,
                &from_before,
                &ctes,
            )
        }
    } else {
        Vec::new()
    };

    let mut lines: Vec<Line> = Vec::new();
    let mut line_start_byte: usize = 0;
    for (i, line_text) in buf.split('\n').enumerate() {
        let prompt = if i == 0 { "> " } else { "  " };
        let mut spans: Vec<Span> =
            vec![Span::styled(prompt.to_string(), Style::default().fg(theme.muted))];

        let line_end_byte = line_start_byte + line_text.len();
        if focused {
            // Cursor split — we still need to overlay the REVERSED
            // glyph on top of whatever syntax colour the byte sits in.
            let byte_at_col = if i == cur_line {
                line_text
                    .char_indices()
                    .nth(cur_col)
                    .map(|(b, _)| b)
                    .unwrap_or(line_text.len())
            } else {
                usize::MAX
            };
            push_highlighted_line(
                &mut spans,
                buf,
                &highlight_spans,
                line_start_byte,
                line_end_byte,
                if i == cur_line { Some(byte_at_col) } else { None },
                theme,
            );
        } else {
            spans.push(Span::styled(
                line_text.to_string(),
                Style::default().fg(text_color),
            ));
        }
        lines.push(Line::from(spans));
        // +1 for the newline we split on (except after the last line).
        line_start_byte = line_end_byte + 1;
    }

    // Vertical scroll: keep the cursor's line visible inside the pane's
    // limited height (3-12 rows incl. borders). For short buffers
    // editor_scroll stays 0; long buffers scroll to follow the cursor.
    let total_rendered = lines.len() as u16;
    let scroll = clamp_editor_scroll(
        app.editor_scroll,
        cur_line as u16,
        total_rendered,
        inner.height,
    );
    app.editor_scroll = scroll;
    f.render_widget(
        Paragraph::new(Text::from(lines)).scroll((scroll, 0)),
        inner,
    );
}

/// Completion candidates popup. Anchored just under the editor pane,
/// flush-left over the body area. Shows up to ~10 candidates with the
/// active one highlighted; "↑ N more" / "↓ N more" markers when the
/// list is longer than the popup. Only the active cycle is rendered;
/// any non-Tab editor key dismisses (see `App::editor_key`).
fn draw_completion_popup(
    f: &mut Frame,
    editor_area: Rect,
    body_area: Rect,
    app: &App,
) {
    let Some(cycle) = app.completion.as_ref() else {
        return;
    };
    if cycle.candidates.is_empty() {
        return;
    }
    let theme = &app.theme;
    // The right-side tail is `(kind · context)`. Long context strings
    // (e.g. a long schema or table name) would otherwise overflow the
    // popup width on narrow terminals and get raggedly truncated by
    // ratatui — defeating the disambiguation the context is there for.
    // Cap the rendered context length and ellipsise; preserve the
    // "..." suffix at the end so the kind label and the boundary
    // marker are always visible.
    const CONTEXT_MAX_CHARS: usize = 24;
    let truncated_context = |ctx: &str| -> String {
        let cc = ctx.chars().count();
        if cc <= CONTEXT_MAX_CHARS {
            ctx.to_string()
        } else {
            let keep = CONTEXT_MAX_CHARS.saturating_sub(1);
            let head: String = ctx.chars().take(keep).collect();
            format!("{head}…")
        }
    };
    let tail_of = |c: &Candidate| -> String {
        match &c.context {
            Some(ctx) => format!(" ({} · {})", c.kind.label(), truncated_context(ctx)),
            None => format!(" ({})", c.kind.label()),
        }
    };
    let tail_width = cycle
        .candidates
        .iter()
        .map(|c| tail_of(c).chars().count())
        .max()
        .unwrap_or(0);
    let label_width = cycle
        .candidates
        .iter()
        .map(|c| c.display.chars().count())
        .max()
        .unwrap_or(0);
    let inner_width = (label_width + tail_width + 4) as u16;
    let width = inner_width.min(body_area.width).max(20);

    // Show at most VISIBLE rows; auto-scroll keeps the active row in view.
    const VISIBLE: usize = 8;
    let total = cycle.candidates.len();
    let visible = total.min(VISIBLE);
    let height = (visible as u16 + 2).min(body_area.height); // +2 = borders
    if height < 3 {
        return;
    }
    // Scroll to keep the selected row centred. When nothing's selected
    // (we expanded a common prefix or just showed the list), show the
    // top of the candidate list.
    let focus_idx = cycle.selected.unwrap_or(0);
    let scroll = if total <= VISIBLE {
        0
    } else if focus_idx >= total - VISIBLE / 2 {
        total - VISIBLE
    } else if focus_idx < VISIBLE / 2 {
        0
    } else {
        focus_idx - VISIBLE / 2
    };

    // Anchor flush under the editor's bottom border, left-aligned.
    let popup = Rect {
        x: body_area.x,
        y: editor_area.y + editor_area.height,
        width,
        height,
    };

    let label_style = Style::default().fg(theme.text);
    let kind_style = Style::default().fg(theme.muted);
    let focus_style = Style::default()
        .fg(theme.text)
        .bg(theme.row_selected_bg)
        .add_modifier(Modifier::BOLD);
    // Style for the prefix portion — bolded so the operator sees at a
    // glance what they've already typed vs. what auto-completion would
    // add. Inherits the row's bg when the row is focused.
    let prefix_style = Style::default()
        .fg(theme.title)
        .add_modifier(Modifier::BOLD);
    let prefix_focus_style = focus_style.fg(theme.title);

    // The bolded "head" portion must visually match what's already in
    // the buffer — i.e. the operator's typed/expanded case — so slice
    // directly from `editor_buffer[cycle.start..cycle.end)` rather than
    // taking the first N chars of the candidate's `display` (which is
    // always in the cache's case). Avoids the surprise where the
    // operator typed `T_` and Tab-expanded to `T_USER_`, but the popup
    // bolds `t_user_` (lowercase) on each row.
    //
    // The cycle's [start..end) is always the live prefix (LCP-expanded,
    // single-match-inserted, or narrowed-via-typing — refresh_completion
    // / editor_complete maintain this invariant). When the slice isn't
    // on a char boundary (shouldn't happen in practice, but defensive
    // here), .get returns None and we fall back to no highlighting.
    let typed_head: String = app
        .editor_buffer
        .get(cycle.start..cycle.end)
        .unwrap_or("")
        .to_string();
    let typed_head_char_count = typed_head.chars().count();

    let inner_w = popup.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::with_capacity(visible);
    for (i, cand) in cycle
        .candidates
        .iter()
        .enumerate()
        .skip(scroll)
        .take(VISIBLE)
    {
        // Only highlight a row when the operator has actually picked
        // one (after the second Tab). Before that, render all rows
        // neutrally — the popup is informational.
        let is_focus = cycle.selected == Some(i);
        let marker = if is_focus { "▶ " } else { "  " };
        let display = &cand.display;
        let display_chars: Vec<char> = display.chars().collect();
        // Skip the first `typed_head_char_count` chars of display when
        // composing the tail — those are the prefix we'll render from
        // the buffer slice instead. Clamp to the candidate length to
        // avoid skipping past the end (rare: short candidate, long
        // prefix — possible in test fixtures).
        let skip_n = typed_head_char_count.min(display_chars.len());
        let head = typed_head.clone();
        let tail: String = display_chars[skip_n..].iter().collect();
        let row_display_chars = typed_head_char_count + (display_chars.len() - skip_n);
        let kind_text = tail_of(cand);
        let kind_w = kind_text.chars().count();
        let body_w = marker.chars().count() + row_display_chars;
        let pad_after = inner_w.saturating_sub(body_w).saturating_sub(kind_w);
        let pad = " ".repeat(pad_after);
        let (l_style, k_style, p_style) = if is_focus {
            (focus_style, focus_style, prefix_focus_style)
        } else {
            (label_style, kind_style, prefix_style)
        };
        lines.push(Line::from(vec![
            Span::styled(marker.to_string(), l_style),
            Span::styled(head, p_style),
            Span::styled(tail, l_style),
            Span::styled(pad, l_style),
            Span::styled(kind_text, k_style),
        ]));
    }

    let title = match cycle.selected {
        Some(i) => format!(" {}/{} ", i + 1, total),
        None => format!(" {} matches · Tab to pick ", total),
    };
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border_active))
                .title(Span::styled(title, Style::default().fg(theme.title))),
        ),
        popup,
    );
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

/// Startup picker shown when multiple data sources were discovered (e.g. an
/// IntelliJ project with several `.idea/dataSources.xml` entries). One row
/// per candidate; Enter starts the connection.
/// Wrap `s` to `width` columns, splitting on existing newlines first and
/// then chunking by character. Returns an empty-string vector for an empty
/// input so the caller still emits a visible row. Pure / testable.
pub(crate) fn wrap_value(s: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let lines: Vec<&str> = s.split('\n').collect();
    for raw in lines {
        // Strip the trailing `\r` from CRLF inputs — otherwise the bare
        // CR character makes crossterm jump to column 0 on render,
        // overwriting the label and producing a corrupted popup.
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let chars: Vec<char> = raw.chars().collect();
        if chars.is_empty() {
            out.push(String::new());
            continue;
        }
        for chunk in chars.chunks(width) {
            out.push(chunk.iter().collect());
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// One column's pre-wrapped contribution to the row-detail layout. Pure
/// data: the label (already truncated + padded to `label_width`), the
/// list of value lines, and a flag for "this value was empty" so the
/// renderer can dim it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldLayout {
    pub label: String,
    pub values: Vec<String>,
    pub is_empty: bool,
}

/// Pure layout: build one `FieldLayout` per column. Label is truncated to
/// `label_width` chars and left-padded; value is wrapped via `wrap_value`.
/// Extracted from the renderer so the field-cursor maths can be unit-tested.
pub(crate) fn build_field_layout(
    columns: &[String],
    row: &[String],
    label_width: usize,
    value_width: usize,
) -> Vec<FieldLayout> {
    columns
        .iter()
        .enumerate()
        .map(|(i, col)| {
            let truncated: String = col.chars().take(label_width).collect();
            let label = format!("{:<width$}", truncated, width = label_width);
            let raw = row.get(i).map(|s| s.as_str()).unwrap_or("");
            let is_empty = raw.is_empty();
            let values = if is_empty {
                vec!["(empty)".to_string()]
            } else {
                wrap_value(raw, value_width)
            };
            FieldLayout {
                label,
                values,
                is_empty,
            }
        })
        .collect()
}

/// Given the per-field line counts, where each field starts in the line
/// stream and the desired scroll offset that keeps `focus_field` fully in
/// view inside a `body_height`-row viewport. Pure / testable.
pub(crate) fn auto_scroll_to_field(
    field_line_counts: &[u16],
    focus_field: usize,
    current_scroll: u16,
    body_height: u16,
    max_scroll: u16,
) -> u16 {
    if field_line_counts.is_empty() || body_height == 0 {
        return current_scroll.min(max_scroll);
    }
    let focus_field = focus_field.min(field_line_counts.len() - 1);
    let field_top: u16 = field_line_counts[..focus_field].iter().sum();
    let field_height = field_line_counts[focus_field];
    let field_end = field_top.saturating_add(field_height);
    let mut scroll = current_scroll;
    // Scroll up so the field's first line is visible.
    if field_top < scroll {
        scroll = field_top;
    }
    // Scroll down so the field's last line is visible. When the field is
    // taller than the viewport, prefer the top — partial view is still
    // navigable with the `y` yank.
    let visible_end = scroll.saturating_add(body_height);
    if field_end > visible_end {
        let needed = field_end.saturating_sub(body_height);
        scroll = scroll.max(needed);
    }
    scroll.min(max_scroll)
}

/// Expanded view of the selected row — one labelled value per column, with
/// long values wrapped to fit the popup width. Inspired by psql's `\x`
/// expanded-display mode. j/k moves a field cursor; `y` yanks the focused
/// value to the system clipboard.
fn draw_row_detail(f: &mut Frame, area: Rect, app: &mut App) {
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
    app.row_detail_field_count = layout.len();
    let focus = app.row_detail_field.min(layout.len().saturating_sub(1));
    app.row_detail_field = focus;

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
        let value_span_style_base = if field.is_empty { null_style } else { value_style };
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
    app.row_detail_max_scroll = max_scroll;
    // Auto-scroll so the focused field is visible, then clamp.
    let effective_scroll = auto_scroll_to_field(
        &field_line_counts,
        focus,
        app.row_detail_scroll,
        inner_height,
        max_scroll,
    );
    app.row_detail_scroll = effective_scroll;

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
fn draw_cell_detail(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = &app.theme;
    let Some(idx) = app.selected_grid_row_idx() else {
        return;
    };
    let Some(row) = app.grid.rows.get(idx).cloned() else {
        return;
    };
    let field = app.row_detail_field;
    let column = app
        .grid
        .columns
        .get(field)
        .cloned()
        .unwrap_or_default();
    let value = row.get(field).cloned().unwrap_or_default();

    // Nest inside the row-detail popup so the zoom reads as drilling in,
    // not a new context. 90% of the screen so big JSON gets room.
    let popup = centered_pct(area, 90, 90);
    let inner_width = popup.width.saturating_sub(4) as usize; // borders + uniform(1) pad
    let inner_height = popup.height.saturating_sub(4);
    let is_empty = value.is_empty();
    let body_lines: Vec<String> = if is_empty {
        vec!["(empty)".to_string()]
    } else {
        wrap_value(&value, inner_width)
    };

    let total_lines = body_lines.len() as u16;
    let max_scroll = total_lines.saturating_sub(inner_height);
    app.cell_detail_max_scroll = max_scroll;
    let effective_scroll = app.cell_detail_scroll.min(max_scroll);

    let value_style = if is_empty {
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::ITALIC)
    } else {
        Style::default().fg(theme.text)
    };
    let lines: Vec<Line> = body_lines
        .iter()
        .map(|l| Line::from(Span::styled(l.clone(), value_style)))
        .collect();

    let title = format!(
        " {} · row {} of {} · field {}/{} ",
        column,
        idx + 1,
        app.grid.row_count(),
        field + 1,
        app.row_detail_field_count.max(1)
    );
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

fn draw_conn_pick(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    // Find the widest origin tag so the DSN column lines up.
    let origin_width = app
        .data_source_picks
        .iter()
        .map(|p| p.origin.len())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = app
        .data_source_picks
        .iter()
        .enumerate()
        .map(|(i, pick)| {
            let prefix = if i == app.data_source_pick_index { "▶ " } else { "  " };
            let style = if i == app.data_source_pick_index {
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
        app.data_source_pick_index + 1,
        app.data_source_picks.len()
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

fn draw_help(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = &app.theme;
    let lines = vec![
        Line::from(Span::styled(
            "pgman — keys",
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled("  grid", Style::default().fg(theme.accent))),
        Line::from("    q             quit (esc is a no-op here so a reflex press doesn't lose your session)"),
        Line::from("    ?             toggle this help"),
        Line::from("    e / i / tab   focus editor"),
        Line::from("    c             change connection (opens the picker mid-session)"),
        Line::from("    S             schema browser (psql `\\d` equivalent)"),
        Line::from("    T             slow queries (pg_stat_statements top-N)"),
        Line::from("    L             active sessions + locks (pg_stat_activity)"),
        Line::from("    h / l  ← →    move column cursor"),
        Line::from("    s             cycle sort on focused column (off → ASC → DESC)"),
        Line::from("    /             live row filter (n/N step through matches)"),
        Line::from("    Y             copy the (filtered) grid to clipboard as CSV"),
        Line::from("    j / k  ↑ ↓    move selection"),
        Line::from("    g / G         first / last row"),
        Line::from("    enter         expand selected row (psql \\x style)"),
        Line::from("    A             about pgman (version, credits)"),
        Line::from(""),
        Line::from(Span::styled("  editor", Style::default().fg(theme.accent))),
        Line::from("    F5 / ctrl-↵   run the statement (through safety guards)"),
        Line::from("    ctrl-c         cancel the running query (while in-flight)"),
        Line::from("    ctrl-e / F6   EXPLAIN  (never executes; tree-viewer opens)"),
        Line::from("    ctrl-a / F7   EXPLAIN ANALYZE  (DML wrapped in rollback tx)"),
        Line::from("    ctrl-r         reverse-incremental history search"),
        Line::from("    ctrl-w         \\watch — re-run every 2s, any key stops"),
        Line::from("    ctrl-x         open the buffer in $EDITOR (\\e)"),
        Line::from("    ctrl-f         pg_format the buffer (requires pgformatter)"),
        Line::from("    ctrl-l / F8   parse buffer as log → pick a reconstructed query"),
        Line::from("    ctrl-d / F9   read buffer as DBUnit fixture path → load apply script"),
        Line::from("    tab / ctrl-spc identifier completion (cycles on repeat tab)"),
        Line::from("    .             auto-trigger qualified completion (users.|)"),
        Line::from("    (in popup) type to narrow live · esc to restore typed prefix"),
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
        Line::from(""),
        Line::from(Span::styled("  row detail", Style::default().fg(theme.accent))),
        Line::from("    j / k  ↑ ↓    move to next / previous field"),
        Line::from("    g / G         first / last field"),
        Line::from("    PageUp/Down   jump 10 fields"),
        Line::from("    enter         zoom into focused field (cell detail)"),
        Line::from("    y             yank focused field value to clipboard"),
        Line::from("    esc / q       close"),
        Line::from(""),
        Line::from(Span::styled("  cell detail (zoomed value)", Style::default().fg(theme.accent))),
        Line::from("    j / k  ↑ ↓    scroll"),
        Line::from("    g / G         top / bottom"),
        Line::from("    PageUp/Down   scroll by 10"),
        Line::from("    y             yank value to clipboard"),
        Line::from("    esc / enter   back to row detail"),
        Line::from(""),
        Line::from(Span::styled("  schema browser", Style::default().fg(theme.accent))),
        Line::from("    j / k  ↑ ↓    navigate schemas / tables"),
        Line::from("    enter         expand / collapse focused schema"),
        Line::from("    g / G         jump to top / bottom"),
        Line::from("    esc / q       close"),
        Line::from(""),
        Line::from(Span::styled("  EXPLAIN tree", Style::default().fg(theme.accent))),
        Line::from("    j / k  ↑ ↓    navigate plan nodes"),
        Line::from("    enter         expand / collapse focused subtree"),
        Line::from("    g / G         jump to root / last visible node"),
        Line::from("    esc / q       close"),
        Line::from(""),
        Line::from(Span::styled("  help", Style::default().fg(theme.accent))),
        Line::from("    j / k  ↑ ↓    scroll"),
        Line::from("    g / G         jump to top / bottom"),
        Line::from("    esc / ? / q   close"),
    ];
    let popup = centered_pct(area, 70, 70);
    f.render_widget(Clear, popup);
    // Body height = popup height minus borders (top + bottom) minus padding
    // (uniform(1) — top + bottom). That's the visible row budget for clamping
    // the scroll offset.
    let total_lines = lines.len() as u16;
    let inner_height = popup.height.saturating_sub(4);
    let max_scroll = total_lines.saturating_sub(inner_height);
    app.help_max_scroll = max_scroll;
    let effective_scroll = app.help_scroll.min(max_scroll);

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
fn draw_explain_tree(f: &mut Frame, area: Rect, app: &App) {
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
    let mut scroll = 0usize;
    if app.explain_cursor >= visible_h {
        scroll = app.explain_cursor + 1 - visible_h;
    }

    let mut lines: Vec<Line> = Vec::with_capacity(rows.len().saturating_sub(scroll).min(visible_h));
    for (i, row) in rows.iter().enumerate().skip(scroll).take(visible_h) {
        let is_focus = i == app.explain_cursor;
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
                    header.push_str(" ");
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

/// Schema browser modal. Two panes inside a centered overlay:
/// the left holds the schema → table tree, the right holds the
/// columns / constraints for the focused table (or a one-line
/// summary for a focused schema). Static — driven entirely by the
/// schema cache; no live queries.
fn draw_schema_browser(f: &mut Frame, area: Rect, app: &App) {
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
    let scroll = if app.schema_browser_cursor >= visible_h {
        app.schema_browser_cursor + 1 - visible_h
    } else {
        0
    };
    let mut lines: Vec<Line> = Vec::new();
    for (i, row) in rows.iter().enumerate().skip(scroll).take(visible_h) {
        let is_focus = i == app.schema_browser_cursor;
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
            SchemaBrowserRow::Table { name, .. } => format!("    {name}"),
        };
        lines.push(Line::from(Span::styled(text, style)));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), left);

    // Right: details for the focused row.
    let mut right_lines: Vec<Line> = Vec::new();
    match rows.get(app.schema_browser_cursor) {
        Some(SchemaBrowserRow::Schema {
            name, table_count, ..
        }) => {
            right_lines.push(Line::from(Span::styled(
                format!("schema: {name}"),
                Style::default().fg(theme.title).add_modifier(Modifier::BOLD),
            )));
            right_lines.push(Line::from(""));
            right_lines.push(Line::from(format!("{table_count} table(s)")));
            right_lines.push(Line::from(""));
            right_lines.push(Line::from(Span::styled(
                "enter to expand — then arrow / j/k into the tables",
                Style::default().fg(theme.muted),
            )));
        }
        Some(SchemaBrowserRow::Table { schema, name }) => {
            right_lines.push(Line::from(Span::styled(
                format!("{schema}.{name}"),
                Style::default().fg(theme.title).add_modifier(Modifier::BOLD),
            )));
            right_lines.push(Line::from(""));
            // Columns from the cache (ordered by attnum).
            let cols = app
                .schema_cache
                .columns_by_table
                .get(&(schema.clone(), name.clone()))
                .cloned()
                .unwrap_or_default();
            right_lines.push(Line::from(Span::styled(
                format!("columns ({})", cols.len()),
                Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
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
                    c.schema.eq_ignore_ascii_case(schema)
                        && c.table.eq_ignore_ascii_case(name)
                })
                .collect();
            if !cons.is_empty() {
                right_lines.push(Line::from(""));
                right_lines.push(Line::from(Span::styled(
                    format!("constraints ({})", cons.len()),
                    Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
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

/// Slow-query top-N panel. Top section: one-line summary per
/// stored statement, sorted by total exec time desc. Bottom
/// section: full SQL for the focused row + key shortcuts.
fn draw_slow_queries(f: &mut Frame, area: Rect, app: &App) {
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

    if app.slow_queries.is_empty() {
        // Either still loading, or genuinely no rows. last_status
        // carries the right phrasing either way; render it inside
        // the popup so the operator doesn't have to look down at
        // the footer.
        let msg = app
            .last_status
            .clone()
            .unwrap_or_else(|| "no rows".into());
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
    let scroll = if app.slow_queries_cursor >= visible_h {
        app.slow_queries_cursor + 1 - visible_h
    } else {
        0
    };
    let mut lines: Vec<Line> = Vec::new();
    // Header row.
    lines.push(Line::from(Span::styled(
        format!(
            "  {:>10}  {:>9}  {:>8}  {:>8}  {}",
            "total ms", "mean ms", "calls", "rows", "query"
        ),
        Style::default().fg(theme.muted).add_modifier(Modifier::BOLD),
    )));
    for (i, row) in app.slow_queries.iter().enumerate().skip(scroll).take(visible_h.saturating_sub(1)) {
        let is_focus = i == app.slow_queries_cursor;
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

    // Bottom: full SQL for the focused row.
    let focused_sql = app
        .slow_queries
        .get(app.slow_queries_cursor)
        .map(|r| r.query.clone())
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(focused_sql)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(theme.text))
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(theme.border_idle)),
            ),
        detail_area,
    );
}

/// Active-sessions + locks panel. Rows ordered by blocked-first;
/// the renderer flags blockers in the same colour as the hottest-
/// node highlight in the EXPLAIN tree (red) so visual scanning
/// matches between the two.
fn draw_sessions(f: &mut Frame, area: Rect, app: &App) {
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

    if app.sessions.is_empty() {
        let msg = app
            .last_status
            .clone()
            .unwrap_or_else(|| "no rows".into());
        f.render_widget(
            Paragraph::new(Text::from(msg)).style(Style::default().fg(theme.muted)),
            inner,
        );
        return;
    }

    let visible_h = inner.height as usize;
    let scroll = if app.sessions_cursor >= visible_h {
        app.sessions_cursor + 1 - visible_h
    } else {
        0
    };
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(
            "  {:>6}  {:>20}  {:>10}  {:>8}  {:>8}  {}",
            "pid", "user/app", "state", "age(s)", "blocked", "query"
        ),
        Style::default().fg(theme.muted).add_modifier(Modifier::BOLD),
    )));
    for (i, row) in app.sessions.iter().enumerate().skip(scroll).take(visible_h.saturating_sub(1)) {
        let is_focus = i == app.sessions_cursor;
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
        let blocked_disp = if is_blocked { row.blocked_by.as_str() } else { "-" };
        let line = format!(
            "  {:>6}  {:>20}  {:>10}  {:>8.1}  {:>8}  {}",
            row.pid, user_app, row.state, row.age_secs, blocked_disp, one_line
        );
        lines.push(Line::from(Span::styled(line, style)));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// A centred `w`%×`h`% rectangle within `area`.
fn centered_pct(area: Rect, w: u16, h: u16) -> Rect {
    let width = area.width * w / 100;
    let height = area.height * h / 100;
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_value_splits_on_existing_newlines() {
        let got = wrap_value("a\nb\nc", 80);
        assert_eq!(got, vec!["a", "b", "c"]);
    }

    #[test]
    fn wrap_value_chunks_long_lines() {
        let got = wrap_value("abcdefghij", 4);
        assert_eq!(got, vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn wrap_value_handles_empty_string() {
        // Empty value should still emit one line so the row stays visible.
        assert_eq!(wrap_value("", 80), vec![""]);
    }

    #[test]
    fn wrap_value_preserves_blank_lines() {
        let got = wrap_value("a\n\nb", 80);
        assert_eq!(got, vec!["a", "", "b"]);
    }

    #[test]
    fn wrap_value_width_zero_returns_input_unchanged() {
        // Defensive: a popup so narrow there's no room for the value column.
        // Falling back to one line keeps render functions from panicking on
        // `chunks(0)`.
        assert_eq!(wrap_value("hello", 0), vec!["hello"]);
    }

    #[test]
    fn clamp_editor_scroll_holds_when_cursor_inside_viewport() {
        // 20-line buffer, 5-row viewport, cursor on line 7, current scroll 3.
        // Viewport shows lines 3..8 — cursor visible — no change.
        assert_eq!(clamp_editor_scroll(3, 7, 20, 5), 3);
    }

    #[test]
    fn clamp_editor_scroll_pulls_down_when_cursor_above() {
        // Cursor moved up to line 1; viewport was at line 5.
        // Scroll up to make cursor the new top row.
        assert_eq!(clamp_editor_scroll(5, 1, 20, 5), 1);
    }

    #[test]
    fn clamp_editor_scroll_pushes_up_when_cursor_below_viewport() {
        // Viewport 0..5, cursor moved to line 12 — bring it to last
        // visible row (so cursor sits at row 4 of the pane).
        assert_eq!(clamp_editor_scroll(0, 12, 20, 5), 12 - 5 + 1);
    }

    #[test]
    fn clamp_editor_scroll_caps_at_total_minus_visible() {
        // Don't reveal blank rows past the buffer's end.
        assert_eq!(clamp_editor_scroll(50, 19, 20, 5), 15);
    }

    #[test]
    fn clamp_editor_scroll_returns_zero_when_buffer_fits() {
        // Buffer fits in one pane — no scrolling possible.
        assert_eq!(clamp_editor_scroll(3, 4, 5, 10), 0);
    }

    #[test]
    fn clamp_editor_scroll_handles_zero_visible() {
        // Defensive: degenerate pane height.
        assert_eq!(clamp_editor_scroll(7, 5, 20, 0), 0);
    }

    #[test]
    fn wrap_value_strips_carriage_returns_in_crlf_input() {
        // CRLF must not leak raw `\r` into the output — crossterm would
        // jump the cursor to column 0 and corrupt the row.
        let got = wrap_value("a\r\nb\r\nc", 80);
        assert_eq!(got, vec!["a", "b", "c"]);
    }

    #[test]
    fn wrap_value_handles_multibyte_chars_by_chars_not_bytes() {
        // "café" is 4 chars / 5 bytes — chunk by chars so a slice doesn't
        // land mid-codepoint.
        let got = wrap_value("café", 2);
        assert_eq!(got, vec!["ca", "fé"]);
    }

    #[test]
    fn build_field_layout_pads_label_and_marks_empty() {
        let cols = vec!["id".to_string(), "long_column_name".to_string()];
        let row = vec!["42".to_string(), "".to_string()];
        let got = build_field_layout(&cols, &row, 16, 40);
        assert_eq!(got.len(), 2);
        // Labels padded to label_width.
        assert_eq!(got[0].label.chars().count(), 16);
        assert!(got[0].label.starts_with("id"));
        assert_eq!(got[0].is_empty, false);
        assert_eq!(got[0].values, vec!["42"]);
        // Empty cell rendered with "(empty)" sentinel.
        assert_eq!(got[1].is_empty, true);
        assert_eq!(got[1].values, vec!["(empty)"]);
    }

    #[test]
    fn build_field_layout_truncates_oversized_labels() {
        let cols = vec!["aaaaaaaaaaaaaaaaaaaaaaaaa".to_string()]; // 25 chars
        let row = vec!["v".to_string()];
        let got = build_field_layout(&cols, &row, 8, 10);
        assert_eq!(got[0].label.chars().count(), 8);
        assert!(got[0].label.starts_with("aaaaaaaa"));
    }

    #[test]
    fn build_field_layout_wraps_long_values() {
        let cols = vec!["bio".to_string()];
        let row = vec!["abcdefghij".to_string()];
        let got = build_field_layout(&cols, &row, 4, 4);
        assert_eq!(got[0].values, vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn auto_scroll_keeps_focused_field_in_view_when_moving_down() {
        // Three fields, 2 lines each = 6 total lines. Viewport = 3 rows.
        // max_scroll = 3. Focus field 2 (lines 4..6) — need scroll = 3.
        let counts = vec![2u16, 2, 2];
        let scroll = auto_scroll_to_field(&counts, 2, 0, 3, 3);
        assert_eq!(scroll, 3);
    }

    #[test]
    fn auto_scroll_scrolls_up_when_focus_moves_above_viewport() {
        // Same shape; user had scrolled down to 3 (last field visible),
        // then moves focus back to field 0. Scroll should snap to 0.
        let counts = vec![2u16, 2, 2];
        let scroll = auto_scroll_to_field(&counts, 0, 3, 3, 3);
        assert_eq!(scroll, 0);
    }

    #[test]
    fn auto_scroll_clamps_to_max_scroll() {
        let counts = vec![2u16, 2];
        let scroll = auto_scroll_to_field(&counts, 1, 999, 4, 0);
        // max_scroll = 0 — everything fits — auto-scroll must clamp.
        assert_eq!(scroll, 0);
    }

    #[test]
    fn auto_scroll_handles_empty_field_list() {
        let scroll = auto_scroll_to_field(&[], 0, 5, 4, 10);
        assert_eq!(scroll, 5);
    }

    #[test]
    fn auto_scroll_handles_zero_height_viewport() {
        let counts = vec![1u16];
        let scroll = auto_scroll_to_field(&counts, 0, 2, 0, 5);
        // body_height=0 → no-op except for the max_scroll clamp.
        assert_eq!(scroll, 2);
    }
}
