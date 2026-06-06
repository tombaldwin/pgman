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
    // Tab bar: one extra line iff we have more than one tab.
    // Keeps the single-tab default UX byte-identical to the
    // pre-multi-tab layout.
    let tabbar_height: u16 = if app.tabs.len() > 1 { 1 } else { 0 };
    let chunks = Layout::vertical([
        Constraint::Length(1),             // header
        Constraint::Length(tabbar_height), // optional tab bar
        Constraint::Length(editor_height), // editor pane (border + lines + border)
        Constraint::Min(0),                // results grid
        Constraint::Length(1),             // footer
    ])
    .split(area);
    draw_header(f, chunks[0], app);
    if tabbar_height > 0 {
        draw_tab_bar(f, chunks[1], app);
    }
    draw_editor(f, chunks[2], app);
    draw_body(f, chunks[3], app);
    draw_footer(f, chunks[4], app);
    // Completion popup sits over the top of the body, anchored just under
    // the editor — only when a cycle is active in Editor mode.
    if app.mode == Mode::Editor && app.completion.is_some() {
        draw_completion_popup(f, chunks[2], chunks[3], app);
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
    if app.mode == Mode::SchemaBrowser || app.mode == Mode::SchemaBrowserFilter {
        draw_schema_browser(f, area, app);
    }
    if app.mode == Mode::SlowQueries {
        draw_slow_queries(f, area, app);
    }
    if app.mode == Mode::Sessions {
        draw_sessions(f, area, app);
    }
    if app.mode == Mode::SchemaLint {
        draw_schema_lint(f, area, app);
    }
    if app.mode == Mode::ErrorDetail {
        draw_error_detail(f, area, app);
    }
    if app.mode == Mode::Notifications {
        draw_notifications(f, area, app);
    }
    if app.mode == Mode::TapMonitor {
        draw_tap_monitor(f, area, app);
    }
    if app.mode == Mode::SavedQueries || app.mode == Mode::SavedQueriesFilter {
        draw_saved_queries(f, area, app);
    }
    if app.mode == Mode::RenameQueryPrompt {
        // Panel underneath stays drawn; the rename box floats over it.
        draw_saved_queries(f, area, app);
        draw_rename_prompt(f, area, app);
    }
    if app.mode == Mode::SaveQueryPrompt {
        draw_save_query_prompt(f, area, app);
    }
    if app.mode == Mode::ParamPrompt {
        draw_param_prompt(f, area, app);
    }
    if app.mode == Mode::ResultDiff {
        draw_result_diff(f, area, app);
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
    lines.push(Line::from(Span::styled(
        format!("pgman {} · beta", env!("CARGO_PKG_VERSION")),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )));
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
                    .title(Span::styled(" about ", Style::default().fg(theme.title))),
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
        ConnState::Disconnected => ("disconnected".to_string(), Style::default().fg(theme.muted)),
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
        ConnState::Failed(_) => (
            "connection failed".to_string(),
            Style::default().fg(theme.health_red),
        ),
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
    let visible = app.grid_visible_rows.len();
    let total = grid.row_count();
    let cap = if grid.truncated {
        format!(" · capped at {}", crate::grid::MAX_ROWS)
    } else {
        String::new()
    };
    let title = if app.grid_filter.is_some() && visible != total {
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

/// Persistent state badges prepended to the footer line: `[RO]`
/// when the connection is read-only, `[TX]` when an auto-tx is
/// currently open. Each is rendered as a coloured pill with a
/// trailing space, in stable order (RO before TX) so a stacked
/// pair lines up consistently.
pub(crate) fn footer_badges(app: &App, theme: &crate::theme::Theme) -> Vec<Span<'static>> {
    footer_badges_with(app, theme, crate::tap::dropped_at_listener())
}

/// Test-friendly variant of [`footer_badges`] that lets the
/// caller pin the listener-drop count. Production callers go
/// through [`footer_badges`] which reads the process-global
/// atomic; tests pass `0` to avoid cross-test leakage from a
/// shared counter.
pub(crate) fn footer_badges_with(
    app: &App,
    theme: &crate::theme::Theme,
    dropped_at_listener: u64,
) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    if app.read_only {
        out.push(Span::styled(
            " RO ",
            Style::default()
                .bg(theme.health_green)
                .fg(theme.row_alt_bg)
                .add_modifier(Modifier::BOLD),
        ));
        out.push(Span::raw(" "));
    }
    if app.tx_open {
        out.push(Span::styled(
            " TX ",
            Style::default()
                .bg(theme.health_yellow)
                .fg(theme.row_alt_bg)
                .add_modifier(Modifier::BOLD),
        ));
        out.push(Span::raw(" "));
    }
    // JDBC tap connection badge — flashed whenever the tap
    // listener has seen at least one event this session. F4
    // hint piggybacks on the badge so the operator knows where
    // to look.
    if app.tap_health.query_count > 0 || app.tap_health.heartbeat_count > 0 {
        out.push(Span::styled(
            " TAP ",
            Style::default()
                .bg(theme.health_green)
                .fg(theme.row_alt_bg)
                .add_modifier(Modifier::BOLD),
        ));
        out.push(Span::raw(" "));
    }
    // Backpressure: dropped-at-listener counter passed in via
    // the function parameter so tests can pin to 0. Non-zero
    // means the App couldn't keep up with the JAR / OTel agent
    // and events were lost at the listener boundary. Amber
    // badge for "you should look at this."
    let dropped = dropped_at_listener;
    if dropped > 0 {
        out.push(Span::styled(
            format!(" DROP ×{dropped} "),
            Style::default()
                .bg(theme.health_yellow)
                .fg(theme.row_alt_bg)
                .add_modifier(Modifier::BOLD),
        ));
        out.push(Span::raw(" "));
    }
    // N+1 alert badge — surfaces when the live detector finds
    // bursts in the current ring. The count lets operators see
    // multiple distinct N+1s at a glance ("N+1 ×3" = three
    // separate burst signatures). Hidden while the TapMonitor
    // panel is open (the operator can already see the findings
    // there — the chrome badge is redundant).
    if !matches!(app.mode, Mode::TapMonitor) {
        let nplus1_count = app.current_nplus1().len();
        if nplus1_count > 0 {
            out.push(Span::styled(
                format!(" N+1 ×{nplus1_count} "),
                Style::default()
                    .bg(theme.health_yellow)
                    .fg(theme.row_alt_bg)
                    .add_modifier(Modifier::BOLD),
            ));
            out.push(Span::raw(" "));
        }
    }
    out
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
        let mut spans = vec![
            Span::styled(" ⚠ ", Style::default().fg(theme.health_red)),
            Span::styled(err.clone(), Style::default().fg(theme.health_red)),
        ];
        if app.last_error_detail.is_some() {
            // F2 surfaces the rich detail — make the pointer
            // visible so operators discover it.
            spans.push(Span::styled(
                "  · F2 detail",
                Style::default().fg(theme.muted),
            ));
        }
        Line::from(spans)
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
        let failed_normal =
            app.mode == Mode::Normal && matches!(app.conn_state, ConnState::Failed(_));
        // While we're still mid-connect, surface that — the Normal hints
        // would suggest j/k/scroll affordances against a grid that
        // doesn't exist yet, and `r retry` wouldn't fire (only Failed
        // accepts r).
        let connecting_normal =
            app.mode == Mode::Normal && matches!(app.conn_state, ConnState::Connecting);
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
                    "F5 run · ctrl-z undo · ctrl-y redo · ctrl-r history · ctrl-e EXPLAIN · tab complete · ctrl-l log · esc"
                }
                Mode::HistorySearch => {
                    "type to search · ctrl-r next-older · ctrl-d delete · enter accept · esc cancel"
                }
                Mode::LogPick => "↑↓ / j/k navigate · enter load · c toggle clusters · esc cancel",
                Mode::ConnPick => "↑↓ / j/k navigate · enter connect · q quit",
            Mode::RowDetail => "↑↓ / j/k field · enter zoom · y yank · g/G first/last · esc close",
            Mode::CellDetail => {
                if app.json_cell_rows.is_empty() {
                    "↑↓ / j/k scroll · y yank · g/G top/bottom · esc / enter back"
                } else {
                    "j/k navigate · enter / space expand/collapse · y yank path · g/G top/bottom · esc back"
                }
            }
            Mode::About => "esc / enter / A close",
                // TxDecision is handled above with a return — this arm is unreachable.
                Mode::TxDecision => "y = commit · n / esc = rollback",
                Mode::Confirm => "y run · n / esc cancel",
                Mode::Normal => "q quit · ? help · e editor · S schema · W wizard · Q saved · T slow · L sessions · D diff · / filter · f find",
                Mode::GridFilter => "type to filter live · enter accept · esc clear",
                Mode::GridFind => "type to find · n/N jump · enter accept · esc clear",
                Mode::ExplainTree => "j/k navigate · enter expand/collapse · g/G top/bottom · q / esc close",
                Mode::SchemaBrowser => "j/k navigate · enter expand · [ ] jump schema · + / − all · / filter · s SELECT · i INSERT · q close",
                Mode::SchemaBrowserFilter => "type to narrow · enter accept · esc clear",
                Mode::SlowQueries => "j/k navigate · enter copy · r refresh · R auto-refresh · q / esc close",
                Mode::Sessions => "j/k navigate · K terminate · r refresh · R auto-refresh · q / esc close",
                Mode::SchemaLint => "j/k navigate · y yank suggestion · r refresh · q / esc close",
                Mode::ErrorDetail => "esc / q / F2 close",
                Mode::ConfirmTerminate => "y confirm terminate · n / esc cancel",
                Mode::Notifications => "j/k navigate · y yank payload · c clear · q / esc close",
                Mode::TapMonitor => "j/k navigate · v cycle 7 views · Shift-B baseline · s sort · c clear · q close",
                Mode::SavedQueries => "j/k navigate · enter load · r rename · d delete · / search · q close",
                Mode::SavedQueriesFilter => "type to narrow · enter accept · esc clear",
                Mode::RenameQueryPrompt => "edit name · enter save · esc cancel",
                Mode::SaveQueryPrompt => "type a name · enter persist · esc cancel",
                Mode::ParamPrompt => "type value · enter next · esc cancel",
                Mode::ResultDiff => "j/k navigate · r re-pin B as A · c clear pin · q / esc close",
            }
        };
        // Append a universal "F1 help" pointer to every non-modal
        // hint so the help overlay is discoverable from any mode
        // without the operator having to know `?` is the right
        // key. Skip in modes that already mention help, in input
        // modes where the user is typing literal characters, and
        // in the y/n prompts (TxDecision returns early upstream;
        // Confirm + Help are noisy with extra hints).
        let appended;
        let hints: &str = if matches!(
            app.mode,
            Mode::Help
                | Mode::Confirm
                | Mode::TxDecision
                | Mode::GridFilter
                | Mode::HistorySearch
                | Mode::SchemaBrowserFilter
                | Mode::GridFind
                | Mode::SaveQueryPrompt
                | Mode::ParamPrompt
                | Mode::SavedQueriesFilter
                | Mode::RenameQueryPrompt
        ) || failed_normal
            || connecting_normal
            || hints.contains("help")
        {
            hints
        } else {
            appended = format!("{hints} · F1 help");
            &appended
        };
        Line::from(Span::styled(
            format!(" {hints}"),
            Style::default().fg(theme.muted),
        ))
    };
    // Prepend persistent state badges: `[RO]` when read-only is in
    // effect (per safety profile), `[TX]` when an auto-tx is open.
    // Visible regardless of which footer branch (error / status /
    // hint) is active — TxDecision pre-empted with its own render
    // above, so we don't double up there.
    let badges = footer_badges(app, theme);
    let badge_width: u16 = badges
        .iter()
        .map(|s| s.content.chars().count() as u16)
        .sum();
    let mut combined: Vec<Span<'static>> = badges;
    for s in line.spans {
        combined.push(s);
    }
    f.render_widget(Paragraph::new(Line::from(combined)), area);

    // Real terminal cursor for the typing modes whose input is
    // surfaced through the footer (GridFilter, HistorySearch,
    // SchemaBrowserFilter). The status text always renders into a
    // single line at `area`; the leading " " prefix from the
    // status branch above is accounted for via `LEADING_SPACE`,
    // and any active `[RO]` / `[TX]` badges add their own width on
    // top.
    const LEADING_SPACE: u16 = 1;
    let cursor_offset: Option<u16> = match app.mode {
        Mode::GridFilter => app.grid_filter.as_ref().map(|f| {
            // Status reads "filter: /<pat>  · …"; cursor sits just
            // after the typed pattern.
            const PREFIX_CHARS: u16 = "filter: /".len() as u16;
            PREFIX_CHARS + f.chars().count() as u16
        }),
        Mode::HistorySearch => app.history_search.as_ref().map(|s| {
            // Two flavours, picked by `matched`:
            //   "(reverse-i-search) '<q>'"
            //   "(failed reverse-i-search) '<q>'"
            let prefix: u16 = if s.matched.is_some() {
                "(reverse-i-search) '".chars().count() as u16
            } else {
                "(failed reverse-i-search) '".chars().count() as u16
            };
            prefix + s.query.chars().count() as u16
        }),
        Mode::SchemaBrowserFilter => app.schema_browser_filter.as_ref().map(|f| {
            // Status reads "filter: /<pat>  · …" — same shape as
            // GridFilter.
            const PREFIX_CHARS: u16 = "filter: /".len() as u16;
            PREFIX_CHARS + f.chars().count() as u16
        }),
        Mode::GridFind => app.grid_find.as_ref().map(|f| {
            // Status reads "find: <pat>  · …".
            const PREFIX_CHARS: u16 = "find: ".len() as u16;
            PREFIX_CHARS + f.chars().count() as u16
        }),
        _ => None,
    };
    if let Some(offset) = cursor_offset {
        let x = area
            .x
            .saturating_add(LEADING_SPACE)
            .saturating_add(badge_width)
            .saturating_add(offset);
        if x < area.x.saturating_add(area.width) {
            f.set_cursor_position((x, area.y));
        }
    }
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
    let (cur_line_check, _) = crate::app::cursor_position(&app.editor_buffer, app.editor_cursor);
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
            crate::query::highlight::classify(raw, buf, &app.schema_cache, &from_before, &ctes)
        }
    } else {
        Vec::new()
    };

    let mut lines: Vec<Line> = Vec::new();
    let mut line_start_byte: usize = 0;
    for (i, line_text) in buf.split('\n').enumerate() {
        let prompt = if i == 0 { "> " } else { "  " };
        let mut spans: Vec<Span> = vec![Span::styled(
            prompt.to_string(),
            Style::default().fg(theme.muted),
        )];

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
                if i == cur_line {
                    Some(byte_at_col)
                } else {
                    None
                },
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
    f.render_widget(Paragraph::new(Text::from(lines)).scroll((scroll, 0)), inner);

    // Real terminal cursor — the blinking one most operators expect.
    // The REVERSED block underneath stays put so the column stays
    // obvious even when the OS cursor blinks off (some terminals
    // hide it during a long pause). Only shown when the editor is
    // focused.
    if focused {
        let visible_y = (cur_line as u16).saturating_sub(scroll);
        if visible_y < inner.height {
            // 2-char prompt prefix on every line ("> " or "  ").
            let x = inner.x.saturating_add(2).saturating_add(cur_col as u16);
            let y = inner.y.saturating_add(visible_y);
            if x < inner.x.saturating_add(inner.width) {
                f.set_cursor_position((x, y));
            }
        }
    }
}

/// Completion candidates popup. Anchored just under the editor pane,
/// flush-left over the body area. Shows up to ~10 candidates with the
/// active one highlighted; "↑ N more" / "↓ N more" markers when the
/// list is longer than the popup. Only the active cycle is rendered;
/// any non-Tab editor key dismisses (see `App::editor_key`).
fn draw_completion_popup(f: &mut Frame, editor_area: Rect, body_area: Rect, app: &App) {
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
    } else {
        focus_idx.saturating_sub(VISIBLE / 2)
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
    use crate::app::LogPickView;
    use crate::query::reconstruct::Source;
    let theme = &app.theme;
    let max_preview = 80usize;
    // One-line triage summary above the picker rows. Surfaces N+1
    // hotspots that the per-row list buries.
    let summary = crate::query::nplus1::summarize(&app.log_picks);
    let mut lines: Vec<Line> = Vec::new();
    let view_label = match app.log_pick_view {
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
    if let (LogPickView::AllQueries, Some(top)) = (app.log_pick_view, summary.top_cluster.as_ref())
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
    let row_lines: Vec<Line> = match app.log_pick_view {
        LogPickView::AllQueries => app
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
                let prefix = if i == app.log_pick_index {
                    "▶ "
                } else {
                    "  "
                };
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
            .collect(),
        LogPickView::Clusters => app
            .log_pick_clusters
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
                let prefix = if i == app.log_pick_index {
                    "▶ "
                } else {
                    "  "
                };
                let style = if i == app.log_pick_index {
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
            app.log_pick_index + 1
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

/// Startup picker shown when multiple data sources were discovered (e.g. an
/// IntelliJ project with several `.idea/dataSources.xml` entries). One row
/// per candidate; Enter starts the connection.
/// Wrap `s` to `width` columns, splitting on existing newlines first and
/// then chunking by character. Returns an empty-string vector for an empty
/// input so the caller still emits a visible row. Pure / testable.
/// Render the JSONB cell-detail tree into a list of styled Lines.
/// The cursor row is highlighted; containers get a ▼/▶ marker;
/// scalars are coloured by JSON type. Each row also shows its
/// jq-style path in dim text on the right so the operator can read
/// off the path without yanking.
pub(crate) fn render_json_tree(app: &App, width: usize) -> Vec<Line<'static>> {
    use crate::query::json_cell::{ContainerKind, JsonDisplay};
    let theme = &app.theme;
    let mut out = Vec::with_capacity(app.json_cell_rows.len());
    for (i, row) in app.json_cell_rows.iter().enumerate() {
        let indent = "  ".repeat(row.depth);
        let mut spans: Vec<Span<'static>> = Vec::new();
        let base_style = if i == app.json_cell_cursor {
            Style::default()
                .bg(theme.row_selected_bg)
                .fg(theme.text)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        spans.push(Span::styled(indent, base_style));

        match &row.display {
            JsonDisplay::Container {
                kind,
                len,
                expanded,
            } => {
                let marker = if *expanded { "▼ " } else { "▶ " };
                spans.push(Span::styled(marker.to_string(), base_style));
                if !row.key.is_empty() {
                    spans.push(Span::styled(
                        format!("{}: ", row.key),
                        base_style.fg(theme.title),
                    ));
                }
                let (open, close) = match kind {
                    ContainerKind::Object => ("{", "}"),
                    ContainerKind::Array => ("[", "]"),
                };
                let summary = if *expanded {
                    format!("{open} {len} {close}")
                } else {
                    format!("{open}…{close}  ({len})")
                };
                spans.push(Span::styled(summary, base_style.fg(theme.muted)));
            }
            JsonDisplay::Scalar(text) => {
                if !row.key.is_empty() {
                    spans.push(Span::styled(
                        format!("{}: ", row.key),
                        base_style.fg(theme.title),
                    ));
                }
                let v_style = if text == "null" {
                    base_style.fg(theme.muted).add_modifier(Modifier::ITALIC)
                } else if text == "true" || text == "false" {
                    base_style.fg(theme.accent)
                } else if text.starts_with('"') {
                    base_style.fg(theme.text)
                } else {
                    base_style.fg(theme.accent)
                };
                spans.push(Span::styled(text.clone(), v_style));
            }
        }

        // Pad and trim each line to width so the cursor row's
        // background extends to the right edge of the popup (without
        // it the highlight ends abruptly mid-line).
        let rendered: String = spans.iter().map(|s| s.content.as_ref()).collect();
        let visible_len = rendered.chars().count();
        if visible_len < width {
            let pad: String = std::iter::repeat_n(' ', width - visible_len).collect();
            spans.push(Span::styled(pad, base_style));
        }
        out.push(Line::from(spans));
    }
    out
}

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
    let column = app.grid.columns.get(field).cloned().unwrap_or_default();
    let value = row.get(field).cloned().unwrap_or_default();

    // Nest inside the row-detail popup so the zoom reads as drilling in,
    // not a new context. 90% of the screen so big JSON gets room.
    let popup = centered_pct(area, 90, 90);
    let inner_width = popup.width.saturating_sub(4) as usize; // borders + uniform(1) pad
    let inner_height = popup.height.saturating_sub(4);
    let is_empty = value.is_empty();

    let is_json = !app.json_cell_rows.is_empty();
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
    app.cell_detail_max_scroll = max_scroll;
    let effective_scroll = if is_json {
        // Keep the focused tree row visible — auto-scroll like the
        // grid does for its cursor.
        let cursor = app.json_cell_cursor as u16;
        let h = inner_height.max(1);
        let scroll = if cursor < app.cell_detail_scroll {
            cursor
        } else if cursor >= app.cell_detail_scroll + h {
            cursor + 1 - h
        } else {
            app.cell_detail_scroll
        };
        let scroll = scroll.min(max_scroll);
        app.cell_detail_scroll = scroll;
        scroll
    } else {
        app.cell_detail_scroll.min(max_scroll)
    };

    let lines: Vec<Line> = body_lines;

    let title = if is_json {
        format!(
            " {} · row {} of {} · field {}/{} · JSON ",
            column,
            idx + 1,
            app.grid.row_count(),
            field + 1,
            app.row_detail_field_count.max(1)
        )
    } else {
        format!(
            " {} · row {} of {} · field {}/{} ",
            column,
            idx + 1,
            app.grid.row_count(),
            field + 1,
            app.row_detail_field_count.max(1)
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
            let prefix = if i == app.data_source_pick_index {
                "▶ "
            } else {
                "  "
            };
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

/// Build the help body: a flat `Vec<Line>` plus an anchor → row
/// index map so callers can scroll to a named section.
pub(crate) fn help_body(
    theme: &crate::theme::Theme,
) -> (
    Vec<Line<'static>>,
    std::collections::HashMap<&'static str, u16>,
) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut anchors: std::collections::HashMap<&'static str, u16> =
        std::collections::HashMap::new();
    let push = |line: Line<'static>, lines: &mut Vec<Line<'static>>| {
        lines.push(line);
    };
    let heading = |label: &'static str,
                   lines: &mut Vec<Line<'static>>,
                   anchors: &mut std::collections::HashMap<&'static str, u16>| {
        anchors.insert(label, lines.len() as u16);
        lines.push(Line::from(Span::styled(
            format!("  {label}"),
            Style::default().fg(theme.accent),
        )));
    };
    let row = |t: &'static str| -> Line<'static> { Line::from(t) };

    push(
        Line::from(Span::styled(
            "pgman — keys (F1 help · F2 error detail · F3 notify · ? from grid)",
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        &mut lines,
    );
    push(Line::from(""), &mut lines);

    heading("grid", &mut lines, &mut anchors);
    push(row("    q             quit (esc here is a no-op so a reflex press doesn't lose the session)"), &mut lines);
    push(row("    ? / F1        toggle this help"), &mut lines);
    push(
        row("    A             about pgman (version, credits)"),
        &mut lines,
    );
    push(row("    e / i / tab   focus editor"), &mut lines);
    push(
        row("    c             change connection (opens the picker mid-session)"),
        &mut lines,
    );
    push(
        row("    S             schema browser (psql `\\d` equivalent)"),
        &mut lines,
    );
    push(
        row("    W             schema wizard / lint (missing PK, mixed-case, reserved words …)"),
        &mut lines,
    );
    push(
        row("    Q             saved queries (named, persisted)"),
        &mut lines,
    );
    push(
        row("    ctrl-t / ctrl-tab / alt-1..9  multi-tab — see the tabs section"),
        &mut lines,
    );
    push(
        row("    T             slow queries (pg_stat_statements top-N)"),
        &mut lines,
    );
    push(
        row("    L             active sessions + locks (pg_stat_activity)"),
        &mut lines,
    );
    push(
        row("    F3            LISTEN/NOTIFY arrivals panel (also reachable from any mode)"),
        &mut lines,
    );
    push(row("    h / l  ← →    move column cursor"), &mut lines);
    push(
        row("    s             cycle sort on focused column (off → ASC → DESC)"),
        &mut lines,
    );
    push(
        row("    /             live row filter (HIDES non-matching rows)"),
        &mut lines,
    );
    push(
        row("    f             find within grid (HIGHLIGHTS / jumps; n/N step matches)"),
        &mut lines,
    );
    push(row("    F             follow FK on focused cell → new tab with SELECT * FROM parent WHERE pk=value"), &mut lines);
    push(
        row("    Y             copy the (filtered) grid to clipboard as CSV"),
        &mut lines,
    );
    push(
        row("    I             yank focused row as INSERT (single-table SELECTs)"),
        &mut lines,
    );
    push(
        row("    m<a-z>        set bookmark at focused (row, col)"),
        &mut lines,
    );
    push(row("    '<a-z>        jump to bookmark"), &mut lines);
    push(row("    j / k  ↑ ↓    move selection"), &mut lines);
    push(row("    g / G         first / last row"), &mut lines);
    push(
        row("    enter         expand selected row (psql \\x style)"),
        &mut lines,
    );
    push(Line::from(""), &mut lines);

    heading("editor", &mut lines, &mut anchors);
    push(
        row("    F5 / ctrl-↵   run the statement (through safety guards)"),
        &mut lines,
    );
    push(
        row("    ctrl-z         undo last edit         ctrl-y / ctrl-shift-z  redo"),
        &mut lines,
    );
    push(
        row("    ctrl-c         cancel the running query (while in-flight)"),
        &mut lines,
    );
    push(
        row("    ctrl-e / F6   EXPLAIN  (never executes; tree-viewer opens)"),
        &mut lines,
    );
    push(
        row("    ctrl-a / F7   EXPLAIN ANALYZE  (DML wrapped in rollback tx)"),
        &mut lines,
    );
    push(
        row("    ctrl-/         toggle `-- ` line comment on the current line"),
        &mut lines,
    );
    push(
        row("    ( [ {          autoclose — pairs the open with its close; cursor between"),
        &mut lines,
    );
    push(row("    ctrl-r         reverse-incremental history search (ctrl-d in there deletes the focused entry)"), &mut lines);
    push(
        row("    ctrl-w         \\watch — re-run every 2 s; any key stops"),
        &mut lines,
    );
    push(
        row("    ctrl-x         open the buffer in $EDITOR (\\e)"),
        &mut lines,
    );
    push(
        row("    ctrl-s         save the current buffer as a named saved query"),
        &mut lines,
    );
    push(
        row("    ctrl-o         open the saved-queries panel (load one in)"),
        &mut lines,
    );
    push(
        row("    ctrl-f         pg_format the buffer (requires pgformatter)"),
        &mut lines,
    );
    push(
        row("    ctrl-l / F8   parse buffer as log → pick a reconstructed query"),
        &mut lines,
    );
    push(
        row("    ctrl-d / F9   read buffer as DBUnit fixture path → load apply script"),
        &mut lines,
    );
    push(
        row("    tab / ctrl-spc identifier completion (cycles on repeat tab)"),
        &mut lines,
    );
    push(
        row("    .             auto-trigger qualified completion (users.|)"),
        &mut lines,
    );
    push(
        row("    (in popup) type to narrow live · esc to restore typed prefix"),
        &mut lines,
    );
    push(row("    enter         insert newline"), &mut lines);
    push(
        row("    ↑ ↓ ← →       move cursor (col remembered across lines)"),
        &mut lines,
    );
    push(
        row("    home / end    start / end of current line"),
        &mut lines,
    );
    push(
        row("    ctrl-p / -n   prev / next history entry (history persists across restarts)"),
        &mut lines,
    );
    push(row("    ctrl-u        clear the buffer"), &mut lines);
    push(row("    esc           back to grid"), &mut lines);
    push(Line::from(""), &mut lines);

    heading("confirm", &mut lines, &mut anchors);
    push(
        row("    y             run the guarded (or pre-flight-flagged) statement"),
        &mut lines,
    );
    push(row("    n / esc       cancel"), &mut lines);
    push(Line::from(""), &mut lines);

    heading("tx open", &mut lines, &mut anchors);
    push(row("    y             commit the transaction"), &mut lines);
    push(row("    n / esc       roll back"), &mut lines);
    push(Line::from(""), &mut lines);

    heading("log pick", &mut lines, &mut anchors);
    push(row("    ↑ ↓ / j / k   navigate"), &mut lines);
    push(
        row("    enter         load selected query into the editor"),
        &mut lines,
    );
    push(
        row("    c             toggle between all-queries and N+1-cluster views"),
        &mut lines,
    );
    push(row("    esc / q       cancel"), &mut lines);
    push(Line::from(""), &mut lines);

    heading("conn pick", &mut lines, &mut anchors);
    push(row("    ↑ ↓ / j / k   navigate connections"), &mut lines);
    push(
        row("    enter         connect to focused entry"),
        &mut lines,
    );
    push(row("    g / G         first / last"), &mut lines);
    push(
        row("    q             quit (esc is a no-op so a reflex press doesn't drop you out)"),
        &mut lines,
    );
    push(Line::from(""), &mut lines);

    heading("row detail", &mut lines, &mut anchors);
    push(
        row("    j / k  ↑ ↓    move to next / previous field"),
        &mut lines,
    );
    push(row("    g / G         first / last field"), &mut lines);
    push(row("    PageUp/Down   jump 10 fields"), &mut lines);
    push(
        row("    enter         zoom into focused field (cell detail)"),
        &mut lines,
    );
    push(
        row("    y             yank focused field value to clipboard"),
        &mut lines,
    );
    push(row("    esc / q       close"), &mut lines);
    push(Line::from(""), &mut lines);

    heading("cell detail", &mut lines, &mut anchors);
    push(row("  text mode (non-JSON cells):"), &mut lines);
    push(row("    j / k  ↑ ↓    scroll"), &mut lines);
    push(
        row("    g / G         top / bottom · PageUp/Down  by 10"),
        &mut lines,
    );
    push(row("    y             yank value to clipboard"), &mut lines);
    push(row("    esc / enter   back to row detail"), &mut lines);
    push(
        row("  JSON mode (cell parses as object / array):"),
        &mut lines,
    );
    push(row("    j / k         navigate the tree"), &mut lines);
    push(
        row("    enter / space / h / l   expand or collapse focused container"),
        &mut lines,
    );
    push(
        row("    y             yank the jq-style path (.foo[0].bar)"),
        &mut lines,
    );
    push(Line::from(""), &mut lines);

    heading("schema browser", &mut lines, &mut anchors);
    push(
        row("    j / k  ↑ ↓    navigate schemas / tables / columns / constraints"),
        &mut lines,
    );
    push(
        row("    enter / space expand / collapse focused schema or table"),
        &mut lines,
    );
    push(
        row("    [ / ]         jump to previous / next schema (skip past table internals)"),
        &mut lines,
    );
    push(
        row("    + / −         expand-all / collapse-all (cursor stays on the focused path)"),
        &mut lines,
    );
    push(row("    PageUp/Down   jump 10 rows"), &mut lines);
    push(row("    /             in-tree filter (live; descendants of matches surface their ancestors)"), &mut lines);
    push(
        row("    s             yank SELECT * FROM <schema>.<table> LIMIT 100 template"),
        &mut lines,
    );
    push(
        row("    i             yank INSERT INTO … (cols) VALUES (NULL, …) template"),
        &mut lines,
    );
    push(row("    g / G         jump to top / bottom"), &mut lines);
    push(row("    esc / q       close"), &mut lines);
    push(Line::from(""), &mut lines);

    heading("EXPLAIN tree", &mut lines, &mut anchors);
    push(
        row("    j / k  ↑ ↓    navigate plan nodes (hottest node highlighted in red)"),
        &mut lines,
    );
    push(
        row("    enter         expand / collapse focused subtree"),
        &mut lines,
    );
    push(
        row("    g / G         jump to root / last visible node"),
        &mut lines,
    );
    push(row("    esc / q       close"), &mut lines);
    push(Line::from(""), &mut lines);

    heading("slow queries", &mut lines, &mut anchors);
    push(
        row("    j / k  ↑ ↓    navigate stored statements (sorted by total exec time)"),
        &mut lines,
    );
    push(
        row("    enter         copy focused SQL into the editor"),
        &mut lines,
    );
    push(
        row("    r             refresh from pg_stat_statements"),
        &mut lines,
    );
    push(
        row("    R             toggle auto-refresh (5 s polling)"),
        &mut lines,
    );
    push(row("    esc / q       close"), &mut lines);
    push(Line::from(""), &mut lines);

    heading("active sessions", &mut lines, &mut anchors);
    push(
        row("    j / k  ↑ ↓    navigate sessions (blocked ones sort to the top, red)"),
        &mut lines,
    );
    push(
        row("    K             pg_terminate_backend the focused session (confirm first)"),
        &mut lines,
    );
    push(
        row("    r             refresh from pg_stat_activity"),
        &mut lines,
    );
    push(
        row("    R             toggle auto-refresh (5 s polling)"),
        &mut lines,
    );
    push(row("    esc / q       close"), &mut lines);
    push(Line::from(""), &mut lines);

    heading("psql backslash commands", &mut lines, &mut anchors);
    push(
        row("    \\d              open schema browser (default view)"),
        &mut lines,
    );
    push(
        row("    \\d <name>       open schema browser filtered to <name>"),
        &mut lines,
    );
    push(
        row("    \\dt / \\dn       open schema browser (default view)"),
        &mut lines,
    );
    push(row("    \\?  / \\h        open this help"), &mut lines);
    push(row("    \\q              quit"), &mut lines);
    push(
        row("    \\timing [on/off]  toggle elapsed-ms in the status footer"),
        &mut lines,
    );
    push(
        row("    \\report [path]   write advisor + tap report (Markdown / HTML)"),
        &mut lines,
    );
    push(
        row("    \\fixture [path]  capture current result as a DBUnit fixture (XML)"),
        &mut lines,
    );
    push(Line::from(""), &mut lines);

    heading("tabs", &mut lines, &mut anchors);
    push(
        row("    ctrl-t        open a new tab (fresh editor + result)"),
        &mut lines,
    );
    push(
        row("    ctrl-w        close the current tab (no-op on the last one)"),
        &mut lines,
    );
    push(
        row("    ctrl-tab      next tab · ctrl-shift-tab  previous"),
        &mut lines,
    );
    push(row("    alt-1 .. 9    jump directly to tab N"), &mut lines);
    push(
        row("    (connection + schema cache + history + saved queries are SHARED across tabs)"),
        &mut lines,
    );
    push(Line::from(""), &mut lines);

    heading("saved queries", &mut lines, &mut anchors);
    push(
        row("    Q             open the saved-queries panel (also Ctrl-O from editor)"),
        &mut lines,
    );
    push(
        row("    Ctrl-S        (editor) save the current buffer with a name"),
        &mut lines,
    );
    push(
        row("    j / k  ↑ ↓    navigate · enter load · d delete · esc / q close"),
        &mut lines,
    );
    push(Line::from(""), &mut lines);

    heading("notifications", &mut lines, &mut anchors);
    push(
        row("    F3            open the NOTIFY arrivals panel (works from any mode)"),
        &mut lines,
    );
    push(
        row("    j / k  ↑ ↓    navigate · g / G  first / last · PageUp/Down  by 10"),
        &mut lines,
    );
    push(
        row("    y             yank focused payload to clipboard"),
        &mut lines,
    );
    push(row("    c             clear the ring"), &mut lines);
    push(row("    esc / q       close"), &mut lines);
    push(
        row("    (operator subscribes via `LISTEN <channel>` in the editor)"),
        &mut lines,
    );
    push(Line::from(""), &mut lines);

    heading("schema wizard", &mut lines, &mut anchors);
    push(
        row("    W (from grid) open the lint panel — pure checks over the schema cache"),
        &mut lines,
    );
    push(
        row("    j / k  ↑ ↓    navigate findings (sorted HIGH → MED → LOW)"),
        &mut lines,
    );
    push(
        row("    y             yank the focused finding's SQL suggestion (if any)"),
        &mut lines,
    );
    push(
        row("    r             refresh (re-runs every check)"),
        &mut lines,
    );
    push(
        row("    PageUp/Down   jump 10 rows · g / G  first / last"),
        &mut lines,
    );
    push(row("    esc / q       close"), &mut lines);
    push(row("    pure checks: LINT001 missing PK · 002 mixed-case · 003 reserved word · 004 mixed naming"), &mut lines);
    push(
        row("    live checks: LINT101 FK no index · 102 unused index · 103 duplicate indexes"),
        &mut lines,
    );
    push(
        row("                  · 104 bloat · 105 no comment · 106 mixed ts/tstz"),
        &mut lines,
    );
    push(Line::from(""), &mut lines);

    heading("help", &mut lines, &mut anchors);
    push(
        row("    j / k  ↑ ↓    scroll · g / G  top / bottom · PageUp/Down  by 10"),
        &mut lines,
    );
    push(
        row("    esc / ? / q / F1   close (returns to the mode you opened help from)"),
        &mut lines,
    );

    (lines, anchors)
}

fn draw_help(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = &app.theme;
    let (lines, anchors) = help_body(theme);
    // If we have a captured help_origin, pre-scroll to the matching
    // section the first time draw runs (`help_scroll` is reset to 0
    // by `open_help_from`; we detect that as "anchor not applied
    // yet" and set it once, then clear the origin).
    if let Some(origin) = app.help_origin {
        if app.help_scroll == 0 {
            if let Some(anchor) = App::help_anchor_for(origin) {
                if let Some(&row) = anchors.get(anchor) {
                    app.help_scroll = row;
                }
            }
        }
        // Consume the origin AFTER we've used it to position the
        // scroll. Subsequent draws (j/k navigation) shouldn't snap
        // back to the anchor.
        app.help_origin = None;
    }
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
    match rows.get(app.schema_browser_cursor) {
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
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    )));
    for (i, row) in app
        .slow_queries
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_h.saturating_sub(1))
    {
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
        let msg = app.last_status.clone().unwrap_or_else(|| "no rows".into());
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
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    )));
    for (i, row) in app
        .sessions
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_h.saturating_sub(1))
    {
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
        let blocked_disp = if is_blocked {
            row.blocked_by.as_str()
        } else {
            "-"
        };
        let line = format!(
            "  {:>6}  {:>20}  {:>10}  {:>8.1}  {:>8}  {}",
            row.pid, user_app, row.state, row.age_secs, blocked_disp, one_line
        );
        lines.push(Line::from(Span::styled(line, style)));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// Schema-lint panel (the "wizard" — `W` from Normal). Top half:
/// scrollable list of findings, severity-coloured. Bottom half:
/// detail strip for the focused finding with its full `detail`
/// text and any SQL suggestion.
fn draw_schema_lint(f: &mut Frame, area: Rect, app: &App) {
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

    if app.schema_lint_findings.is_empty() {
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
    let scroll = if app.schema_lint_cursor >= visible_h {
        app.schema_lint_cursor + 1 - visible_h
    } else {
        0
    };
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
        .schema_lint_findings
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_h.saturating_sub(1))
    {
        let is_focus = i == app.schema_lint_cursor;
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
    let focused = &app.schema_lint_findings[app
        .schema_lint_cursor
        .min(app.schema_lint_findings.len() - 1)];
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

/// Render the tab bar (one line above the editor). Active tab
/// is reverse-styled. Tab labels are auto-derived from the
/// buffer's first non-blank line (truncated), so the operator
/// sees what each tab is doing without naming them.
fn draw_tab_bar(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::raw(" "));
    for (i, _) in app.tabs.iter().enumerate() {
        let label = tab_label(app, i);
        let style = if i == app.active_tab {
            Style::default()
                .fg(theme.text)
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        spans.push(Span::styled(format!(" {} {label} ", i + 1), style));
        spans.push(Span::raw(" "));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Derive a short label for tab `idx` from its editor buffer's
/// first non-blank line. The ACTIVE tab reads from App's live
/// fields; the rest read from their stashed snapshot. Empty
/// buffers get the placeholder "(empty)" so the tab is still
/// addressable.
fn tab_label(app: &App, idx: usize) -> String {
    let body: &str = if idx == app.active_tab {
        &app.editor_buffer
    } else {
        app.tabs
            .get(idx)
            .map(|t| t.editor_buffer.as_str())
            .unwrap_or("")
    };
    let first: String = body
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("(empty)")
        .chars()
        .take(20)
        .collect();
    first
}

/// Saved-queries panel — list view with body preview for the
/// focused entry.
fn draw_saved_queries(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let popup = centered_pct(area, 88, 80);
    f.render_widget(Clear, popup);
    // Title carries the live filter when searching, plus a
    // shown/total count so a narrowed list is obvious.
    let visible = app.visible_saved_indices();
    let total = app.saved_queries.entries.len();
    let title = match app.saved_queries_filter.as_ref().map(|t| t.text()) {
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
                    app.saved_queries_filter
                        .as_ref()
                        .map(|t| t.text())
                        .unwrap_or("")
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

    let cursor = app.saved_queries_cursor.min(visible.len() - 1);
    let visible_h = top.height as usize;
    let scroll = if cursor >= visible_h {
        cursor + 1 - visible_h
    } else {
        0
    };
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
fn draw_rename_prompt(f: &mut Frame, area: Rect, app: &App) {
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
                app.rename_query_from
            ),
            Style::default().fg(theme.title),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    let lines = vec![
        Line::from(Span::styled("new name:", Style::default().fg(theme.muted))),
        Line::from(Span::styled(
            app.rename_query_buffer.text().to_string(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        )),
    ];
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
    let x = inner.x + app.rename_query_buffer.cursor_col() as u16;
    let y = inner.y + 1;
    if x < inner.x + inner.width {
        f.set_cursor_position((x, y));
    }
}

/// Name-prompt overlay for `Ctrl-S` — small centred box; the
/// editor stays visible behind it so the operator can re-check
/// what they're about to save.
fn draw_save_query_prompt(f: &mut Frame, area: Rect, app: &App) {
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
            app.save_query_name.clone(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        )),
    ];
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
    // Place the terminal cursor at end of the typed name.
    let prefix = 0u16; // already on its own line, no leading indent here
    let x = inner.x + prefix + app.save_query_name.chars().count() as u16;
    let y = inner.y + 1; // second line of the popup body
    if x < inner.x + inner.width {
        f.set_cursor_position((x, y));
    }
}

/// `:param` value prompt shown when loading a parameterised saved
/// query. Renders one input box for the current placeholder, the
/// progress (`2/3`), and the values already entered so the
/// operator can see what they've filled.
fn draw_param_prompt(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let Some(pp) = app.param_prompt.as_ref() else {
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
fn draw_notifications(f: &mut Frame, area: Rect, app: &App) {
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

    if app.notifications.is_empty() {
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
    let scroll = if app.notifications_cursor >= visible_h {
        app.notifications_cursor + 1 - visible_h
    } else {
        0
    };
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("  {:<20}  {:>6}  {}", "channel", "pid", "payload"),
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    )));
    for (i, n) in app
        .notifications
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_h.saturating_sub(1))
    {
        let is_focus = i == app.notifications_cursor;
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

/// Result-diff overlay (`Mode::ResultDiff`). Renders A-vs-B as a
/// grouped list: removed rows, then changed rows (with per-cell
/// old→new deltas), then added rows. Unchanged rows are only
/// counted in the header.
fn draw_result_diff(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let popup = centered_pct(area, 92, 80);
    f.render_widget(Clear, popup);
    let Some(state) = app.result_diff.as_ref() else {
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
    let cursor = app.result_diff_cursor.min(total.saturating_sub(1));
    let scroll = if cursor >= visible_h {
        cursor + 1 - visible_h
    } else {
        0
    };
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

/// JDBC-tap event monitor (`F4` from anywhere). Dispatches
/// to the recency list (L1) or the hotspots grouped view
/// (L2) depending on `app.tap_view`. Shift-G toggles between
/// them; `c` clears the ring; `q`/esc close.
fn draw_tap_monitor(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let popup = centered_pct(area, 92, 80);
    f.render_widget(Clear, popup);
    let dropped = app.tap_health.dropped_events_total;
    let dropped_suffix = if dropped > 0 {
        format!(" · {dropped} dropped")
    } else {
        String::new()
    };
    let view_label = match app.tap_view {
        crate::app::TapView::List => "list",
        crate::app::TapView::Hotspots => "hotspots",
        crate::app::TapView::Callers => "callers",
        crate::app::TapView::Transactions => "transactions",
        crate::app::TapView::Pools => "pools",
        crate::app::TapView::NplusOne => "N+1",
        crate::app::TapView::Baseline => "baseline",
    };
    let sort_suffix = if matches!(
        app.tap_view,
        crate::app::TapView::Hotspots | crate::app::TapView::Callers
    ) {
        format!(" · sort: {}", app.tap_sort.label())
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

    match app.tap_view {
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
    let cursor = app.tap_events_cursor.min(app.tap_events.len() - 1);
    let scroll = if cursor >= visible_h {
        cursor + 1 - visible_h
    } else {
        0
    };
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
    let cursor = app.tap_hotspots_cursor.min(hotspots.len() - 1);
    let scroll = if cursor >= visible_h {
        cursor + 1 - visible_h
    } else {
        0
    };
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
    let cursor = app.tap_callers_cursor.min(groups.len() - 1);
    let scroll = if cursor >= visible_h {
        cursor + 1 - visible_h
    } else {
        0
    };
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
    let cursor = app.tap_baseline_cursor.min(diffs.len() - 1);
    let scroll = if cursor >= table_h {
        cursor + 1 - table_h
    } else {
        0
    };
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
    let cursor = app.tap_txns_cursor.min(txns.len() - 1);
    let scroll = if cursor >= visible_h {
        cursor + 1 - visible_h
    } else {
        0
    };
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
    let cursor = app.tap_pools_cursor.min(pools.len() - 1);
    let scroll = if cursor >= body_h {
        cursor + 1 - body_h
    } else {
        0
    };
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
    let cursor = app.tap_nplus1_cursor.min(findings.len() - 1);
    let scroll = if cursor >= visible_h {
        cursor + 1 - visible_h
    } else {
        0
    };
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

/// Render the no-tap-yet onboarding hint for the TapMonitor
/// empty state. Lays out two routes:
/// - **OTLP/HTTP** (works today) — point any OTel-equipped
///   JVM at pgman's OTLP listener.
/// - **pgman-tap** (richer context; JAR ships separately) —
///   the bespoke Spring Boot starter with caller / pool /
///   txn data.
///
/// Pure (returns `Vec<Line>`) so the empty-state layout is
/// renderer-agnostic and snapshot-testable.
fn tap_setup_hint_lines(theme: &crate::theme::Theme) -> Vec<Line<'static>> {
    let muted = Style::default().fg(theme.muted);
    let title = Style::default()
        .fg(theme.title)
        .add_modifier(Modifier::BOLD);
    let code = Style::default().fg(theme.text);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled("no tap events yet", muted)));
    lines.push(Line::from(""));

    // Route 1 — OTLP (works today)
    lines.push(Line::from(Span::styled(
        "Route 1: OpenTelemetry (works today, any JVM)",
        title,
    )));
    lines.push(Line::from(Span::styled("  start pgman with:", muted)));
    lines.push(Line::from(Span::styled("    pgman --tap-otlp :4318", code)));
    lines.push(Line::from(Span::styled("  on the JVM side:", muted)));
    lines.push(Line::from(Span::styled(
        "    -javaagent:opentelemetry-javaagent.jar",
        code,
    )));
    lines.push(Line::from(Span::styled(
        "    OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://localhost:4318",
        code,
    )));
    lines.push(Line::from(Span::styled(
        "    OTEL_EXPORTER_OTLP_PROTOCOL=http/json",
        code,
    )));
    lines.push(Line::from(""));

    // Route 2 — pgman-tap (richer context; JAR ships separately)
    lines.push(Line::from(Span::styled(
        "Route 2: pgman-tap (richer context — caller / pool / txn)",
        title,
    )));
    lines.push(Line::from(Span::styled("  add to build.gradle:", muted)));
    lines.push(Line::from(Span::styled(
        "    implementation 'co.polymorphism:pgman-tap-spring-boot-starter:0.1.0'",
        code,
    )));
    lines.push(Line::from(Span::styled("  add to application.yml:", muted)));
    lines.push(Line::from(Span::styled(
        "    pgman.tap.enabled: true",
        code,
    )));
    lines.push(Line::from(Span::styled(
        "    pgman.tap.endpoint: tcp://localhost:7432",
        code,
    )));
    lines.push(Line::from(Span::styled(
        "  (the JAR is in development — Route 1 works today)",
        muted,
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Once events arrive: j/k navigate · v cycle views · s sort · F1 help",
        muted,
    )));
    lines
}

/// Render a duration in microseconds as a compact
/// human-readable string for the tap monitor.
fn format_duration(micros: u64) -> String {
    if micros < 1_000 {
        format!("{micros}µs")
    } else if micros < 1_000_000 {
        format!("{:.1}ms", micros as f64 / 1_000.0)
    } else {
        format!("{:.2}s", micros as f64 / 1_000_000.0)
    }
}

/// Rich error overlay (F2 after a query failure). Renders the
/// full server-side `DbError` fields in a labelled vertical
/// list. Read-only modal; closes on F2 / esc / q.
fn draw_error_detail(f: &mut Frame, area: Rect, app: &App) {
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
    fn format_duration_picks_unit_by_magnitude() {
        assert_eq!(format_duration(0), "0µs");
        assert_eq!(format_duration(999), "999µs");
        assert_eq!(format_duration(1_000), "1.0ms");
        assert_eq!(format_duration(1_500), "1.5ms");
        assert_eq!(format_duration(999_999), "1000.0ms");
        assert_eq!(format_duration(1_000_000), "1.00s");
        assert_eq!(format_duration(3_500_000), "3.50s");
    }

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
        assert!(!got[0].is_empty);
        assert_eq!(got[0].values, vec!["42"]);
        // Empty cell rendered with "(empty)" sentinel.
        assert!(got[1].is_empty);
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

    fn settled_app(read_only: bool, tx_open: bool) -> App {
        use crate::safety::SafetyConfig;
        use crate::theme::Theme;
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.read_only = read_only;
        a.tx_open = tx_open;
        a
    }

    #[test]
    fn footer_badges_empty_when_nothing_to_signal() {
        // Pin dropped=0 via the `_with` variant so a leaked
        // count from another test running in the same process
        // can't flip this assertion.
        let a = settled_app(false, false);
        assert!(footer_badges_with(&a, &a.theme, 0).is_empty());
    }

    #[test]
    fn footer_badges_render_ro_then_tx_in_stable_order() {
        let a = settled_app(true, true);
        let spans = footer_badges_with(&a, &a.theme, 0);
        // Pairs of (badge, space). Length 4: " RO ", " ", " TX ", " ".
        assert_eq!(spans.len(), 4);
        assert_eq!(spans[0].content, " RO ");
        assert_eq!(spans[1].content, " ");
        assert_eq!(spans[2].content, " TX ");
    }

    #[test]
    fn footer_badges_show_only_active_ones() {
        let a = settled_app(true, false);
        let spans = footer_badges_with(&a, &a.theme, 0);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content, " RO ");
        let a = settled_app(false, true);
        let spans = footer_badges_with(&a, &a.theme, 0);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content, " TX ");
    }

    #[test]
    fn footer_badges_drop_counter_surfaces_amber_badge() {
        let a = settled_app(false, false);
        let spans = footer_badges_with(&a, &a.theme, 42);
        let labels: Vec<String> = spans
            .iter()
            .map(|s| s.content.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        assert!(
            labels.iter().any(|l| l.contains("DROP ×42")),
            "expected DROP badge with count 42: {labels:?}"
        );
    }

    #[test]
    fn tap_setup_hint_includes_otel_and_pgman_tap_routes() {
        let theme = crate::theme::Theme::default();
        let lines = tap_setup_hint_lines(&theme);
        let dump: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        // Both routes named.
        assert!(dump.contains("Route 1: OpenTelemetry"), "got:\n{dump}");
        assert!(dump.contains("Route 2: pgman-tap"), "got:\n{dump}");
        // The flag + env vars the operator needs.
        assert!(dump.contains("--tap-otlp :4318"), "got:\n{dump}");
        assert!(
            dump.contains("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"),
            "got:\n{dump}"
        );
        assert!(dump.contains("OTEL_EXPORTER_OTLP_PROTOCOL"), "got:\n{dump}");
        // Spring Boot starter snippet.
        assert!(
            dump.contains("pgman-tap-spring-boot-starter"),
            "got:\n{dump}"
        );
        assert!(dump.contains("pgman.tap.enabled"), "got:\n{dump}");
        // Honest about the JAR still being in development.
        assert!(
            dump.contains("Route 1 works today"),
            "expected an honest note that Route 2 isn't shipped yet; got:\n{dump}"
        );
    }

    #[test]
    fn footer_badges_surface_tap_and_nplus1_when_findings_exist() {
        let mut a = settled_app(false, false);
        // Seed 6 same-shape events in one txn within window:
        // detect_nplus1 fires one finding.
        for i in 0..6u64 {
            a.on_tap_event(crate::tap::TapEvent {
                v: 1,
                kind: crate::tap::TapKind::Query,
                ts_unix_micros: i * 20_000,
                received_at_unix_micros: i * 20_000,
                app: Some("svc".into()),
                pool: None,
                conn: Some("c-1".into()),
                txn: Some("c-1#1".into()),
                sql: Some("SELECT * FROM t WHERE id = ?".into()),
                params: None,
                params_redacted: false,
                duration_micros: Some(1),
                rows: None,
                error: None,
                caller: None,
                dropped_events_total: None,
                txn_outcome: None,
            });
        }
        let spans = footer_badges_with(&a, &a.theme, 0);
        let labels: Vec<String> = spans
            .iter()
            .map(|s| s.content.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        assert!(
            labels.iter().any(|l| l == "TAP"),
            "expected TAP badge: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("N+1 ×1")),
            "expected N+1 ×1 badge: {labels:?}"
        );
    }
}
