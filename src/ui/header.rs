//! The two-box header, as in ebman: on the left the build (in the
//! border), the connection and the session; on the right the view
//! you're in and the keys that work everywhere. A short terminal keeps
//! the one-row header instead — five rows are worth more to the panes
//! there.

use super::*;
use crate::ui::chrome::chrome;
use tui_common::chrome::{join_fields_to_fit, kv, pill, rounded_block};

/// Terminals at least this tall get the boxed header.
pub(crate) const TALL_HEADER_MIN_ROWS: u16 = 24;
/// The boxed header's height: three lines and two borders.
pub(crate) const TALL_HEADER_ROWS: u16 = 5;

/// How many rows the header takes on a terminal `height` rows tall.
pub(crate) fn header_rows(height: u16) -> u16 {
    if height >= TALL_HEADER_MIN_ROWS {
        TALL_HEADER_ROWS
    } else {
        1
    }
}

/// The view the operator is in, as the right-hand box names it. Pure.
pub(crate) fn view_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Normal | Mode::GridFilter | Mode::GridFind => "results",
        Mode::Editor | Mode::HistorySearch => "editor",
        Mode::Help | Mode::HelpFilter => "help",
        Mode::Confirm | Mode::TxDecision | Mode::ConfirmTerminate => "confirm",
        Mode::LogPick => "logs → SQL",
        Mode::ConnPick => "connections",
        Mode::RowDetail | Mode::CellDetail => "row",
        Mode::CellEdit => "edit",
        Mode::About => "about",
        Mode::ExplainTree => "explain",
        Mode::SchemaBrowser | Mode::SchemaBrowserFilter => "schema",
        Mode::SlowQueries => "slow queries",
        Mode::Sessions => "sessions",
        Mode::Environments => "environments",
        Mode::SchemaLint => "schema lint",
        Mode::ErrorDetail => "error",
        Mode::Notifications => "notifications",
        Mode::TapMonitor => "JDBC tap",
        Mode::SavedQueries | Mode::SavedQueriesFilter | Mode::RenameQueryPrompt => "saved queries",
        Mode::SaveQueryPrompt => "save query",
        Mode::ParamPrompt => "parameters",
        Mode::ResultDiff => "diff",
        Mode::CommandBar => "command",
    }
}

/// The connection state's words and colour.
fn state(app: &App) -> (String, Color) {
    let theme = &app.theme;
    match &app.conn_state {
        ConnState::Disconnected => ("disconnected".into(), theme.muted),
        ConnState::Connecting => (
            format!("{} connecting", SPINNER[app.anim_tick % SPINNER.len()]),
            theme.health_yellow,
        ),
        ConnState::Connected { server_version } => (
            format!("connected · pg {}", short_server_version(server_version)),
            theme.health_green,
        ),
        ConnState::Failed(_) => ("connection failed".into(), theme.health_red),
    }
}

/// Columns the header's `Server` value gets: what's left of `width`
/// after its label and the fields after it (`rest`), but never under
/// [`MIN_SERVER`] — past that, later fields drop instead — nor more
/// than the line holds. Pure.
fn server_budget(width: usize, label: usize, rest: usize) -> usize {
    const MIN_SERVER: usize = 16;
    let line = width.saturating_sub(label);
    line.saturating_sub(rest).max(MIN_SERVER).min(line)
}

/// `host:port` cut to `budget` columns: whole leading DNS labels, then
/// `…` and the port (`ledger-prod.c9abc1…:5432`), so what's kept still
/// names the server. Without room for the port, the labels alone
/// (`ledger-prod…`); with no label short enough, an end cut. Pure.
pub(crate) fn fit_server(server: &str, budget: usize) -> String {
    if display_width(server) <= budget {
        return server.to_string();
    }
    let (host, port) = server.rsplit_once(':').unwrap_or((server, ""));
    let labels: Vec<&str> = host.split('.').collect();
    let mut tails = vec!["…".to_string()];
    if !port.is_empty() {
        tails.insert(0, format!("…:{port}"));
    }
    tails
        .iter()
        .flat_map(|tail| {
            let labels = &labels;
            (1..labels.len())
                .rev()
                .map(move |k| format!("{}{tail}", labels[..k].join(".")))
        })
        .find(|s| display_width(s) <= budget)
        .unwrap_or_else(|| crate::grid::truncate_cell(server, budget))
}

pub(crate) fn draw_tall_header(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let c = chrome(theme);
    let cols =
        Layout::horizontal([Constraint::Percentage(64), Constraint::Percentage(36)]).split(area);

    // Left: the build in the border, then three lines of facts.
    let build = header_build(cols[0].width.saturating_sub(4) as usize, 0);
    let left = rounded_block(&c, false)
        .padding(ratatui::widgets::Padding::horizontal(1))
        .title(Line::from(vec![
            Span::styled(
                " pgman",
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{build} "), Style::default().fg(theme.muted)),
        ]));
    let inner = left.inner(cols[0]);
    f.render_widget(left, cols[0]);
    let width = inner.width;
    let dash = "—".to_string();
    let (server, database, user) = match &app.dsn {
        Some(d) => (
            format!("{}:{}", d.host, d.port),
            d.dbname.clone(),
            d.user.clone().unwrap_or_else(|| dash.clone()),
        ),
        None => (dash.clone(), dash.clone(), dash.clone()),
    };
    // A long RDS endpoint is cut (at a dot) so Database and User keep
    // their place, rather than the whole line dropping them.
    let rest: usize = [kv("Database", &database, &c), kv("User", &user, &c)]
        .iter()
        .map(|g| g.iter().map(Span::width).sum::<usize>() + tui_common::chrome::SEP_WIDTH)
        .sum();
    let server = fit_server(
        &server,
        server_budget(width as usize, "Server: ".len(), rest),
    );
    let line1 = join_fields_to_fit(
        vec![
            kv("Server", &server, &c),
            kv("Database", &database, &c),
            kv("User", &user, &c),
        ],
        &c,
        width,
    );
    let (state_text, state_colour) = state(app);
    let mut status = vec![
        Span::styled("Status: ", Style::default().fg(theme.muted)),
        Span::styled(
            state_text,
            Style::default()
                .fg(state_colour)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if crate::ui::shows_replica_badge(app) {
        status.push(Span::raw(" "));
        status.push(pill("replica", theme.row_alt_bg, theme.health_yellow));
    }
    if app.tx_open || app.mode == Mode::TxDecision {
        status.push(Span::raw(" "));
        status.push(pill("TX", theme.row_alt_bg, theme.health_yellow));
    }
    let session = if app.read_only {
        vec![
            Span::styled("Session: ", Style::default().fg(theme.muted)),
            pill("read-only", theme.row_alt_bg, theme.health_green),
        ]
    } else {
        kv("Session", "writable", &c)
    };
    let mut groups = vec![
        status,
        session,
        kv("Tabs", &app.tabs.len().max(1).to_string(), &c),
    ];
    if let Some(update) = &app.update_available {
        groups.push(vec![Span::styled(
            format!("⬆ {} available", update.version),
            Style::default().fg(theme.accent),
        )]);
    }
    let line2 = join_fields_to_fit(groups, &c, width);
    let via = app.dsn_origin.as_deref().unwrap_or("—");
    let line3 = vec![
        Span::styled("Via: ", Style::default().fg(theme.muted)),
        Span::styled(
            crate::grid::truncate_cell(via, (width as usize).saturating_sub(5)),
            Style::default().fg(theme.text),
        ),
    ];
    f.render_widget(
        Paragraph::new(vec![
            Line::from(line1),
            Line::from(line2),
            Line::from(line3),
        ]),
        inner,
    );

    // Right: the view, and the keys that work from anywhere.
    let right = rounded_block(&c, false).padding(ratatui::widgets::Padding::horizontal(1));
    let inner = right.inner(cols[1]);
    f.render_widget(right, cols[1]);
    let key = |k: &str| {
        Span::styled(
            format!("<{k}>"),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
    };
    let label = |t: &str| Span::styled(format!(" {t}"), Style::default().fg(theme.muted));
    let gap = || Span::raw("  ");
    let lines = vec![
        Line::from(vec![
            Span::styled("PostgreSQL   ", Style::default().fg(theme.muted)),
            Span::styled(
                view_label(app.mode),
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            key("?"),
            label("help"),
            gap(),
            key(":"),
            label("command"),
        ]),
        Line::from(vec![
            key("E"),
            label("envs"),
            gap(),
            key("ctrl-t"),
            label("tab"),
        ]),
    ];
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Right), inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_boxed_header_is_for_tall_terminals_only() {
        assert_eq!(header_rows(24), TALL_HEADER_ROWS);
        assert_eq!(header_rows(23), 1);
        assert_eq!(header_rows(16), 1);
    }

    #[test]
    fn fit_server_keeps_whole_labels_and_the_port() {
        let rds = "ledger-prod.c9abc1def2gh.eu-west-2.rds.amazonaws.com:5432";
        assert_eq!(fit_server("localhost:5433", 20), "localhost:5433");
        assert_eq!(fit_server(rds, 30), "ledger-prod.c9abc1def2gh…:5432");
        assert_eq!(fit_server(rds, 20), "ledger-prod…:5432");
        // No room for the port: the name alone, not `ledger-prod.c9…`.
        assert_eq!(fit_server(rds, 16), "ledger-prod…");
        assert_eq!(fit_server("db.example.com", 8), "db…");
        let cut = fit_server(rds, 10);
        assert_eq!(display_width(&cut), 10, "{cut}");
        assert!(cut.starts_with("ledger-pr"), "{cut}");
    }

    #[test]
    fn server_budget_leaves_room_for_the_later_fields() {
        // 47 columns, "Server: " 8, Database + User 25 → 14, floored at 16.
        assert_eq!(server_budget(47, 8, 25), 16);
        assert_eq!(server_budget(100, 8, 25), 67);
        // Never wider than the line itself.
        assert_eq!(server_budget(20, 8, 25), 12);
    }

    #[test]
    fn every_mode_names_its_view() {
        assert_eq!(view_label(Mode::Editor), "editor");
        assert_eq!(view_label(Mode::Environments), "environments");
        assert_eq!(view_label(Mode::Normal), "results");
    }
}
