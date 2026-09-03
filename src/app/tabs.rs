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
            grid_view: self.grid_view.clone(),
            last_run_sql: self.last_run_sql.clone(),
            pinned_result: self.result_diff.pinned.clone(),
            bookmarks: self.bookmarks.clone(),
            editor_lines: self.editor_lines,
            zoomed: self.zoomed,
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
        self.grid_view = snap.grid_view;
        self.last_run_sql = snap.last_run_sql;
        self.result_diff.pinned = snap.pinned_result;
        self.bookmarks = snap.bookmarks;
        self.editor_lines = snap.editor_lines;
        self.zoomed = snap.zoomed;
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
        self.result_diff.active = None;
        self.result_diff.cursor = 0;
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

/// Fewest content lines the editor can be shrunk to by hand.
const EDITOR_LINES_MIN: u16 = 1;
/// Rows the result pane keeps when the editor is grown by hand — its
/// two borders and one content row — so `Alt-=` can never push the
/// grid off the screen; that is what `Alt-Z` is for.
const RESULTS_ROWS_MIN: u16 = 3;

impl App {
    /// `Alt-Z` — the focused pane takes the whole body; again restores
    /// the split as it was (manual or automatic — `editor_lines` is not
    /// touched). Per-tab.
    pub fn toggle_zoom(&mut self) {
        self.zoomed = !self.zoomed;
        self.last_status = Some(if self.zoomed {
            let pane = if self.mode == Mode::Editor {
                "editor"
            } else {
                "results"
            };
            format!("{pane} zoomed · alt-z restores the split")
        } else {
            "zoom off".to_string()
        });
    }

    /// `Alt-=` / `Alt--` — grow / shrink the editor by `delta` content
    /// lines, starting from whatever it is on screen now (the automatic
    /// split when no size was set). Clamped to `[1, body - results
    /// minimum]` against the last rendered body height. Leaves zoom
    /// alone: the new size shows once the zoom is toggled off.
    pub fn resize_editor(&mut self, delta: i32) {
        let body = self.body_rows;
        let auto_lines = crate::ui::editor_rows(self.editor_content_lines(), body)
            .saturating_sub(crate::ui::EDITOR_BORDERS);
        let current = self.editor_lines.unwrap_or(auto_lines);
        let max = body
            .saturating_sub(crate::ui::EDITOR_BORDERS + RESULTS_ROWS_MIN)
            .max(EDITOR_LINES_MIN);
        let wanted = i64::from(current) + i64::from(delta);
        let clamped = wanted.clamp(i64::from(EDITOR_LINES_MIN), i64::from(max));
        // `max ≥ EDITOR_LINES_MIN ≥ 1`, so the clamp always lands in u16.
        let next = u16::try_from(clamped).unwrap_or(EDITOR_LINES_MIN);
        self.editor_lines = Some(next);
        let edge = if wanted > i64::from(max) {
            " (results keep a row · alt-z to zoom)"
        } else if wanted < i64::from(EDITOR_LINES_MIN) {
            " (minimum)"
        } else {
            ""
        };
        self.last_status = Some(format!(
            "editor {next} line{}{edge} · alt-0 auto",
            if next == 1 { "" } else { "s" }
        ));
    }

    /// `Alt-0` — back to the automatic split.
    pub fn reset_editor_size(&mut self) {
        self.editor_lines = None;
        self.last_status = Some("editor auto-sized".into());
    }

    /// Lines in the editor buffer — the same count `ui::draw` feeds
    /// `editor_rows`, so a resize starts from the height on screen.
    fn editor_content_lines(&self) -> usize {
        self.editor.buffer.matches('\n').count() + 1
    }
}
