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
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

mod editor;
mod landing;
mod panels;
mod results;
mod schema;
mod tap;

use editor::{draw_completion_popup, draw_editor};
use landing::draw_landing;
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
    // Every overlay below is centred inside the BODY, not the whole
    // terminal: the header carries the connection state and the footer
    // carries the mode's only close hint, and a popup tall enough to
    // reach either row used to paint its own border over it (the About
    // card at 80x24 replaced the footer with `└────┘`). One rect,
    // computed once, so no overlay can drift back out of it.
    let area = body_area(area);
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
    // The two pickers float inside the results panel rather than
    // being centred on the body: centred, the popup's own frame landed
    // on the panel's top border and title (`┌ pgman┌ pick a
    // connection`). Same treatment the completion popup already gets.
    if app.mode == Mode::LogPick {
        draw_log_pick(f, chunks[3], app);
    }
    if app.mode == Mode::ConnPick {
        draw_conn_pick(f, chunks[3], app);
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
            format!("connected · pg {}", short_server_version(server_version)),
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
    if let Some(update) = &app.update_available {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("⬆ {}", update.version),
            Style::default().fg(theme.accent),
        ));
    }
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
            if app.mode == Mode::ConnPick {
                // `draw_conn_pick` (called from `draw` once the body has
                // rendered) draws its own framed, titled popup over this
                // area with the actual picks — a "no connection" message
                // here would sit directly above it and contradict it.
                // Leave the body as an empty frame and let the picker own
                // the content.
                f.render_widget(bordered(&app.theme, "pgman"), area);
            } else if app.conn_pick.picks.is_empty() {
                // Genuinely nothing to connect to and nothing discovered —
                // spell out both ways forward.
                f.render_widget(
                    Paragraph::new(Text::from(vec![
                        Line::from("no connection — start pgman with --dsn postgres://…"),
                        Line::from(
                            "pgman also auto-discovers application*.yml, .idea/dataSources.xml and .pgman/pgman.toml when run inside a project",
                        ),
                    ]))
                    .style(Style::default().fg(app.theme.muted))
                    .block(bordered(&app.theme, "pgman")),
                    area,
                );
            } else {
                // Discovered connections exist but the picker isn't open
                // right now (e.g. dismissed via `q`) — point at how to
                // reopen it instead of implying there's no way to connect.
                f.render_widget(
                    Paragraph::new("no connection — press c to choose a connection")
                        .style(Style::default().fg(app.theme.muted))
                        .block(bordered(&app.theme, "pgman")),
                    area,
                );
            }
        }
        ConnState::Connected { .. } => {
            if app.grid.is_empty() {
                // `grid.columns` is empty only when nothing has ever run
                // (App::new's default `Grid`) — any real result, including
                // a genuinely empty one, leaves its column list behind.
                // That's what tells "nothing run yet" (show the start
                // card) apart from "ran a query, got zero rows" (still
                // `(no rows)`); an error also forces the plain message so
                // it isn't hidden behind a welcome screen.
                if app.grid.columns.is_empty() && app.last_error.is_none() {
                    draw_landing(f, area, app);
                } else {
                    f.render_widget(
                        Paragraph::new("(no rows)")
                            .style(Style::default().fg(app.theme.muted))
                            .block(bordered(&app.theme, "result")),
                        area,
                    );
                }
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
    if !app.conn_pick.picks.is_empty() {
        actions.push_str(" · p change connection");
    }
    // Resolved through `util::cache_dir()` (not hand-typed) so this
    // stays correct if the cache location ever moves.
    let log_path = crate::util::cache_dir().join("pgman.log");
    actions.push_str(&format!(" · q quit · logs in {}", log_path.display()));
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

/// Display columns `s` occupies in a terminal — NOT its char count.
/// A CJK server error (`lc_messages=ja_JP`) is roughly two columns per
/// char, so a 40-char message painted 67 columns wide and shoved the
/// protected `· F2 detail` pointer off the end of a row that measured
/// as fitting. Every footer/status width budget goes through this.
pub(crate) fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Display columns one `char` occupies (a combining mark is 0, a
/// full-width CJK glyph 2, a control char treated as 0).
fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// Fit a ` · `-joined hint string into `width` columns without ever
/// truncating a hint mid-item. `hints` is treated as an ordered list of
/// `" · "`-separated items, already written most-important-first; when
/// the full string doesn't fit, whole trailing items are dropped and
/// replaced with a single `"F1 +N more"` item (so the join reads `"…kept ·
/// F1 +N more"`) telling the operator how many hints were cut and that
/// F1 — help, reachable from every mode — is where they are. The returned
/// string is never wider than `width` — if even the first item plus the
/// marker can't fit, the marker alone is returned; if that doesn't fit
/// either, an empty string is returned.
pub(crate) fn fit_hints(hints: &str, width: usize) -> String {
    const SEP: &str = " · ";
    if hints.is_empty() || display_width(hints) <= width {
        return hints.to_string();
    }
    let items: Vec<&str> = hints.split(SEP).collect();
    // Try keeping progressively fewer leading items, each candidate
    // capped off with an "F1 +N more" marker accounting for the rest.
    for kept in (0..items.len()).rev() {
        let remaining = items.len() - kept;
        let marker = format!("F1 +{remaining} more");
        let mut pieces: Vec<&str> = items[..kept].to_vec();
        pieces.push(&marker);
        let candidate = pieces.join(SEP);
        if display_width(&candidate) <= width {
            return candidate;
        }
    }
    // Not even the marker for every item fits.
    String::new()
}

/// `"1 query"` / `"8 queries"` — a count and its noun, pluralised.
/// The crate's other count labels use the `row(s)` idiom; that reads
/// badly in a panel title next to a second count, so titles use this
/// instead — but consistently, every count in the title or none.
/// Pure / testable.
pub(crate) fn count_label(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Fit a ` · `-joined panel title into `width` columns by dropping
/// whole trailing segments, marking the cut with a trailing `…`
/// segment. A title left to the border gets cut mid-word instead
/// (`… Shift-B base┐`), which reads as a rendering fault rather than
/// as "there is more here". The first segment — the panel's identity
/// and its counts — is kept even when it alone has to be ellipsised.
/// Pure / testable.
pub(crate) fn fit_title(title: &str, width: usize) -> String {
    const SEP: &str = " · ";
    if display_width(title) <= width {
        return title.to_string();
    }
    let items: Vec<&str> = title.split(SEP).collect();
    for kept in (1..items.len()).rev() {
        let candidate = format!("{}{SEP}…", items[..kept].join(SEP));
        if display_width(&candidate) <= width {
            return candidate;
        }
    }
    crate::grid::truncate_cell(items[0], width)
}

/// Middle-ellipsise `s` down to `target_len` chars, keeping both ends —
/// `"abc…xyz"` — since for a quoted SQL statement the tail matters as
/// much as the head (a `WHERE` clause, a trailing `;`). `target_len` is
/// the *exact* char budget the result must not exceed; if `s` already
/// fits, it's returned unchanged. `target_len == 0` yields `""`;
/// `target_len == 1` yields just the ellipsis marker (no room for either
/// end). Pure / testable.
fn middle_ellipsis(s: &str, target_len: usize) -> String {
    if display_width(s) <= target_len {
        return s.to_string();
    }
    if target_len == 0 {
        return String::new();
    }
    if target_len == 1 {
        return "…".to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let avail = target_len - 1; // minus the ellipsis marker itself
    let front_budget = avail.div_ceil(2);
    let back_budget = avail - front_budget;
    // Walk in from each end by display columns, never letting the two
    // walks cross — a double-width glyph that only half fits is
    // dropped rather than cut.
    let mut fi = 0;
    let mut fw = 0;
    while fi < chars.len() {
        let cw = char_width(chars[fi]);
        if fw + cw > front_budget {
            break;
        }
        fw += cw;
        fi += 1;
    }
    let mut bi = chars.len();
    let mut bw = 0;
    while bi > fi {
        let cw = char_width(chars[bi - 1]);
        if bw + cw > back_budget {
            break;
        }
        bw += cw;
        bi -= 1;
    }
    let front_str: String = chars[..fi].iter().collect();
    let back_str: String = chars[bi..].iter().collect();
    format!("{front_str}…{back_str}")
}

/// End-ellipsise `s` down to `width` chars — `"abc…"` — used only as the
/// last resort when even the protected final segment (see [`fit_status`])
/// doesn't fit on its own. Pure / testable.
fn end_ellipsis(s: &str, width: usize) -> String {
    if display_width(s) <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_string();
    }
    let budget = width - 1; // minus the ellipsis marker itself
    let mut keep = String::new();
    let mut kw = 0;
    for c in s.chars() {
        let cw = char_width(c);
        if kw + cw > budget {
            break;
        }
        kw += cw;
        keep.push(c);
    }
    format!("{keep}…")
}

/// Fit a ` · `-joined status/error line into `width` columns without ever
/// cutting a word mid-letter. Unlike [`fit_hints`] (whole hint items,
/// dropped from the tail), the *last* segment here is load-bearing — it
/// carries the action keys (`y confirm · n cancel`) or a discoverability
/// pointer (`F2 detail`) — so it is never dropped and never cut except as
/// an absolute last resort.
///
/// Order of attack, stopping as soon as the candidate fits:
/// 1. If `text` already fits, return it unchanged.
/// 2. Split on `" · "`. Repeatedly middle-ellipsise the longest segment
///    other than the last (`"abc…xyz"`, keeping both ends) until either
///    the join fits or every other segment has been collapsed to a bare
///    `"…"`.
/// 3. Still too wide → drop whole leading segments (the ones already
///    reduced to `"…"` first, by construction) until only the last
///    segment remains.
/// 4. Still too wide (the last segment alone doesn't fit) → end-
///    ellipsise the last segment (`"abc…"`).
///
/// The returned string is never wider than `width`.
pub(crate) fn fit_status(text: &str, width: usize) -> String {
    const SEP: &str = " · ";
    if display_width(text) <= width {
        return text.to_string();
    }
    let mut segments: Vec<String> = text.split(SEP).map(str::to_string).collect();
    let last_idx = segments.len() - 1;

    // Step 2: shrink the longest non-last segment, repeatedly, until the
    // join fits or nothing non-last is left to shrink. Only segments
    // long enough to be prose or SQL are shrunk: a twelve-character key
    // hint middle-ellipsised reads as broken (`enter…cept`), and a
    // dropped hint reads as a choice.
    const MIN_SHRINK: usize = 16;
    loop {
        let candidate = segments.join(SEP);
        let over = display_width(&candidate).saturating_sub(width);
        if over == 0 {
            return candidate;
        }
        let longest = segments
            .iter()
            .enumerate()
            .filter(|(i, s)| *i != last_idx && display_width(s) >= MIN_SHRINK)
            .max_by_key(|(_, s)| display_width(s));
        let Some((i, s)) = longest else { break };
        let target_len = display_width(s).saturating_sub(over).max(1);
        segments[i] = middle_ellipsis(s, target_len);
    }

    // Step 2b: nothing left to shrink. Drop whole segments from the
    // second onwards — the first is the context (`find: pro`), the
    // last is the action keys; what sits between is the most
    // dispensable.
    while segments.len() > 2 {
        let candidate = segments.join(SEP);
        if display_width(&candidate) <= width {
            return candidate;
        }
        segments.remove(1);
    }

    // Step 3: every other segment is now "…" (or there were none) —
    // drop leading segments outright until only the last remains.
    while segments.len() > 1 {
        let candidate = segments.join(SEP);
        if display_width(&candidate) <= width {
            return candidate;
        }
        segments.remove(0);
    }

    // Step 4: the last segment alone doesn't fit — end-ellipsise it.
    // (`segments[0]` here since drops above always leave the last
    // segment at index 0.)
    end_ellipsis(&segments[0], width)
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
    // Badges occupy width before the hints text; compute them up front so
    // the hints branch below knows how much room it actually has. `line`'s
    // leading " " (added by every branch) is accounted for separately via
    // `LEADING_SPACE`, reused for the cursor-position math further down.
    const LEADING_SPACE: u16 = 1;
    let badges = footer_badges(app, theme);
    let badge_width: u16 = badges
        .iter()
        .map(|s| display_width(&s.content) as u16)
        .sum();
    // Priority: the `:` command bar (the operator is typing INTO the
    // footer — nothing else may occupy it) > query error > status
    // (e.g. "EXPLAIN ok · 4 rows") > hints.
    let line = if let Some(bar) = &app.command_bar {
        Line::from(vec![
            Span::styled(" :", Style::default().fg(theme.accent)),
            Span::styled(
                bar.input.text().to_string(),
                Style::default().fg(theme.text),
            ),
        ])
    } else if let Some(err) = &app.last_error {
        let icon = " ⚠ ";
        // The "F2 detail" pointer is the load-bearing part of this line
        // when it's present — it's how the operator finds the rich error
        // view — so it's appended as `fit_status`'s protected last
        // segment (joined with the same " · " it uses to split on)
        // rather than as a separate span that could get truncated away
        // by a raw cell-width clip.
        let pointer = if app.last_error_detail.is_some() {
            " · F2 detail"
        } else {
            ""
        };
        let full = format!("{err}{pointer}");
        let available = area
            .width
            .saturating_sub(badge_width)
            .saturating_sub(display_width(icon) as u16) as usize;
        let fitted = fit_status(&full, available);
        let spans = if !pointer.is_empty() && fitted.ends_with(pointer) {
            let msg_part = &fitted[..fitted.len() - pointer.len()];
            vec![
                Span::styled(icon, Style::default().fg(theme.health_red)),
                Span::styled(msg_part.to_string(), Style::default().fg(theme.health_red)),
                Span::styled(pointer.to_string(), Style::default().fg(theme.muted)),
            ]
        } else {
            vec![
                Span::styled(icon, Style::default().fg(theme.health_red)),
                Span::styled(fitted, Style::default().fg(theme.health_red)),
            ]
        };
        Line::from(spans)
    } else if let Some(status) = &app.last_status {
        let sp = SPINNER[app.anim_tick % SPINNER.len()];
        let prefix = if app.query_running {
            format!(" {sp} ")
        } else {
            " ".to_string()
        };
        let available = area
            .width
            .saturating_sub(badge_width)
            .saturating_sub(display_width(&prefix) as u16) as usize;
        let fitted = fit_status(status, available);
        Line::from(Span::styled(
            format!("{prefix}{fitted}"),
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
        // The picker's list keys are wrong while its ssh-tunnel
        // confirmation is up: `enter` does nothing, `q` cancels along
        // with every other key, and the footer said `enter connect ·
        // q quit` under a prompt asking a yes/no question about
        // running ssh with the operator's keys.
        let tunnel_prompt = app.mode == Mode::ConnPick && app.pending_tunnel.is_some();
        let hints: &str = if tunnel_prompt {
            "y proceed · any other key cancels"
        } else if failed_normal {
            // Same gate as the failure card's action line and the `p`
            // key handler: one discovered candidate is still a picker
            // worth opening.
            if !app.conn_pick.picks.is_empty() {
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
                // Unreachable in practice: the command-bar branch at
                // the head of this chain owns the footer while the bar
                // is open. Kept so the match stays exhaustive.
                Mode::CommandBar => "type a command · enter run · tab complete · esc cancel",
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
                | Mode::CommandBar
        ) || tunnel_prompt
            || failed_normal
            || connecting_normal
            || hints.contains("help")
        {
            hints
        } else {
            appended = format!("{hints} · F1 help");
            &appended
        };
        // Fit whole hints into what's left after the badges and the
        // leading space — never truncate a hint mid-word. When some are
        // dropped, `fit_hints` appends an "F1 +N more" marker so the operator
        // knows there's more and roughly how much.
        let available = area
            .width
            .saturating_sub(badge_width)
            .saturating_sub(LEADING_SPACE) as usize;
        let fitted = fit_hints(hints, available);
        Line::from(Span::styled(
            format!(" {fitted}"),
            Style::default().fg(theme.muted),
        ))
    };
    // Prepend persistent state badges: `[RO]` when read-only is in
    // effect (per safety profile), `[TX]` when an auto-tx is open.
    // Visible regardless of which footer branch (error / status /
    // hint) is active — TxDecision pre-empted with its own render
    // above, so we don't double up there.
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
    let cursor_offset: Option<u16> = match app.mode {
        // The bar renders as ":<typed>"; the cursor sits inside the
        // typed text at the widget's own column.
        Mode::CommandBar => app.command_bar.as_ref().map(|bar| {
            const PREFIX_CHARS: u16 = 1; // the ':'
            PREFIX_CHARS + bar.input.cursor_col() as u16
        }),
        Mode::GridFilter => app.grid_view.filter.as_ref().map(|f| {
            // Status reads "filter: /<pat>  · …"; cursor sits just
            // after the typed pattern.
            const PREFIX_CHARS: u16 = "filter: /".len() as u16;
            PREFIX_CHARS + display_width(f) as u16
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
            prefix + display_width(&s.query) as u16
        }),
        Mode::SchemaBrowserFilter => app.schema_browser.filter.as_ref().map(|f| {
            // Status reads "filter: /<pat>  · …" — same shape as
            // GridFilter.
            const PREFIX_CHARS: u16 = "filter: /".len() as u16;
            PREFIX_CHARS + display_width(f) as u16
        }),
        Mode::GridFind => app.grid_find.needle.as_ref().map(|f| {
            // Status reads "find: <pat>  · …".
            const PREFIX_CHARS: u16 = "find: ".len() as u16;
            PREFIX_CHARS + display_width(f) as u16
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
            "pgman — keys (? / F1 help · : commands · F2 error detail · F3 notify)",
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        &mut lines,
    );
    push(Line::from(""), &mut lines);

    heading("grid", &mut lines, &mut anchors);
    push(row("    q             quit (esc here is a no-op so a reflex press doesn't lose the session)"), &mut lines);
    push(
        row("    ? / F1        toggle this help (both work from every non-typing mode)"),
        &mut lines,
    );
    push(
        row("    :             command bar — see the ': commands' section"),
        &mut lines,
    );
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
        row("    D             pin result as A · run another query, D again to diff as B"),
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
        row("    ( [ {  ' \"    autoclose / skip-over — pairs the open with its close (cursor between); typing the close (or matching quote) over it just steps past"),
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
    push(row("    esc / q / enter  back to row detail"), &mut lines);
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
    push(row("    esc / q       back to row detail"), &mut lines);
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
    push(
        row("    \\l                list databases (name + size)"),
        &mut lines,
    );
    push(
        row("    \\x [on/off]       toggle expanded (row-detail) output"),
        &mut lines,
    );
    push(
        row("    \\c [name]         connect · no name opens the picker"),
        &mut lines,
    );
    push(
        row("    \\i <path>         load a SQL file into the editor (doesn't run it)"),
        &mut lines,
    );
    push(Line::from(""), &mut lines);

    heading(": commands", &mut lines, &mut anchors);
    push(
        row("    :             open the command bar (any mode except while typing)"),
        &mut lines,
    );
    push(
        row("                  enter runs · esc cancels · tab completes the command name"),
        &mut lines,
    );
    push(row("    :about          the About card"), &mut lines);
    push(
        row("    :update         About card + where the release check got to"),
        &mut lines,
    );
    push(
        row("    :help [topic]   this help · topics: grid, editor, commands, schema, saved,"),
        &mut lines,
    );
    push(
        row("                    slow, sessions, tap, explain, diff, wizard"),
        &mut lines,
    );
    push(row("    :quit  /  :q    quit pgman"), &mut lines);
    push(
        row("    :readonly on|off  the read-only flag pgman opens connections with; applies"),
        &mut lines,
    );
    push(
        row("                    at the next connect. Refused when safety.toml pins this"),
        &mut lines,
    );
    push(row("                    database read-only."), &mut lines);
    push(
        row("    :connect [NAME] the picker, or the named data source (= \\c). Quote a name"),
        &mut lines,
    );
    push(
        row("                    with spaces, or give a unique prefix of it."),
        &mut lines,
    );
    push(
        row("    (:l :x :dt :dn :d NAME :i PATH :timing :report :fixture — the backslash"),
        &mut lines,
    );
    push(
        row("     commands above, same arguments, without the backslash)"),
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
    push(
        row("    r             rename the focused entry"),
        &mut lines,
    );
    push(
        row("    /             live filter (name + body) · esc restores full list"),
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

    heading("jdbc tap", &mut lines, &mut anchors);
    push(
        row("    F4            open the JDBC tap monitor (live stream, works from any mode)"),
        &mut lines,
    );
    push(
        row("    v             cycle view: list → hotspots → callers → txns → pools → N+1 → baseline"),
        &mut lines,
    );
    push(
        row("    j / k  ↑ ↓    navigate · g / G  first / last · PageUp/Down  by 10"),
        &mut lines,
    );
    push(
        row("    s             cycle sort (hotspots / callers views)"),
        &mut lines,
    );
    push(
        row("    B             capture a baseline snapshot (any view; see it under baseline)"),
        &mut lines,
    );
    push(row("    c             clear the event ring"), &mut lines);
    push(row("    esc / q       close"), &mut lines);
    push(Line::from(""), &mut lines);

    heading("result diff", &mut lines, &mut anchors);
    push(
        row("    D (from grid) pin the current result as A · run another query, D again for B"),
        &mut lines,
    );
    push(
        row("    j / k  ↑ ↓    navigate diff rows · g / G  first / last · PageUp/Down  by 10"),
        &mut lines,
    );
    push(
        row("    r             re-pin the current B side as a new A (iterate)"),
        &mut lines,
    );
    push(
        row("    c             clear the pinned baseline"),
        &mut lines,
    );
    push(row("    esc / q       close"), &mut lines);
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
    vec![
        Line::from(Span::styled("no tap events yet", muted)),
        Line::from(""),
        // Route 1 — OTLP (works today)
        Line::from(Span::styled(
            "Route 1: OpenTelemetry (works today, any JVM)",
            title,
        )),
        Line::from(Span::styled("  start pgman with:", muted)),
        Line::from(Span::styled("    pgman --tap-otlp :4318", code)),
        Line::from(Span::styled("  on the JVM side:", muted)),
        Line::from(Span::styled(
            "    -javaagent:opentelemetry-javaagent.jar",
            code,
        )),
        Line::from(Span::styled(
            "    OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://localhost:4318",
            code,
        )),
        Line::from(Span::styled(
            "    OTEL_EXPORTER_OTLP_PROTOCOL=http/json",
            code,
        )),
        Line::from(""),
        // Route 2 — pgman-tap. Not shipped: no coordinate here, since
        // the one that used to be printed
        // (`co.polymorphism:pgman-tap-spring-boot-starter:0.1.0`)
        // resolves to nothing, and a build file edited to use it fails
        // six lines before the note saying the JAR is in development.
        Line::from(Span::styled("Route 2: pgman-tap — not yet released", title)),
        Line::from(Span::styled(
            "  will add caller, pool and transaction context to each",
            muted,
        )),
        Line::from(Span::styled(
            "  query. Nothing to install yet — use Route 1 today.",
            muted,
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Once events arrive: j/k navigate · v cycle views · s sort · F1 help",
            muted,
        )),
    ]
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

/// The release number out of a `server_version` string, dropping the
/// packager's parenthesised build detail: `"16.15 (Debian
/// 16.15-1.pgdg13+2)"` → `"16.15"`. The header and the start card both
/// state the version in passing, where the Debian build id is noise
/// twice over; the full string stays in the About overlay, which is
/// where someone chasing a packaging difference would look. Cut at the
/// first `" ("` so a version with no build detail passes through
/// unchanged. Pure / testable.
pub(crate) fn short_server_version(v: &str) -> &str {
    match v.find(" (") {
        Some(i) => v[..i].trim_end(),
        None => v,
    }
}

/// The body rect of a full-terminal `area`: everything between the
/// one-row header and the one-row footer. Overlays are centred inside
/// this rather than the whole terminal, so a popup can never paint
/// over the connection state above or the close hint below. Degrades
/// to the input rect when there aren't two rows to give up.
fn body_area(area: Rect) -> Rect {
    if area.height < 3 {
        return area;
    }
    Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height - 2,
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

/// A popup floated one row and one column inside `panel`, sized to
/// `content_w` × `content_h` but never past the room that leaves.
/// Anchored top-left rather than centred: a popup centred on a panel
/// it is nearly as wide as puts its own top border on the panel's
/// border and title, fusing the two frames into one run of glyphs
/// (`┌ pgman┌ pick a connection`). Falls back to centring when the
/// panel is too small to float inside at all.
fn floated_in_panel(panel: Rect, content_w: u16, content_h: u16) -> Rect {
    if panel.width < 4 || panel.height < 4 {
        return centered(panel, content_w, content_h);
    }
    Rect {
        x: panel.x + 1,
        y: panel.y + 1,
        width: content_w.min(panel.width - 2),
        height: content_h.min(panel.height - 2),
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
