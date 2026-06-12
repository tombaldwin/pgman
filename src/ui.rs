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

mod editor;
mod panels;
mod results;
mod schema;
mod tap;

use editor::{draw_completion_popup, draw_editor};
use panels::{
    draw_about, draw_confirm, draw_conn_pick, draw_error_detail, draw_explain_tree, draw_help,
    draw_log_pick, draw_notifications, draw_param_prompt, draw_rename_prompt,
    draw_save_query_prompt, draw_saved_queries, draw_sessions, draw_slow_queries,
};
use results::{draw_cell_detail, draw_grid, draw_result_diff, draw_row_detail};
use schema::{draw_schema_browser, draw_schema_lint};
use tap::draw_tap_monitor;

const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub fn draw(f: &mut Frame, app: &mut App) {
    if app.splash_visible {
        draw_splash(f, app);
        return;
    }
    let area = f.area();
    // Dynamic editor height: grow with the buffer up to a cap, with a min so
    // the focused empty editor still has a visible content line.
    let editor_lines = app.editor.buffer.matches('\n').count() + 1;
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
    if app.conn_pick.picks.len() >= 2 {
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
            if app.conn_pick.picks.len() >= 2 {
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
                if app.cell_detail.json_rows.is_empty() {
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
        Mode::GridFilter => app.grid_view.filter.as_ref().map(|f| {
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
        Mode::SchemaBrowserFilter => app.schema_browser.filter.as_ref().map(|f| {
            // Status reads "filter: /<pat>  · …" — same shape as
            // GridFilter.
            const PREFIX_CHARS: u16 = "filter: /".len() as u16;
            PREFIX_CHARS + f.chars().count() as u16
        }),
        Mode::GridFind => app.grid_find.needle.as_ref().map(|f| {
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
    let mut out = Vec::with_capacity(app.cell_detail.json_rows.len());
    for (i, row) in app.cell_detail.json_rows.iter().enumerate() {
        let indent = "  ".repeat(row.depth);
        let mut spans: Vec<Span<'static>> = Vec::new();
        let base_style = if i == app.cell_detail.json_cursor {
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
        &app.editor.buffer
    } else {
        app.tabs
            .get(idx)
            .map(|t| t.editor.buffer.as_str())
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

/// A centred `w`%×`h`% rectangle within `area`.
fn centered_pct(area: Rect, w: u16, h: u16) -> Rect {
    // Compute in u32: `area.width * w` overflows u16 once the terminal is
    // ~713+ columns wide (e.g. 713 × 92% > 65535), which panics in debug and
    // wraps to a garbage rect in release. Both operands are tiny, so u32 is
    // always lossless and the result fits back in u16.
    let width = (area.width as u32 * w as u32 / 100) as u16;
    let height = (area.height as u32 * h as u32 / 100) as u16;
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

/// Top index of a scrolled list window: keeps `cursor` in view within a
/// `visible`-row viewport (0 until the cursor passes the last visible row).
fn scroll_offset(cursor: usize, visible: usize) -> usize {
    if cursor >= visible {
        cursor + 1 - visible
    } else {
        0
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
mod tests;
