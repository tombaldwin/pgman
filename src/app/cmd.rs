use super::*;

/// Every command name the `:` bar knows, in the order `:help`
/// lists them and Tab completes them. Aliases that exist only for
/// psql muscle memory (`:q`) are deliberately absent — Tab on `:q`
/// completes to `:quit`, which is the same command spelled in full.
pub const COMMAND_NAMES: &[&str] = &[
    "about", "connect", "d", "dn", "dt", "fixture", "help", "i", "l", "quit", "readonly", "report",
    "timing", "update", "x",
];

/// Help topics `:help <topic>` accepts, paired with the mode whose
/// help anchor they scroll to. Reuses `App::help_anchor_for` rather
/// than naming heading strings twice — a renamed heading can't leave
/// this table pointing at nothing.
///
/// The mode here is *only* an anchor. It is not where the operator
/// was, and closing help must not send them there — `:help commands`
/// used to land them in `Mode::CommandBar` with no command bar behind
/// it. See `App::open_help_anchored`.
const HELP_TOPICS: &[(&str, Mode)] = &[
    ("grid", Mode::Normal),
    ("editor", Mode::Editor),
    ("commands", Mode::CommandBar),
    ("schema", Mode::SchemaBrowser),
    ("saved", Mode::SavedQueries),
    ("slow", Mode::SlowQueries),
    ("sessions", Mode::Sessions),
    ("tap", Mode::TapMonitor),
    ("explain", Mode::ExplainTree),
    ("diff", Mode::ResultDiff),
    ("wizard", Mode::SchemaLint),
];

/// The `(topic, anchor mode)` table `:help <topic>` dispatches on.
/// Exposed so a test can walk every topic rather than a sample.
#[cfg(test)]
pub(crate) fn help_topics() -> &'static [(&'static str, Mode)] {
    HELP_TOPICS
}

/// Candidate command names for `typed` — every entry of
/// [`COMMAND_NAMES`] it is a prefix of. Pure; drives Tab completion
/// in the command bar.
pub fn command_candidates(typed: &str) -> Vec<&'static str> {
    COMMAND_NAMES
        .iter()
        .copied()
        .filter(|c| c.starts_with(typed))
        .collect()
}

/// Longest common prefix of `items`. `""` for an empty slice — the
/// caller treats that as "nothing to complete".
pub fn longest_common_prefix(items: &[&str]) -> String {
    let Some(first) = items.first() else {
        return String::new();
    };
    let mut end = first.len();
    for other in &items[1..] {
        end = end.min(
            first
                .char_indices()
                .zip(other.char_indices())
                .take_while(|((_, a), (_, b))| a == b)
                .last()
                .map(|((i, c), _)| i + c.len_utf8())
                .unwrap_or(0),
        );
    }
    first[..end].to_string()
}

impl App {
    /// User requested a run. Classify, evaluate safety, and either run, prompt,
    /// or reject. Multi-statement buffers (e.g. DBUnit scripts) take the batch
    /// path.
    /// Route a parsed backslash command to the corresponding
    /// interactive action. Called from `request_run` ahead of the
    /// regular safety / spawn path. After dispatch, the editor
    /// buffer is cleared so the next Run press doesn't re-fire
    /// the same command (psql's behaviour too).
    pub(super) fn dispatch_backslash(&mut self, cmd: crate::query::backslash::BackslashCmd) {
        use crate::query::backslash::BackslashCmd;
        // Clear the buffer immediately so a second F5 doesn't
        // run the same command twice. `\timing` / `\x` are exceptions:
        // operators often toggle them back off in the same buffer.
        // `Invalid` is too — its message says how to fix what the
        // operator typed, which is no help once the text is gone.
        let clear_buffer = !matches!(
            cmd,
            BackslashCmd::Timing(_) | BackslashCmd::Expanded(_) | BackslashCmd::Invalid(_)
        );
        if clear_buffer {
            self.editor.buffer.clear();
            self.editor.cursor = 0;
            self.draft_dirty = true;
        }
        self.apply_backslash(cmd);
    }

    /// The action half of [`App::dispatch_backslash`], without the
    /// editor-buffer clear. The `:` command bar dispatches through
    /// here: the command came from the bar, not from the buffer, so
    /// clearing the buffer would throw away the operator's draft SQL.
    pub(super) fn apply_backslash(&mut self, cmd: crate::query::backslash::BackslashCmd) {
        use crate::query::backslash::BackslashCmd;
        match cmd {
            BackslashCmd::Describe(target) => {
                if self.schema_cache.is_empty() {
                    self.last_status =
                        Some("schema cache empty — connect to a database first".into());
                    return;
                }
                // `\d <name>` → open browser with the name as
                // filter; the schema/table/column whose name
                // matches surfaces with its ancestors visible.
                // `\d` alone → open with no filter (default view).
                self.schema_browser.filter = target.clone();
                self.schema_browser.cursor = 0;
                self.mode = Mode::SchemaBrowser;
                self.last_status = Some(match target {
                    Some(t) => format!("\\d {t} → schema browser filtered to '{t}'"),
                    None => "\\d → schema browser".into(),
                });
            }
            BackslashCmd::ListTables | BackslashCmd::ListSchemas => {
                if self.schema_cache.is_empty() {
                    self.last_status =
                        Some("schema cache empty — connect to a database first".into());
                    return;
                }
                self.schema_browser.filter = None;
                self.schema_browser.cursor = 0;
                self.mode = Mode::SchemaBrowser;
                self.last_status = Some("schema browser".into());
            }
            BackslashCmd::Help => self.open_help_from(Mode::Editor),
            BackslashCmd::Quit => self.should_quit = true,
            BackslashCmd::Timing(target) => {
                // Toggle if no explicit value supplied.
                let new = target.unwrap_or(!self.timing_on);
                self.timing_on = new;
                self.last_status = Some(format!("\\timing {}", if new { "on" } else { "off" }));
            }
            BackslashCmd::Report(target) => self.dispatch_report(target),
            BackslashCmd::Fixture(target) => self.dispatch_fixture(target),
            BackslashCmd::ListDatabases => self.dispatch_list_databases(),
            BackslashCmd::Expanded(target) => {
                // Toggle if no explicit value supplied — same shape as
                // `\timing`.
                let new = target.unwrap_or(!self.expanded_on);
                self.expanded_on = new;
                self.last_status = Some(format!("expanded {}", if new { "on" } else { "off" }));
            }
            BackslashCmd::Connect(target) => self.dispatch_connect(target),
            BackslashCmd::Include(target) => self.dispatch_include(target),
            BackslashCmd::Invalid(message) => {
                self.last_error = Some(message);
            }
            BackslashCmd::Unknown(raw) => {
                self.last_error = Some(format!("unknown backslash command: {raw}"));
            }
        }
    }

    /// Open the `:` command bar over the current mode. The mode we
    /// came from is remembered so Esc (and any command that doesn't
    /// change the mode itself) puts the operator back where they were.
    ///
    /// Opening the bar over the guarded-run confirmation *cancels*
    /// that run. `:` is not one of the modal's keys, so the statement
    /// was never confirmed — but `pending_run` used to survive
    /// anyway, and it is a `\watch` blocker
    /// (`watch_should_fire`): F5 on a DELETE, `:about`, Esc, and
    /// `\watch` then refused to fire for the rest of the session with
    /// no visible modal to explain why. Cancelling matches the `n` /
    /// Esc path, down to the status text and the return to the
    /// editor.
    pub(super) fn open_command_bar(&mut self) {
        let mut origin = self.mode;
        let cancelled_run = origin == Mode::Confirm;
        if cancelled_run {
            self.pending_run = None;
            self.last_status = Some("cancelled".to_string());
            // Esc out of the bar must not land back on a modal with
            // nothing behind it — `draw_confirm` renders nothing
            // without a `pending_run`, and its y/n would no-op.
            origin = Mode::Editor;
        }
        if origin == Mode::Help {
            // The bar opened over the help overlay. Its origin is
            // where the operator was BEFORE help, not `Help` itself:
            // `:help` from here re-opened help with `return_to =
            // Help`, and Esc then closed help into help — no way back
            // to the editor.
            origin = self.help.return_to.take().unwrap_or(Mode::Normal);
            self.help.origin = None;
            self.help.scroll = 0;
        }
        self.command_bar = Some(CommandBarUi {
            input: TextInput::new(),
            origin,
            cancelled_run,
        });
        self.mode = Mode::CommandBar;
    }

    /// Dispatch one command-bar line (without the leading `:`).
    ///
    /// `about` / `update` / `help` / `readonly` are the bar's own;
    /// everything else is handed to the SAME parser the editor's
    /// backslash commands use, with the `:` swapped for a `\`, so
    /// `:dt` and `\dt` can't drift apart. `connect` is spelled `c`
    /// for that parser. An empty line is a no-op (the operator
    /// pressed Enter on an empty bar).
    pub(super) fn dispatch_command(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let (name, rest) = match line.split_once(char::is_whitespace) {
            Some((n, r)) => (n, r.trim()),
            None => (line, ""),
        };
        let arg = (!rest.is_empty()).then(|| rest.to_string());
        match name.to_ascii_lowercase().as_str() {
            "about" => self.mode = Mode::About,
            "update" => self.show_update_status(),
            "help" => self.dispatch_help_topic(arg),
            "readonly" => self.dispatch_read_only(arg),
            other => {
                // Rebuild from the LOWERCASED name, not the raw line:
                // the four commands handled above matched on the
                // lowercased name, so `:ABOUT` worked, while
                // everything delegated here was handed `\DT` — which
                // `parse_backslash_command` doesn't recognise, so
                // `:DT` answered "unknown command". The argument keeps
                // its case (paths and data-source names are
                // case-sensitive); only the name is folded.
                //
                // `connect` is the bar's spelling of psql's `\c`.
                let name = if other == "connect" { "c" } else { other };
                let body = if rest.is_empty() {
                    name.to_string()
                } else {
                    format!("{name} {rest}")
                };
                match crate::query::backslash::parse_backslash_command(&format!("\\{body}")) {
                    Some(crate::query::backslash::BackslashCmd::Unknown(_)) | None => {
                        self.last_error =
                            Some(format!("unknown command :{name} · :help lists them"));
                    }
                    Some(cmd) => self.apply_backslash(cmd),
                }
            }
        }
    }

    /// `:update` — open the About card (which carries the install
    /// channel and the upgrade command) and say in the footer where
    /// the check actually got to. "Up to date" is only claimed once
    /// a check has landed; a run with the check disabled says so
    /// rather than implying a clean result.
    fn show_update_status(&mut self) {
        self.mode = Mode::About;
        self.last_status = Some(match (&self.update_available, self.update_check_done) {
            (Some(update), _) => format!(
                "update available: {} — {}",
                update.version,
                crate::update_check::detect_install_channel().upgrade_command()
            ),
            (None, true) => format!("up to date · {}", env!("CARGO_PKG_VERSION")),
            (None, false) if !self.update_check_enabled => {
                "update check is off for this run — see the About card for the install channel"
                    .into()
            }
            (None, false) => "update check hasn't answered yet".into(),
        });
    }

    /// `:help` / `:help <topic>` — open the help overlay, scrolled to
    /// the section for `<topic>`. Topics map onto the modes whose
    /// anchors the overlay already knows (`App::help_anchor_for`), so
    /// there is exactly one list of section names in the codebase.
    fn dispatch_help_topic(&mut self, topic: Option<String>) {
        let Some(topic) = topic else {
            self.open_help_from(self.mode);
            return;
        };
        let wanted = topic.to_ascii_lowercase();
        match HELP_TOPICS
            .iter()
            .find(|(name, _)| *name == wanted)
            .map(|(_, mode)| *mode)
        {
            // Anchor on the topic; come back to where the bar was
            // opened from (`on_command_bar_key` has already restored
            // `self.mode` to the bar's origin by this point).
            Some(anchor) => self.open_help_anchored(anchor, self.mode),
            None => {
                let names: Vec<&str> = HELP_TOPICS.iter().map(|(n, _)| *n).collect();
                self.last_error = Some(format!(
                    "unknown help topic '{topic}' · try: {}",
                    names.join(", ")
                ));
            }
        }
    }

    /// `:readonly on|off` — set the read-only flag pgman opens
    /// connections with, through the same profile lookup
    /// `App::connect_to_pick` uses.
    ///
    /// Turning it OFF is refused when `safety.toml` pins the current
    /// database read-only: that file is the authority, and a session
    /// cannot vote itself out of it (same refusal as a `SET
    /// default_transaction_read_only = off` statement).
    ///
    /// The flag is applied at connect (`SET
    /// default_transaction_read_only`), so a change while a
    /// connection is live takes effect on the next connect — the
    /// status line says so rather than implying the running session
    /// just changed under the operator.
    fn dispatch_read_only(&mut self, arg: Option<String>) {
        let want = match arg.as_deref().map(str::to_ascii_lowercase).as_deref() {
            Some("on") => true,
            Some("off") => false,
            _ => {
                self.last_error = Some("usage: :readonly on|off".into());
                return;
            }
        };
        let db = self
            .dsn
            .as_ref()
            .map(|d| d.dbname.as_str())
            .unwrap_or("default");
        if !want && self.safety_config.profile_for(db).read_only {
            self.last_error = Some(read_only_escape_refusal(safety_toml_exists()));
            return;
        }
        self.read_only = want;
        // Sticky: `connect_to_pick` recomputes `read_only` from the
        // picked database's profile, which used to discard this the
        // moment the operator reconnected.
        self.read_only_override = Some(want);
        let state = if want { "on" } else { "off" };
        self.last_status = Some(if matches!(self.conn_state, ConnState::Connected { .. }) {
            format!("read-only {state} · applies at the next connect (:connect); this session keeps what it opened with")
        } else {
            format!("read-only {state} · the next connection opens with it {state}")
        });
    }

    /// `\l` handler. Renders `App.databases` — every database on the
    /// server + its on-disk size, already fetched by the bootstrap
    /// query at connect time — as a result grid. Sends no query of its
    /// own.
    fn dispatch_list_databases(&mut self) {
        if self.databases.is_empty() {
            // Nothing to show. Replacing the grid with a header-only
            // one would swap the start card ("nothing has run yet") for
            // a permanent empty two-column result — `Grid.columns`
            // being non-empty is exactly what tells those apart, and
            // nothing puts the card back. Say it in the status line and
            // leave the body alone.
            self.last_status = Some("\\l → no databases (connect first)".into());
            return;
        }
        self.grid = Grid {
            columns: vec!["database".into(), "size".into()],
            rows: self
                .databases
                .iter()
                .map(|d| vec![d.name.clone(), d.size.clone()])
                .collect(),
            truncated: false,
        };
        self.grid_state
            .select(if self.grid.is_empty() { None } else { Some(0) });
        self.reset_grid_view();
        self.last_status = Some(format!("\\l → {} database(s)", self.databases.len()));
    }

    /// `\c` / `\c <name>` handler.
    ///
    /// No argument: open the connection picker (same as the `c` key).
    /// `<name>` resolving to a picker entry (`match_pick_name`: exact,
    /// else a unique case-insensitive prefix — several matches are
    /// listed rather than guessed): reconnect to it, through the exact
    /// same path the picker's Enter key uses (`App::connect_to_pick`).
    /// A name containing spaces — which is most discovered ones,
    /// `dataSource (application)` — is given double-quoted.
    /// `<name>` matching nothing: swap
    /// `dbname` on the CURRENT dsn and reconnect through that same
    /// path — `Dsn::dbname` is a plain `String` field, so the swap is
    /// trivial and doesn't require reconstructing host/user/etc. With
    /// no active connection to swap, that's an actionable error.
    fn dispatch_connect(&mut self, target: Option<String>) {
        let Some(name) = target else {
            self.start_connection_change();
            return;
        };
        use crate::query::backslash::{match_pick_name, NameMatch};
        let names: Vec<&str> = self
            .conn_pick
            .picks
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        let resolved = match match_pick_name(&name, &names) {
            NameMatch::One(i) => Some(i),
            NameMatch::Ambiguous(hits) => {
                // Picking one of them would be picking which database
                // the operator connects to. List them instead, and say
                // how to name one exactly.
                let listed: Vec<String> =
                    hits.iter().map(|&i| format!("\"{}\"", names[i])).collect();
                self.last_error = Some(format!(
                    "connect '{name}' is ambiguous — {} data sources start with it: {} · quote the full name",
                    hits.len(),
                    listed.join(", ")
                ));
                return;
            }
            NameMatch::None => None,
        };
        if let Some(pick) = resolved.map(|i| self.conn_pick.picks[i].clone()) {
            if self.refuse_if_unresolved(&pick) {
                return;
            }
            // `refuse_if_unresolved` already rejected a pick with no
            // DSN, so this is always `Some`.
            let Some(dsn) = pick.dsn.clone() else {
                return;
            };
            let origin = format!("picked {} data source '{}'", pick.origin, pick.name);
            // Same tunnel confirmation as the picker's Enter — naming a
            // discovered pick is not the same as authorising an `ssh`
            // session to the bastion it carries.
            self.connect_to_discovered_pick(dsn, origin);
            return;
        }
        let Some(mut dsn) = self.dsn.clone() else {
            self.last_error = Some(format!(
                "\\c {name} — no data source named '{name}' and no active connection to swap the database on"
            ));
            return;
        };
        dsn.dbname = name.clone();
        self.connect_to_pick(dsn, format!("\\c switched database to '{name}'"));
    }

    /// `\i <path>` handler. Reads the whole file and replaces the
    /// editor buffer with it — the operator reviews before running,
    /// same as pasting it in by hand. Never runs anything itself.
    fn dispatch_include(&mut self, target: Option<String>) {
        let Some(path) = target else {
            self.last_error = Some("\\i requires a file path".into());
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(contents) => {
                let n = contents.lines().count();
                self.editor.buffer = contents;
                self.editor.cursor = self.editor.buffer.len();
                self.editor.preferred_col = None;
                self.history_pos = None;
                self.draft_dirty = true;
                self.last_status = Some(format!("loaded {n} lines from {path}"));
            }
            Err(e) => {
                self.last_error = Some(format!("\\i {path} failed: {e}"));
            }
        }
    }

    /// `\report` / `\report <path>` handler. Snapshots current
    /// App state, renders as Markdown or HTML per the path
    /// extension, and writes atomically. Default path lives
    /// under the cache dir with a wall-clock-stamped filename.
    fn dispatch_report(&mut self, target: Option<String>) {
        let path = match target {
            Some(p) if !p.trim().is_empty() => std::path::PathBuf::from(p),
            _ => default_report_path(),
        };
        let snapshot = self.report_snapshot();
        let body = match crate::report::format_for_path(&path) {
            crate::report::ReportFormat::Markdown => crate::report::render_markdown(&snapshot),
            crate::report::ReportFormat::Html => crate::report::render_html(&snapshot),
        };
        let ok = format!("wrote report to {}", path.display());
        self.write_export(&path, &body, "\\report", ok);
    }

    /// Shared write path for `\report` / `\fixture`: write atomically
    /// and owner-only (`crate::util::write_private`, which also
    /// creates the parent directory if needed), and set the status
    /// (on success, `ok_status`) or error line. `cmd` names the
    /// backslash command for the error message.
    fn write_export(&mut self, path: &std::path::Path, body: &str, cmd: &str, ok_status: String) {
        match crate::util::write_private(path, body) {
            Ok(()) => self.last_status = Some(ok_status),
            Err(e) => {
                self.last_error = Some(format!("{cmd} failed: {} ({e})", path.display()));
            }
        }
    }

    /// `\fixture` / `\fixture <path>` handler. Captures the
    /// current result grid as a DBUnit FlatXmlDataSet — the
    /// reverse of the apply script. Requires a non-empty,
    /// single-table result (the source table is the element
    /// name). Writes atomically; default path lives under the
    /// cache dir with a wall-clock-stamped filename.
    pub(super) fn dispatch_fixture(&mut self, target: Option<String>) {
        if self.grid.rows.is_empty() {
            self.last_error = Some("no result to capture — run a query first".into());
            return;
        }
        let Some((_schema, table)) = self.grid_view.source.clone() else {
            self.last_error = Some(
                "fixture capture needs a single-table result (no source table inferred)".into(),
            );
            return;
        };
        let fixture = crate::dbunit::fixture_from_rows(&table, &self.grid.columns, &self.grid.rows);
        let xml = crate::dbunit::generate_flat_xml(&fixture);
        let path = match target {
            Some(p) if !p.trim().is_empty() => std::path::PathBuf::from(p),
            _ => default_fixture_path(&table),
        };
        let ok = format!("wrote {} row(s) to {}", fixture.rows.len(), path.display());
        self.write_export(&path, &xml, "\\fixture", ok);
    }
}
