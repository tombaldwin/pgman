use super::*;

impl App {
    /// Step back into older history (Ctrl-P). The first step saves the live
    /// draft so Ctrl-N past the newest entry can restore it.
    pub(super) fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let new_pos = match self.history_pos {
            None => {
                self.history_draft = self.editor.buffer.clone();
                self.history.len() - 1
            }
            Some(i) if i > 0 => i - 1,
            Some(_) => return,
        };
        self.history_pos = Some(new_pos);
        self.editor.buffer = self.history[new_pos].clone();
        self.editor.cursor = self.editor.buffer.len();
        self.editor.preferred_col = None;
    }

    /// Step forward into newer history (Ctrl-N). Past the newest entry,
    /// restores the saved draft.
    pub(super) fn history_next(&mut self) {
        let Some(pos) = self.history_pos else {
            return;
        };
        if pos + 1 < self.history.len() {
            self.history_pos = Some(pos + 1);
            self.editor.buffer = self.history[pos + 1].clone();
        } else {
            self.editor.buffer = std::mem::take(&mut self.history_draft);
            self.history_pos = None;
        }
        self.editor.cursor = self.editor.buffer.len();
        self.editor.preferred_col = None;
    }

    /// Ctrl-R from the editor — enter reverse-incremental history
    /// search. Snapshots the buffer/cursor so Esc can restore them;
    /// then begins searching from the newest history entry. The
    /// initial query is empty, so the most-recent entry shows by
    /// default (matches bash's "Ctrl-R then Enter recalls the last
    /// command" idiom). If history is empty, surface a status and
    /// stay in editor mode.
    pub(super) fn start_history_search(&mut self) {
        if self.history.is_empty() {
            self.last_status = Some("history is empty".to_string());
            return;
        }
        let saved_buffer = self.editor.buffer.clone();
        let saved_cursor = self.editor.cursor;
        let initial = self.history.len() - 1;
        self.history_search = Some(HistorySearchState {
            query: String::new(),
            matched: Some(initial),
            saved_buffer,
            saved_cursor,
        });
        // Mirror the most-recent entry into the buffer so the
        // operator can see what they'd commit to with Enter.
        self.editor.buffer = self.history[initial].clone();
        self.editor.cursor = self.editor.buffer.len();
        self.mode = Mode::HistorySearch;
        self.refresh_history_search_status();
    }

    /// Sync the footer status to the active history-search session —
    /// `(reverse-i-search) 'query'` when there's a match, or
    /// `(failed reverse-i-search) 'query'` when not (mirroring bash).
    pub(super) fn refresh_history_search_status(&mut self) {
        let Some(state) = self.history_search.as_ref() else {
            return;
        };
        self.last_status = Some(match state.matched {
            Some(_) => format!("(reverse-i-search) '{}'", state.query),
            None => format!("(failed reverse-i-search) '{}'", state.query),
        });
    }

    /// Reverse-incremental search step: starting from
    /// `state.matched.unwrap_or(history.len())`, walk backward (older)
    /// looking for an entry whose lowercased text contains the lower-
    /// cased query as a substring. Updates `state.matched` and the
    /// editor buffer in place.
    pub(super) fn history_search_step(&mut self, from_index: Option<usize>) {
        let Some(state) = self.history_search.as_ref() else {
            return;
        };
        let found = history_search_next(&self.history, &state.query, from_index);
        // Borrow `history_search` mutably for the write of `matched`.
        if let Some(s) = self.history_search.as_mut() {
            s.matched = found;
        }
        if let Some(i) = found {
            self.editor.buffer = self.history[i].clone();
            self.editor.cursor = self.editor.buffer.len();
        }
        // If `found` is None we leave the buffer alone (showing the
        // last good match) — same UX as bash, where a failed search
        // displays `(failed reverse-i-search)` but keeps the prior
        // match on screen.
    }
}
