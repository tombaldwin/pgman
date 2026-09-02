use super::*;

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
        // run the same command twice. `\timing` is the exception:
        // operators often toggle it back off in the same buffer.
        let clear_buffer = !matches!(cmd, BackslashCmd::Timing(_) | BackslashCmd::Expanded(_));
        if clear_buffer {
            self.editor.buffer.clear();
            self.editor.cursor = 0;
            self.draft_dirty = true;
        }
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
            BackslashCmd::Unknown(raw) => {
                self.last_error = Some(format!("unknown backslash command: {raw}"));
            }
        }
    }

    /// `\l` handler. Renders `App.databases` — every database on the
    /// server + its on-disk size, already fetched by the bootstrap
    /// query at connect time — as a result grid. Sends no query of its
    /// own.
    fn dispatch_list_databases(&mut self) {
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
        self.last_status = Some(if self.databases.is_empty() {
            "\\l → no databases (connect first)".into()
        } else {
            format!("\\l → {} database(s)", self.databases.len())
        });
    }

    /// `\c` / `\c <name>` handler.
    ///
    /// No argument: open the connection picker (same as the `c` key).
    /// `<name>` matching a picker entry: reconnect to it, through the
    /// exact same path the picker's Enter key uses
    /// (`App::connect_to_pick`). `<name>` matching nothing: swap
    /// `dbname` on the CURRENT dsn and reconnect through that same
    /// path — `Dsn::dbname` is a plain `String` field, so the swap is
    /// trivial and doesn't require reconstructing host/user/etc. With
    /// no active connection to swap, that's an actionable error.
    fn dispatch_connect(&mut self, target: Option<String>) {
        let Some(name) = target else {
            self.start_connection_change();
            return;
        };
        if let Some(pick) = self
            .conn_pick
            .picks
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(&name))
            .cloned()
        {
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
