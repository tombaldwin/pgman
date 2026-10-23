//! `u`: edit the focused grid cell as a one-row `UPDATE`
//! (docs/backlog/cell-editing.md). Which row and column a cell is, and
//! the statement, are planned purely in `query::cell_edit`; this is the
//! App's half — the refusals that need session state, the editor's
//! keys, the route through `safety.rs`, the spawn, and putting the
//! server's answer back in the grid.

use super::*;
use crate::query::cell_edit;

/// `u` on a read-only session, refused before anything is typed, with
/// the same advice `:readonly off` gives for the reason it is
/// read-only: a replica, the safety profile, or `:readonly on`. Pure.
pub(crate) fn read_only_edit_refusal(
    replica_pin: bool,
    profile_read_only: bool,
    cfg: &SafetyConfig,
    db: &str,
    safety_toml_exists: bool,
) -> String {
    let why = if replica_pin {
        cmd::REPLICA_READONLY_REFUSAL.to_string()
    } else if profile_read_only {
        readonly_off_refusal(cfg, db, safety_toml_exists)
    } else {
        "read-only (:readonly on) · :readonly off to write".to_string()
    };
    format!("can't edit: {why}")
}

/// Whether `sql` is exactly one statement as `safety.rs` splits it — the
/// check an editor statement passes before it is classified. A cell
/// edit's UPDATE is one statement by construction; checking anyway
/// means a quoting bug can't reach the server unclassified. Pure.
pub(crate) fn is_one_statement(sql: &str) -> bool {
    matches!(safety::split_verified(sql), Ok(s) if s.len() == 1)
}

/// `public.users.name where id = 3`: what an edit changes, for the
/// confirm modal and the status line. Pure.
pub(crate) fn edit_target(edit: &cell_edit::CellEdit) -> String {
    format!(
        "{}.{}.{} where {}",
        edit.schema,
        edit.table,
        edit.column,
        cell_edit::key_summary(edit)
    )
}

/// Put `value` in column `col` of the row that read `before`, trying
/// `hint` first: the row may have moved (a sort) since the edit was
/// planned. `false` when no row reads `before` any more. A primary key
/// in the row makes a match unique. Pure.
pub(crate) fn set_edited_cell(
    rows: &mut [Vec<Cell>],
    hint: usize,
    before: &[Cell],
    col: usize,
    value: Cell,
) -> bool {
    let idx = if rows.get(hint).is_some_and(|r| r.as_slice() == before) {
        Some(hint)
    } else {
        rows.iter().position(|r| r.as_slice() == before)
    };
    match idx.and_then(|i| rows[i].get_mut(col)) {
        Some(cell) => {
            *cell = value;
            true
        }
        None => false,
    }
}

impl App {
    /// `u` in the grid (`return_to` Normal: the cursor's cell) or row
    /// detail (`RowDetail`: the focused field). Opens the editor, or
    /// says why this cell can't be edited.
    pub(super) fn start_cell_edit(&mut self, return_to: Mode) {
        let col = if return_to == Mode::RowDetail {
            self.row_detail.field
        } else {
            self.grid_view.col_cursor
        };
        let Some(row) = self
            .selected_grid_row_idx()
            .filter(|&i| i < self.grid.rows.len())
        else {
            self.last_status = Some("no cell to edit · run a query first".into());
            return;
        };
        if self.query_running {
            self.last_status = Some("a query is already running · ctrl-c cancels it".into());
            return;
        }
        if let Some(refusal) = self.cell_edit_session_refusal() {
            self.last_error = Some(refusal);
            return;
        }
        let row_cells = self.grid.rows[row].clone();
        match cell_edit::plan_cell_edit(
            &self.schema_cache,
            self.grid_view.source.as_ref(),
            &self.grid.columns,
            &self.grid_view.origins,
            &row_cells,
            row,
            col,
        ) {
            Err(refusal) => self.last_error = Some(refusal.to_string()),
            Ok(edit) => {
                let input = TextInput::with_text(edit.old.clone().unwrap_or_default());
                self.cell_edit = Some(CellEditUi {
                    null: edit.old.is_none(),
                    edit,
                    input,
                    row_cells,
                    return_to,
                });
                self.last_error = None;
                self.last_status = None;
                self.mode = Mode::CellEdit;
            }
        }
    }

    /// Why this session can't edit at all, whatever the cell: no
    /// connection, a transaction awaiting its prompt, read-only.
    fn cell_edit_session_refusal(&self) -> Option<String> {
        if self.client.is_none() {
            return Some(if self.demo {
                "can't edit: --demo has no database to write to".into()
            } else {
                self.not_connected_message()
            });
        }
        if self.tx_open {
            return Some(
                "a transaction is open — commit (y) or roll back (n) first, then edit".into(),
            );
        }
        if self.read_only {
            let db = self
                .dsn
                .as_ref()
                .map(|d| d.dbname.as_str())
                .unwrap_or("default");
            return Some(read_only_edit_refusal(
                self.replica_pin,
                self.safety_config.profile_for(db).read_only,
                &self.safety_config,
                db,
                safety_toml_exists(),
            ));
        }
        None
    }

    /// `Mode::CellEdit`: Enter saves, Esc cancels, `Ctrl-N` toggles
    /// NULL; everything else edits the value (`TextInput`). Typing a
    /// character into a NULL starts a text value.
    pub(super) fn on_cell_edit_key(&mut self, key: KeyEvent) {
        let Some(ui) = self.cell_edit.as_mut() else {
            self.mode = Mode::Normal;
            return;
        };
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        match key.code {
            KeyCode::Esc => {
                self.mode = ui.return_to;
                self.cell_edit = None;
                self.last_status = Some("edit cancelled".into());
            }
            KeyCode::Enter => self.save_cell_edit(),
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // NULL → the empty string; anything → NULL.
                ui.null = !ui.null;
                if ui.null {
                    ui.input.clear();
                }
            }
            KeyCode::Char(_) if plain => {
                ui.null = false;
                ui.input.handle_key(key);
            }
            // Editing keys on a NULL have no text to act on.
            _ if !ui.null => {
                ui.input.handle_key(key);
            }
            _ => {}
        }
    }

    /// A bracketed paste while editing a cell: inserted at the cursor,
    /// unless it holds a newline or another control character, which a
    /// one-line value can't show faithfully. The one line break a copied
    /// line ends with is dropped, not counted.
    pub(super) fn paste_into_cell_edit(&mut self, text: &str) {
        let Some(ui) = self.cell_edit.as_mut() else {
            return;
        };
        let text = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
            .unwrap_or(text);
        if text.chars().any(char::is_control) {
            self.last_status = Some(
                "can't paste a newline or control character into a cell · write the UPDATE in the editor"
                    .into(),
            );
            return;
        }
        ui.null = false;
        ui.input.insert_str(text);
    }

    /// Enter in the editor: write the UPDATE and send it through the
    /// safety guard exactly as an editor statement — one verified
    /// statement, classified, then the database's guard: blocked,
    /// confirmed (the modal shows the statement) or run.
    fn save_cell_edit(&mut self) {
        let Some(ui) = self.cell_edit.take() else {
            return;
        };
        self.mode = ui.return_to;
        let new = ui.value();
        if new == ui.edit.old {
            self.last_status = Some("no change".into());
            return;
        }
        let sql = cell_edit::build_update_sql(&ui.edit, new.as_deref());
        // The statement is built, not typed, so it is one statement by
        // construction; checking costs nothing and a quoting bug would
        // otherwise reach the server unclassified.
        if !is_one_statement(&sql) {
            tracing::warn!("cell edit: the generated UPDATE did not split as one statement");
            self.last_error =
                Some("can't edit: the UPDATE didn't read as one statement · nothing sent".into());
            return;
        }
        let db = self
            .dsn
            .as_ref()
            .map(|d| d.dbname.clone())
            .unwrap_or_else(|| "default".into());
        let decision = safety::evaluate(&self.safety_config, &db, &sql);
        let summary = format!("edit {}", edit_target(&ui.edit));
        let pending = PendingCellEdit {
            edit: ui.edit,
            new,
            row_cells: ui.row_cells,
            return_to: ui.return_to,
        };
        match decision.guard {
            Guard::Block => self.last_error = Some(blocked_by_safety_message(&decision, &db)),
            Guard::Confirm => {
                self.pending_run = Some(PendingRun {
                    sql,
                    kind: RunKind::Run,
                    decision,
                    is_batch: false,
                    summary: Some(summary),
                    cell_edit: Some(pending),
                });
                self.mode = Mode::Confirm;
            }
            Guard::Allow => self.spawn_cell_update(pending, sql, decision),
        }
    }

    /// Run a cell edit's UPDATE under the one-row guard
    /// (`conn::run_single_row_update`); lands as `AppMsg::CellUpdated`,
    /// or `QueryFailed` with the reason nothing changed.
    pub(super) fn spawn_cell_update(
        &mut self,
        pending: PendingCellEdit,
        sql: String,
        decision: Decision,
    ) {
        // Through `db()`, which closes an open result cursor first: an
        // UPDATE sent inside the cursor's transaction would be undone
        // when the pager ends it.
        let Some(db) = self.db() else {
            self.last_error = Some(self.not_connected_message());
            return;
        };
        self.remember_in_history(&sql);
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        let keep_open = decision.wrap_in_tx;
        self.query_running = true;
        self.query_started = Some(Instant::now());
        self.last_error = None;
        self.last_status = Some("saving the edit…".into());
        tokio::spawn(async move {
            let client = db.ready().await;
            let result = conn::run_single_row_update(&client, &sql, &pending.edit, keep_open).await;
            let msg = match result {
                Ok(done) => AppMsg::CellUpdated {
                    generation,
                    pending,
                    value: done.value,
                    tx: done.tx,
                },
                Err(e) => AppMsg::QueryFailed {
                    generation,
                    error: e.msg,
                    position: None,
                    detail: e.detail,
                },
            };
            let _ = tx.send(msg);
        });
    }

    /// The UPDATE changed its row: show the value the server stored in
    /// the grid (and the unsorted copy behind sort), and say what
    /// happened — committed, waiting on the `auto_tx` prompt, or inside
    /// the operator's own transaction.
    pub(super) fn on_cell_updated(
        &mut self,
        mut pending: PendingCellEdit,
        value: Cell,
        tx: conn::EditTx,
    ) {
        self.query_running = false;
        self.query_started = None;
        self.last_error = None;
        self.last_error_detail = None;
        let (hint, col) = (pending.edit.row, pending.edit.col);
        let shown = set_edited_cell(
            &mut self.grid.rows,
            hint,
            &pending.row_cells,
            col,
            value.clone(),
        );
        if let Some(raw) = self.grid_view.raw_rows.as_mut() {
            set_edited_cell(raw, hint, &pending.row_cells, col, value.clone());
        }
        let target = edit_target(&pending.edit);
        // What the row holds now, should a rollback have to undo it.
        pending.new = value;
        let state = match tx {
            conn::EditTx::Committed => "committed",
            conn::EditTx::LeftOpen => {
                self.tx_open = true;
                self.mode = Mode::TxDecision;
                self.edit_awaiting_tx = Some(pending);
                "in a transaction · y commits, n rolls back"
            }
            conn::EditTx::InOperatorTx => "inside your open transaction · COMMIT to keep it",
        };
        let moved = if shown {
            ""
        } else {
            " · the grid changed meanwhile, re-run to see it"
        };
        self.last_status = Some(format!("updated {target} · {state}{moved}"));
    }

    /// An `auto_tx` edit was rolled back: the grid shows the old value
    /// again.
    pub(super) fn revert_cell_edit(&mut self, pending: &PendingCellEdit) {
        let col = pending.edit.col;
        let mut after = pending.row_cells.clone();
        if let Some(cell) = after.get_mut(col) {
            cell.clone_from(&pending.new);
        }
        let old = pending.edit.old.clone();
        set_edited_cell(
            &mut self.grid.rows,
            pending.edit.row,
            &after,
            col,
            old.clone(),
        );
        if let Some(raw) = self.grid_view.raw_rows.as_mut() {
            set_edited_cell(raw, pending.edit.row, &after, col, old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn cells(values: &[Option<&str>]) -> Vec<Cell> {
        values.iter().map(|v| v.map(str::to_string)).collect()
    }

    /// `public.users (id, name)`: three rows, the editor open on row 1's
    /// `name`, as `u` in the grid would leave it.
    fn editing(old: Option<&str>) -> App {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.grid = Grid {
            columns: vec!["id".into(), "name".into()],
            rows: vec![
                cells(&[Some("1"), Some("ann")]),
                cells(&[Some("2"), old]),
                cells(&[Some("3"), Some("cy")]),
            ],
            truncated: false,
        };
        a.reset_grid_view();
        let edit = cell_edit::CellEdit {
            schema: "public".into(),
            table: "users".into(),
            column: "name".into(),
            key: vec![("id".into(), "2".into())],
            old: old.map(str::to_string),
            row: 1,
            col: 1,
            table_oid: 100,
            attnum: 2,
            key_attnums: vec![1],
        };
        a.cell_edit = Some(CellEditUi {
            input: TextInput::with_text(old.unwrap_or_default()),
            null: old.is_none(),
            row_cells: a.grid.rows[1].clone(),
            edit,
            return_to: Mode::Normal,
        });
        a.mode = Mode::CellEdit;
        a
    }

    fn typed(a: &mut App, text: &str) {
        for c in text.chars() {
            a.on_key(key(KeyCode::Char(c)));
        }
    }

    fn value(a: &App) -> Cell {
        a.cell_edit.as_ref().expect("editor open").value()
    }

    #[test]
    fn ctrl_n_toggles_null_and_typing_ends_it() {
        let mut a = editing(Some("bo"));
        a.on_key(ctrl('n'));
        assert_eq!(value(&a), None);
        // A NULL has no text for Backspace to edit.
        a.on_key(key(KeyCode::Backspace));
        assert_eq!(value(&a), None);
        // Ctrl-N on a NULL: the empty string, a different value.
        a.on_key(ctrl('n'));
        assert_eq!(value(&a), Some(String::new()));
        a.on_key(ctrl('n'));
        typed(&mut a, "x");
        assert_eq!(value(&a), Some("x".into()));
        // `?` and `:` are characters here, not help or the command bar.
        typed(&mut a, "?:");
        assert_eq!(value(&a), Some("x?:".into()));
        assert_eq!(a.mode, Mode::CellEdit);
    }

    #[test]
    fn esc_cancels_back_where_the_edit_began() {
        let mut a = editing(Some("bo"));
        a.cell_edit.as_mut().unwrap().return_to = Mode::RowDetail;
        typed(&mut a, "zz");
        a.on_key(key(KeyCode::Esc));
        assert_eq!(a.mode, Mode::RowDetail);
        assert!(a.cell_edit.is_none());
        assert_eq!(a.grid.rows[1][1].as_deref(), Some("bo"));
    }

    #[test]
    fn saving_the_same_value_sends_nothing() {
        let mut a = editing(None);
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.last_status.as_deref(), Some("no change"));
        assert!(a.pending_run.is_none());
        assert_eq!(a.mode, Mode::Normal);
    }

    /// The default guard confirms an UPDATE: the modal holds the
    /// generated statement and the edit; `n` goes back to the grid, not
    /// the editor.
    #[test]
    fn a_save_goes_through_the_guard_and_the_confirm_shows_the_update() {
        let mut a = editing(None);
        a.on_key(ctrl('n'));
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Confirm);
        let pending = a.pending_run.as_ref().expect("confirm");
        assert_eq!(
            pending.sql,
            "UPDATE public.users SET name = '' WHERE id = '2' RETURNING name"
        );
        assert_eq!(
            pending.summary.as_deref(),
            Some("edit public.users.name where id = 2")
        );
        let edit = pending.cell_edit.as_ref().expect("an edit");
        assert_eq!(edit.new, Some(String::new()));
        assert_eq!(edit.row_cells, cells(&[Some("2"), None]));
        a.on_key(key(KeyCode::Char('n')));
        assert_eq!(a.mode, Mode::Normal);
        assert!(a.pending_run.is_none());
        assert_eq!(a.grid.rows[1][1], None);
    }

    #[test]
    fn a_blocked_update_is_refused_with_the_guards_message() {
        let mut a = editing(Some("bo"));
        a.safety_config.default.guards.update = Guard::Block;
        typed(&mut a, "!");
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Normal);
        assert!(a.pending_run.is_none());
        let err = a.last_error.as_deref().unwrap_or_default();
        assert!(err.starts_with("blocked by safety: UPDATE"), "{err}");
    }

    fn pending(a: &App, new: Option<&str>) -> PendingCellEdit {
        let ui = a.cell_edit.as_ref().expect("editor open");
        PendingCellEdit {
            edit: ui.edit.clone(),
            new: new.map(str::to_string),
            row_cells: ui.row_cells.clone(),
            return_to: ui.return_to,
        }
    }

    /// The server's value lands in the row it came from — found by its
    /// contents after a sort moved it — and in the unsorted copy.
    #[test]
    fn the_updated_value_lands_in_the_row_even_after_a_sort() {
        let mut a = editing(Some("bo"));
        let p = pending(&a, Some("BO"));
        a.mode = Mode::Normal;
        a.grid_view.raw_rows = Some(a.grid.rows.clone());
        a.grid.rows.reverse();
        a.query_running = true;
        a.on_msg(AppMsg::CellUpdated {
            generation: a.generation,
            pending: p,
            value: Some("BO".into()),
            tx: conn::EditTx::Committed,
        });
        assert_eq!(a.grid.rows[1], cells(&[Some("2"), Some("BO")]));
        assert_eq!(
            a.grid_view.raw_rows.as_ref().unwrap()[1],
            cells(&[Some("2"), Some("BO")])
        );
        assert!(!a.query_running);
        assert_eq!(
            a.last_status.as_deref(),
            Some("updated public.users.name where id = 2 · committed")
        );
    }

    #[test]
    fn a_row_gone_from_the_grid_says_so() {
        let mut a = editing(Some("bo"));
        let p = pending(&a, Some("BO"));
        a.grid.rows.remove(1);
        a.on_msg(AppMsg::CellUpdated {
            generation: a.generation,
            pending: p,
            value: Some("BO".into()),
            tx: conn::EditTx::InOperatorTx,
        });
        assert_eq!(
            a.last_status.as_deref(),
            Some(
                "updated public.users.name where id = 2 · inside your open transaction · \
                 COMMIT to keep it · the grid changed meanwhile, re-run to see it"
            )
        );
    }

    /// Under `auto_tx` the edit waits on the commit / rollback prompt; a
    /// rollback puts the old value back, a commit keeps the new, and
    /// either way the operator lands back on the grid.
    #[test]
    fn an_auto_tx_edit_waits_on_the_prompt_and_a_rollback_reverts_it() {
        for (committed, expect) in [(false, Some("bo")), (true, Some("BO"))] {
            let mut a = editing(Some("bo"));
            let p = pending(&a, Some("BO"));
            a.on_msg(AppMsg::CellUpdated {
                generation: a.generation,
                pending: p,
                value: Some("BO".into()),
                tx: conn::EditTx::LeftOpen,
            });
            assert_eq!(a.mode, Mode::TxDecision);
            assert!(a.tx_open);
            assert_eq!(a.grid.rows[1][1].as_deref(), Some("BO"));
            a.on_msg(AppMsg::TxClosed {
                generation: a.generation,
                committed,
                error: None,
            });
            assert_eq!(a.grid.rows[1][1].as_deref(), expect, "{committed}");
            assert_eq!(a.mode, Mode::Normal);
            assert!(a.edit_awaiting_tx.is_none());
        }
    }

    /// A reconnect, or a prompt answered with no connection, ends the
    /// `auto_tx` transaction without a commit: the grid takes the old
    /// value back.
    #[tokio::test]
    async fn an_auto_tx_edit_lost_to_a_reconnect_is_reverted() {
        let mut a = editing(Some("bo"));
        let p = pending(&a, Some("BO"));
        a.on_msg(AppMsg::CellUpdated {
            generation: a.generation,
            pending: p.clone(),
            value: Some("BO".into()),
            tx: conn::EditTx::LeftOpen,
        });
        a.on_key(key(KeyCode::Char('y')));
        assert_eq!(a.grid.rows[1][1].as_deref(), Some("bo"));
        assert_eq!(a.mode, Mode::Normal);
        assert!(a.edit_awaiting_tx.is_none());

        let mut a = editing(Some("bo"));
        a.on_msg(AppMsg::CellUpdated {
            generation: a.generation,
            pending: p,
            value: Some("BO".into()),
            tx: conn::EditTx::LeftOpen,
        });
        a.dsn = crate::conn::Dsn::parse("postgres://u@127.0.0.1:1/db").ok();
        a.start_connect();
        assert_eq!(a.grid.rows[1][1].as_deref(), Some("bo"));
        assert!(a.edit_awaiting_tx.is_none());
    }

    /// `:` over an edit's confirm cancels it back to the grid, like `n`.
    #[test]
    fn the_command_bar_over_an_edits_confirm_goes_back_to_the_grid() {
        let mut a = editing(Some("bo"));
        typed(&mut a, "!");
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Confirm);
        a.on_key(key(KeyCode::Char(':')));
        a.on_key(key(KeyCode::Esc));
        assert_eq!(a.mode, Mode::Normal);
        assert!(a.pending_run.is_none());
    }

    /// No connection (or `--demo`) refuses `u` before the editor opens.
    #[test]
    fn u_refuses_without_a_connection() {
        let mut a = editing(Some("bo"));
        a.cell_edit = None;
        a.mode = Mode::Normal;
        a.grid_state.select(Some(1));
        a.on_key(key(KeyCode::Char('u')));
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(a.last_error, Some(a.not_connected_message()));
        a.demo = true;
        a.on_key(key(KeyCode::Char('u')));
        assert_eq!(
            a.last_error.as_deref(),
            Some("can't edit: --demo has no database to write to")
        );
    }

    #[test]
    fn the_read_only_refusal_names_why_and_the_way_out() {
        let cfg = SafetyConfig::default();
        assert_eq!(
            read_only_edit_refusal(true, true, &cfg, "app", true),
            format!("can't edit: {}", cmd::REPLICA_READONLY_REFUSAL)
        );
        assert_eq!(
            read_only_edit_refusal(false, true, &cfg, "app", false),
            format!("can't edit: {}", crate::conn::READ_ONLY_DEFAULT_HINT)
        );
        assert_eq!(
            read_only_edit_refusal(false, false, &cfg, "app", true),
            "can't edit: read-only (:readonly on) · :readonly off to write"
        );
    }

    #[test]
    fn a_paste_inserts_one_line_and_refuses_a_newline() {
        let mut a = editing(None);
        a.on_paste("a'b\\c".into());
        assert_eq!(value(&a), Some("a'b\\c".into()));
        a.on_paste("two\nlines".into());
        assert_eq!(value(&a), Some("a'b\\c".into()));
        // A copied line's own line break is not part of the value.
        a.on_paste("!\r\n".into());
        a.on_paste("?\n".into());
        assert_eq!(value(&a), Some("a'b\\c!?".into()));
        assert!(a
            .last_status
            .as_deref()
            .is_some_and(|s| s.starts_with("can't paste a newline")));
    }

    #[test]
    fn only_a_single_statement_passes_the_split_check() {
        assert!(is_one_statement(
            "UPDATE public.users SET name = 'a;b' WHERE id = '2' RETURNING name"
        ));
        assert!(!is_one_statement(
            "UPDATE public.users SET name = 'a' WHERE id = '2'; DROP TABLE users"
        ));
        assert!(!is_one_statement("UPDATE t SET a = 'unterminated"));
    }

    #[test]
    fn set_edited_cell_prefers_the_hint_and_finds_a_moved_row() {
        let before = cells(&[Some("2"), Some("bo")]);
        let mut rows = vec![
            cells(&[Some("1"), Some("ann")]),
            before.clone(),
            cells(&[Some("3"), None]),
        ];
        assert!(set_edited_cell(&mut rows, 1, &before, 1, None));
        assert_eq!(rows[1], cells(&[Some("2"), None]));
        // Moved: the hint points at another row, which is left alone.
        let mut rows = vec![before.clone(), cells(&[Some("1"), Some("ann")])];
        assert!(set_edited_cell(&mut rows, 1, &before, 1, Some("x".into())));
        assert_eq!(rows[0], cells(&[Some("2"), Some("x")]));
        assert_eq!(rows[1], cells(&[Some("1"), Some("ann")]));
        // Gone.
        assert!(!set_edited_cell(&mut rows, 0, &before, 1, None));
    }
}

/// The whole `u` journey in a real `App` against the compose database
/// (`--features integration`; docker-compose.test.yml): run a SELECT from
/// the editor, `u` on a cell, type, Enter, answer the confirm, and see
/// the database and the grid agree. Tables are `pgman_it_<what>_<pid>`,
/// so `tests/integration.rs`'s clean-up drops any a killed run leaves.
#[cfg(all(test, feature = "integration"))]
mod live {
    use super::*;
    use crate::theme::Theme;

    const DSN: &str = "postgres://pgman_test:pgman_test@127.0.0.1:55432/pgman_test?sslmode=disable";

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn name(what: &str) -> String {
        format!("pgman_it_{what}_{}", std::process::id())
    }

    async fn client() -> Arc<tokio_postgres::Client> {
        let (notice_tx, _) = tokio::sync::mpsc::unbounded_channel();
        let (notification_tx, _) = tokio::sync::mpsc::unbounded_channel();
        let dsn = Dsn::parse(DSN).expect("test DSN");
        conn::connect_only(dsn, false, 30_000, notice_tx, notification_tx)
            .await
            .expect("connect to the compose postgres")
            .0
    }

    /// A connected, writable App (guards as shipped: an UPDATE confirms;
    /// no `auto_tx` unless a test turns it on) with a fresh schema cache.
    async fn app(client: &Arc<tokio_postgres::Client>) -> App {
        let mut cfg = SafetyConfig::default();
        cfg.default.read_only = false;
        cfg.default.auto_tx = false;
        let dsn = Dsn::parse(DSN).ok();
        let mut a = App::new(Theme::default(), dsn, Vec::new(), cfg);
        a.splash_visible = false;
        a.splash_until = None;
        a.client = Some(client.clone());
        // As a connect does (`handle.rs`): a plain SELECT runs behind
        // the pager, so its origins must come through that route too.
        a.pager = Some(crate::query::paging::Pager::spawn(client.clone()));
        a.read_only = false;
        a.conn_state = ConnState::Connected {
            server_version: "16".into(),
        };
        a.schema_cache = crate::query::schema::fetch(client).await;
        a.mode = Mode::Normal;
        a
    }

    /// Hand the next message the App's spawned task sends to `on_msg`.
    async fn pump(a: &mut App) {
        let msg = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            a.msg_rx.as_mut().expect("msg_rx").recv(),
        )
        .await
        .expect("a message within 10s")
        .expect("channel open");
        a.on_msg(msg);
    }

    async fn run(a: &mut App, sql: &str) {
        a.editor.buffer = sql.to_string();
        a.request_run(RunKind::Run);
        pump(a).await;
        assert!(a.last_error.is_none(), "{sql}: {:?}", a.last_error);
    }

    /// Focus (row, col) and press `u`.
    fn u_at(a: &mut App, row: usize, col: usize) {
        a.mode = Mode::Normal;
        a.grid_state.select(Some(row));
        a.grid_view.col_cursor = col;
        a.on_key(key(KeyCode::Char('u')));
    }

    fn type_and_save(a: &mut App, text: &str) {
        let ui = a.cell_edit.as_mut().expect("the editor opened");
        ui.input.clear();
        for c in text.chars() {
            a.on_key(key(KeyCode::Char(c)));
        }
        a.on_key(key(KeyCode::Enter));
    }

    async fn scalar(client: &tokio_postgres::Client, sql: &str) -> Option<String> {
        client
            .query_one(&format!("SELECT ({sql})::text"), &[])
            .await
            .expect(sql)
            .get(0)
    }

    #[tokio::test]
    async fn u_edits_a_cell_through_the_confirm_and_the_grid_shows_the_servers_value() {
        let client = client().await;
        let t = name("live_edit");
        client
            .batch_execute(&format!(
                "CREATE TABLE {t} (id int PRIMARY KEY, price numeric(8,2), note text); \
                 INSERT INTO {t} VALUES (1, 1, 'a'), (2, 2, 'b')"
            ))
            .await
            .expect("create");
        let mut a = app(&client).await;
        run(&mut a, &format!("SELECT * FROM {t} ORDER BY id")).await;
        u_at(&mut a, 1, 1);
        assert_eq!(a.mode, Mode::CellEdit, "{:?}", a.last_error);
        type_and_save(&mut a, "2.5");
        assert_eq!(a.mode, Mode::Confirm);
        let sql = a.pending_run.as_ref().expect("confirm").sql.clone();
        a.on_key(key(KeyCode::Char('y')));
        assert_eq!(a.mode, Mode::Normal);
        pump(&mut a).await;
        let stored = scalar(&client, &format!("SELECT price FROM {t} WHERE id = 2")).await;
        let other = scalar(&client, &format!("SELECT price FROM {t} WHERE id = 1")).await;
        let _ = client.batch_execute(&format!("DROP TABLE {t}")).await;
        assert_eq!(
            sql,
            format!("UPDATE public.{t} SET price = '2.5' WHERE id = '2' RETURNING price")
        );
        assert_eq!(stored.as_deref(), Some("2.50"));
        assert_eq!(other.as_deref(), Some("1.00"));
        assert_eq!(a.grid.rows[1][1].as_deref(), Some("2.50"));
        assert_eq!(
            a.last_status.as_deref(),
            Some(format!("updated public.{t}.price where id = 2 · committed").as_str())
        );
        assert_eq!(a.history.last(), Some(&sql));
    }

    /// `D`, a change, the same query, `D`: keyed on the table's primary
    /// key (not the unique `name` to its left), a renamed row is one
    /// change.
    #[tokio::test]
    async fn the_result_diff_keys_on_the_catalogs_primary_key() {
        let client = client().await;
        let t = name("live_diff");
        client
            .batch_execute(&format!(
                "CREATE TABLE {t} (name text UNIQUE, id int PRIMARY KEY); \
                 INSERT INTO {t} VALUES ('ann', 1), ('bo', 2)"
            ))
            .await
            .expect("create");
        let mut a = app(&client).await;
        let select = format!("SELECT name, id FROM {t} ORDER BY id");
        run(&mut a, &select).await;
        a.on_key(key(KeyCode::Char('D')));
        client
            .batch_execute(&format!("UPDATE {t} SET name = 'anna' WHERE id = 1"))
            .await
            .expect("rename");
        run(&mut a, &select).await;
        a.on_key(key(KeyCode::Char('D')));
        let _ = client.batch_execute(&format!("DROP TABLE {t}")).await;
        assert_eq!(a.mode, Mode::ResultDiff);
        let d = a.result_diff.active.as_ref().expect("a diff");
        assert_eq!(d.key, crate::query::row_diff::RowKey::Columns(vec![1]));
        assert!(d.key_is_pk);
        assert_eq!(
            (
                d.diff.changed.len(),
                d.diff.added.len(),
                d.diff.removed.len()
            ),
            (1, 0, 0)
        );
    }

    /// A database whose guard allows an UPDATE runs the edit at Enter,
    /// with no confirm.
    #[tokio::test]
    async fn an_allowed_update_runs_without_a_confirm() {
        let client = client().await;
        let t = name("live_allow");
        client
            .batch_execute(&format!(
                "CREATE TABLE {t} (id int PRIMARY KEY, note text); INSERT INTO {t} VALUES (1, 'a')"
            ))
            .await
            .expect("create");
        let mut a = app(&client).await;
        a.safety_config.default.guards.update = Guard::Allow;
        run(&mut a, &format!("SELECT * FROM {t}")).await;
        u_at(&mut a, 0, 1);
        type_and_save(&mut a, "it's");
        let mode = a.mode;
        pump(&mut a).await;
        let stored = scalar(&client, &format!("SELECT note FROM {t}")).await;
        let _ = client.batch_execute(&format!("DROP TABLE {t}")).await;
        assert_eq!(mode, Mode::Normal);
        assert!(a.pending_run.is_none());
        assert_eq!(stored.as_deref(), Some("it's"));
        assert_eq!(a.grid.rows[0][1].as_deref(), Some("it's"));
    }

    /// Refused at `u`, before an editor opens: a join, a read-only
    /// session, a transaction awaiting its prompt.
    #[tokio::test]
    async fn u_refuses_a_join_a_read_only_session_and_an_open_transaction() {
        let client = client().await;
        let t = name("live_refuse");
        client
            .batch_execute(&format!(
                "CREATE TABLE {t} (id int PRIMARY KEY, note text); INSERT INTO {t} VALUES (1, 'a')"
            ))
            .await
            .expect("create");
        let mut a = app(&client).await;
        run(
            &mut a,
            &format!("SELECT a.id, a.note FROM {t} a JOIN {t} b USING (id)"),
        )
        .await;
        u_at(&mut a, 0, 1);
        let join = (a.mode, a.last_error.clone());
        run(&mut a, &format!("SELECT * FROM {t}")).await;
        a.tx_open = true;
        u_at(&mut a, 0, 1);
        let tx = (a.mode, a.last_error.clone());
        a.tx_open = false;
        a.read_only = true;
        a.safety_config.default.read_only = true;
        u_at(&mut a, 0, 1);
        let ro = (a.mode, a.last_error.clone());
        let _ = client.batch_execute(&format!("DROP TABLE {t}")).await;
        assert_eq!(
            join,
            (
                Mode::Normal,
                Some(
                    "can't edit: not a SELECT from one table (a join, subquery or no FROM)".into()
                )
            )
        );
        assert_eq!(
            tx,
            (
                Mode::Normal,
                Some("a transaction is open — commit (y) or roll back (n) first, then edit".into())
            )
        );
        assert_eq!(ro.0, Mode::Normal);
        assert!(
            ro.1.as_deref()
                .is_some_and(|e| e.starts_with("can't edit: ") && e.contains("read_only = false")),
            "{:?}",
            ro.1
        );
    }

    /// Two rows match (an inheritance child shares the key): the UPDATE
    /// is rolled back, the footer says so, and neither the database nor
    /// the grid changed.
    #[tokio::test]
    async fn an_edit_matching_two_rows_is_rolled_back_and_says_so() {
        let client = client().await;
        let parent = name("live_parent");
        let child = name("live_child");
        client
            .batch_execute(&format!(
                "CREATE TABLE {parent} (id int PRIMARY KEY, note text); \
                 CREATE TABLE {child} () INHERITS ({parent}); \
                 INSERT INTO {parent} VALUES (1, 'p'); INSERT INTO {child} VALUES (1, 'c')"
            ))
            .await
            .expect("create");
        let mut a = app(&client).await;
        run(&mut a, &format!("SELECT * FROM ONLY {parent}")).await;
        u_at(&mut a, 0, 1);
        type_and_save(&mut a, "x");
        a.on_key(key(KeyCode::Char('y')));
        pump(&mut a).await;
        let notes = scalar(
            &client,
            &format!("SELECT string_agg(note, ',' ORDER BY note) FROM {parent}"),
        )
        .await;
        let _ = client
            .batch_execute(&format!("DROP TABLE {parent} CASCADE"))
            .await;
        assert_eq!(
            a.last_error.as_deref(),
            Some("the UPDATE matched 2 rows, not 1 · rolled back, nothing changed")
        );
        assert!(!a.query_running);
        assert_eq!(notes.as_deref(), Some("c,p"));
        assert_eq!(a.grid.rows[0][1].as_deref(), Some("p"));
    }

    /// `auto_tx`: the edit waits on the commit / rollback prompt; `n`
    /// rolls it back in the database and in the grid.
    #[tokio::test]
    async fn an_auto_tx_edit_rolled_back_at_the_prompt_changes_nothing() {
        let client = client().await;
        let t = name("live_autotx");
        client
            .batch_execute(&format!(
                "CREATE TABLE {t} (id int PRIMARY KEY, note text); INSERT INTO {t} VALUES (1, NULL)"
            ))
            .await
            .expect("create");
        let mut a = app(&client).await;
        a.safety_config.default.auto_tx = true;
        run(&mut a, &format!("SELECT * FROM {t}")).await;
        u_at(&mut a, 0, 1);
        // NULL → the empty string: Ctrl-N on a NULL.
        a.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        a.on_key(key(KeyCode::Enter));
        a.on_key(key(KeyCode::Char('y')));
        pump(&mut a).await;
        let mode_after_edit = a.mode;
        let grid_after_edit = a.grid.rows[0][1].clone();
        a.on_key(key(KeyCode::Char('n')));
        pump(&mut a).await;
        let stored = scalar(&client, &format!("SELECT note IS NULL FROM {t}")).await;
        let _ = client.batch_execute(&format!("DROP TABLE {t}")).await;
        assert_eq!(mode_after_edit, Mode::TxDecision);
        assert_eq!(grid_after_edit, Some(String::new()));
        assert_eq!(stored.as_deref(), Some("true"));
        assert_eq!(a.grid.rows[0][1], None);
        assert_eq!(a.mode, Mode::Normal);
    }
}
