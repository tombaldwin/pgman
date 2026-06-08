use super::*;

impl App {
    /// Snapshot the currently-active per-session fields into
    /// `tabs[active_tab]`. Called before every switch and on
    /// every tab-close. Pure mechanical copy — no side effects.
    pub(super) fn snapshot_active_tab(&mut self) {
        let snap = TabSnapshot {
            editor: self.editor.clone(),
            grid: self.grid.clone(),
            grid_selected: self.grid_state.selected(),
            grid_col_cursor: self.grid_col_cursor,
            grid_sort: self.grid_sort,
            grid_raw_rows: self.grid_raw_rows.clone(),
            grid_filter: self.grid_filter.clone(),
            grid_visible_rows: self.grid_visible_rows.clone(),
            last_run_sql: self.last_run_sql.clone(),
            grid_source: self.grid_source.clone(),
            pinned_result: self.diff.pinned.clone(),
        };
        if let Some(slot) = self.tabs.get_mut(self.active_tab) {
            *slot = snap;
        }
    }

    /// Restore `tabs[active_tab]` into the per-session fields.
    /// Mirror of `snapshot_active_tab`.
    pub(super) fn load_active_tab(&mut self) {
        let snap = match self.tabs.get(self.active_tab) {
            Some(s) => s.clone(),
            None => return,
        };
        self.editor = snap.editor;
        self.grid = snap.grid;
        self.grid_state.select(snap.grid_selected);
        self.grid_col_cursor = snap.grid_col_cursor;
        self.grid_sort = snap.grid_sort;
        self.grid_raw_rows = snap.grid_raw_rows;
        self.grid_filter = snap.grid_filter;
        self.grid_visible_rows = snap.grid_visible_rows;
        self.last_run_sql = snap.last_run_sql;
        self.grid_source = snap.grid_source;
        self.diff.pinned = snap.pinned_result;
    }

    /// Close the transient result-diff overlay if one is open. Called
    /// on every tab change: the overlay is bound to the tab's live grid,
    /// which is about to swap out from under it, so it must not survive
    /// the switch. The per-tab `pinned_result` baseline is preserved
    /// (snapshotted/restored separately).
    pub(super) fn dismiss_result_diff(&mut self) {
        if self.mode == Mode::ResultDiff {
            self.mode = Mode::Normal;
        }
        self.diff.active = None;
        self.diff.cursor = 0;
    }

    /// `Ctrl-T` — push a fresh tab and switch to it. Refuses
    /// past `TAB_CAP` with an actionable status.
    pub fn new_tab(&mut self) {
        if self.tabs.len() >= TAB_CAP {
            self.last_status = Some(format!("max tabs reached ({TAB_CAP}) — close one first"));
            return;
        }
        if self.query_running {
            self.last_status = Some("can't switch tabs while a query is running".into());
            return;
        }
        self.dismiss_result_diff();
        self.snapshot_active_tab();
        self.tabs.push(TabSnapshot::default());
        self.active_tab = self.tabs.len() - 1;
        self.load_active_tab();
        // Reset transient state that doesn't belong to a tab
        // (completion popup, history nav).
        self.completion = None;
        self.history_pos = None;
        self.last_status = Some(format!(
            "new tab · now on tab {}/{}",
            self.active_tab + 1,
            self.tabs.len()
        ));
    }

    /// `Ctrl-W` — close the active tab. The next tab becomes
    /// active (or the previous if active was last). No-op when
    /// only one tab exists (closing the last is a quit-via-q).
    pub fn close_active_tab(&mut self) {
        if self.tabs.len() <= 1 {
            self.last_status = Some("only one tab open · q to quit".into());
            return;
        }
        if self.query_running {
            self.last_status = Some("can't close tab while a query is running".into());
            return;
        }
        self.dismiss_result_diff();
        self.tabs.remove(self.active_tab);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        self.load_active_tab();
        self.completion = None;
        self.last_status = Some(format!(
            "closed tab · now on tab {}/{}",
            self.active_tab + 1,
            self.tabs.len()
        ));
    }

    /// Jump to `idx` (0-based). No-op out-of-range / same tab.
    pub fn switch_to_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() || idx == self.active_tab {
            return;
        }
        if self.query_running {
            self.last_status = Some("can't switch tabs while a query is running".into());
            return;
        }
        self.dismiss_result_diff();
        self.snapshot_active_tab();
        self.active_tab = idx;
        self.load_active_tab();
        self.completion = None;
        self.history_pos = None;
        self.last_status = Some(format!("tab {}/{}", self.active_tab + 1, self.tabs.len()));
    }

    /// Step forward / backward through the tab list, wrapping.
    pub fn cycle_tab(&mut self, forward: bool) {
        if self.tabs.len() <= 1 {
            return;
        }
        let n = self.tabs.len();
        let next = if forward {
            (self.active_tab + 1) % n
        } else {
            (self.active_tab + n - 1) % n
        };
        self.switch_to_tab(next);
    }
}
