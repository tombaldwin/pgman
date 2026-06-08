use super::*;

impl App {
    pub(super) fn on_tap_monitor_key(&mut self, key: KeyEvent) {
        // `B` is universal across all TapMonitor views —
        // captures the current hotspots as a baseline so the
        // operator can then cycle to the Baseline view to see
        // the diff. Surfacing this on every view (instead of
        // gating to Baseline only) means the "I just deployed,
        // grab a baseline NOW" workflow is one keystroke.
        if matches!(key.code, KeyCode::Char('B')) && key.modifiers.contains(KeyModifiers::SHIFT) {
            self.capture_tap_baseline();
            return;
        }
        match self.tap_nav.view {
            TapView::List => self.on_tap_monitor_list_key(key),
            TapView::Hotspots => self.on_tap_monitor_hotspots_key(key),
            TapView::Callers => self.on_tap_monitor_callers_key(key),
            TapView::Transactions => self.on_tap_monitor_txns_key(key),
            TapView::Pools => self.on_tap_monitor_pools_key(key),
            TapView::NplusOne => self.on_tap_monitor_nplus1_key(key),
            TapView::Baseline => self.on_tap_monitor_baseline_key(key),
        }
    }

    pub(super) fn on_tap_monitor_txns_key(&mut self, key: KeyEvent) {
        let last = self.current_txns().len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                self.last_status = None;
            }
            KeyCode::Char('v') => self.cycle_tap_view(),
            KeyCode::Char('j') | KeyCode::Down => {
                self.tap_nav.txns_cursor = (self.tap_nav.txns_cursor + 1).min(last);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.tap_nav.txns_cursor = self.tap_nav.txns_cursor.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.tap_nav.txns_cursor = 0,
            KeyCode::Char('G') | KeyCode::End => self.tap_nav.txns_cursor = last,
            KeyCode::PageDown => {
                self.tap_nav.txns_cursor = (self.tap_nav.txns_cursor + 10).min(last);
            }
            KeyCode::PageUp => {
                self.tap_nav.txns_cursor = self.tap_nav.txns_cursor.saturating_sub(10);
            }
            KeyCode::Char('c') => self.clear_tap_ring(),
            _ => {}
        }
    }

    pub(super) fn on_tap_monitor_pools_key(&mut self, key: KeyEvent) {
        let last = self.current_pools().len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                self.last_status = None;
            }
            KeyCode::Char('v') => self.cycle_tap_view(),
            KeyCode::Char('j') | KeyCode::Down => {
                self.tap_nav.pools_cursor = (self.tap_nav.pools_cursor + 1).min(last);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.tap_nav.pools_cursor = self.tap_nav.pools_cursor.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.tap_nav.pools_cursor = 0,
            KeyCode::Char('G') | KeyCode::End => self.tap_nav.pools_cursor = last,
            KeyCode::PageDown => {
                self.tap_nav.pools_cursor = (self.tap_nav.pools_cursor + 10).min(last);
            }
            KeyCode::PageUp => {
                self.tap_nav.pools_cursor = self.tap_nav.pools_cursor.saturating_sub(10);
            }
            KeyCode::Char('c') => self.clear_tap_ring(),
            _ => {}
        }
    }

    pub(super) fn on_tap_monitor_baseline_key(&mut self, key: KeyEvent) {
        let last = self.current_baseline_diff().len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                self.last_status = None;
            }
            KeyCode::Char('v') => self.cycle_tap_view(),
            KeyCode::Char('j') | KeyCode::Down => {
                self.tap_nav.baseline_cursor = (self.tap_nav.baseline_cursor + 1).min(last);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.tap_nav.baseline_cursor = self.tap_nav.baseline_cursor.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.tap_nav.baseline_cursor = 0,
            KeyCode::Char('G') | KeyCode::End => self.tap_nav.baseline_cursor = last,
            KeyCode::PageDown => {
                self.tap_nav.baseline_cursor = (self.tap_nav.baseline_cursor + 10).min(last);
            }
            KeyCode::PageUp => {
                self.tap_nav.baseline_cursor = self.tap_nav.baseline_cursor.saturating_sub(10);
            }
            KeyCode::Char('c') => self.clear_tap_ring(),
            _ => {}
        }
    }

    pub(super) fn on_tap_monitor_callers_key(&mut self, key: KeyEvent) {
        let last = self.current_callers().len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                self.last_status = None;
            }
            KeyCode::Char('v') => self.cycle_tap_view(),
            // `s` cycles the sort (shared HotspotSort with the
            // hotspots view — TotalTime / CallCount / P95Latency).
            KeyCode::Char('s') => {
                self.tap_nav.sort = self.tap_nav.sort.next();
                self.last_status =
                    Some(format!("tap callers · sort: {}", self.tap_nav.sort.label()));
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.tap_nav.callers_cursor = (self.tap_nav.callers_cursor + 1).min(last);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.tap_nav.callers_cursor = self.tap_nav.callers_cursor.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.tap_nav.callers_cursor = 0,
            KeyCode::Char('G') | KeyCode::End => self.tap_nav.callers_cursor = last,
            KeyCode::PageDown => {
                self.tap_nav.callers_cursor = (self.tap_nav.callers_cursor + 10).min(last);
            }
            KeyCode::PageUp => {
                self.tap_nav.callers_cursor = self.tap_nav.callers_cursor.saturating_sub(10);
            }
            KeyCode::Char('c') => self.clear_tap_ring(),
            _ => {}
        }
    }

    pub(super) fn on_tap_monitor_nplus1_key(&mut self, key: KeyEvent) {
        let last = self.current_nplus1().len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                self.last_status = None;
            }
            KeyCode::Char('v') => self.cycle_tap_view(),
            KeyCode::Char('j') | KeyCode::Down => {
                self.tap_nav.nplus1_cursor = (self.tap_nav.nplus1_cursor + 1).min(last);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.tap_nav.nplus1_cursor = self.tap_nav.nplus1_cursor.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.tap_nav.nplus1_cursor = 0,
            KeyCode::Char('G') | KeyCode::End => self.tap_nav.nplus1_cursor = last,
            KeyCode::PageDown => {
                self.tap_nav.nplus1_cursor = (self.tap_nav.nplus1_cursor + 10).min(last);
            }
            KeyCode::PageUp => {
                self.tap_nav.nplus1_cursor = self.tap_nav.nplus1_cursor.saturating_sub(10);
            }
            KeyCode::Char('c') => self.clear_tap_ring(),
            _ => {}
        }
    }

    pub(super) fn on_tap_monitor_list_key(&mut self, key: KeyEvent) {
        let last = self.tap_events.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                self.last_status = None;
            }
            // `v` toggles to the hotspots (grouped) view. We
            // keep vim-style g/G for top/bottom within the
            // current view; `v` is the cross-view mnemonic
            // ("view").
            KeyCode::Char('v') => self.cycle_tap_view(),
            KeyCode::Char('j') | KeyCode::Down => {
                self.tap_nav.events_cursor = (self.tap_nav.events_cursor + 1).min(last);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.tap_nav.events_cursor = self.tap_nav.events_cursor.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.tap_nav.events_cursor = 0,
            KeyCode::Char('G') | KeyCode::End => self.tap_nav.events_cursor = last,
            KeyCode::PageDown => {
                self.tap_nav.events_cursor = (self.tap_nav.events_cursor + 10).min(last);
            }
            KeyCode::PageUp => {
                self.tap_nav.events_cursor = self.tap_nav.events_cursor.saturating_sub(10);
            }
            KeyCode::Char('c') => self.clear_tap_ring(),
            _ => {}
        }
    }

    pub(super) fn on_tap_monitor_hotspots_key(&mut self, key: KeyEvent) {
        let last = self.current_hotspots().len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                self.last_status = None;
            }
            // `v` toggles back to the list view (mirror of the
            // list-side binding).
            KeyCode::Char('v') => self.cycle_tap_view(),
            // 's' cycles the sort mode and flashes the new mode
            // so the operator sees what they just selected.
            KeyCode::Char('s') => {
                self.tap_nav.sort = self.tap_nav.sort.next();
                self.last_status = Some(format!(
                    "tap hotspots · sort: {}",
                    self.tap_nav.sort.label()
                ));
                // Resort uses the same grouping; cursor stays at
                // its index (callers parking on a row see the row
                // move under them — acceptable for a sort cycle).
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.tap_nav.hotspots_cursor = (self.tap_nav.hotspots_cursor + 1).min(last);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.tap_nav.hotspots_cursor = self.tap_nav.hotspots_cursor.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.tap_nav.hotspots_cursor = 0,
            KeyCode::Char('G') | KeyCode::End => self.tap_nav.hotspots_cursor = last,
            KeyCode::PageDown => {
                self.tap_nav.hotspots_cursor = (self.tap_nav.hotspots_cursor + 10).min(last);
            }
            KeyCode::PageUp => {
                self.tap_nav.hotspots_cursor = self.tap_nav.hotspots_cursor.saturating_sub(10);
            }
            KeyCode::Char('c') => self.clear_tap_ring(),
            _ => {}
        }
    }

    pub(super) fn on_notifications_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                self.last_status = None;
            }
            KeyCode::Char('j') | KeyCode::Down => self.notifications.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.notifications.select_prev(),
            KeyCode::Char('g') | KeyCode::Home => self.notifications.select_first(),
            KeyCode::Char('G') | KeyCode::End => self.notifications.select_last(),
            KeyCode::PageDown => self.notifications.page_down(),
            KeyCode::PageUp => self.notifications.page_up(),
            KeyCode::Char('c') => {
                let n = self.notifications.items.len();
                self.notifications.items.clear();
                self.notifications.cursor = 0;
                self.last_status = Some(format!("cleared {n} notification(s)"));
            }
            KeyCode::Char('y') => self.yank_focused_notification(),
            _ => {}
        }
    }

    pub(super) fn on_save_query_prompt_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.saved_ui.save_name.clear();
                self.last_status = Some("save cancelled".into());
                self.mode = Mode::Editor;
            }
            KeyCode::Enter => {
                let name = self.saved_ui.save_name.trim().to_string();
                if name.is_empty() {
                    self.last_status = Some("name required".into());
                    return;
                }
                let body = self.editor.buffer.clone();
                let replaced = self.saved_queries.upsert(crate::saved::SavedQuery {
                    name: name.clone(),
                    body,
                });
                // Persist immediately so a crash doesn't lose it
                // (the on-quit save is the safety net, not the
                // primary).
                if let Err(e) = crate::saved::save_to(&saved_queries_path(), &self.saved_queries) {
                    self.last_error = Some(format!("save failed: {e}"));
                }
                self.last_status = Some(format!(
                    "saved query '{name}' ({})",
                    if replaced { "replaced" } else { "new" }
                ));
                self.saved_ui.save_name.clear();
                self.mode = Mode::Editor;
            }
            KeyCode::Backspace => {
                self.saved_ui.save_name.pop();
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.saved_ui.save_name.push(c);
            }
            _ => {}
        }
    }

    pub(super) fn on_param_prompt_key(&mut self, key: KeyEvent) {
        // Take the prompt out so the completion path can borrow
        // `self` mutably (to load the editor) without aliasing.
        let Some(mut pp) = self.saved_ui.param_prompt.take() else {
            self.mode = Mode::Normal;
            return;
        };
        match key.code {
            KeyCode::Esc => {
                // Cancel back to the list (it's still populated).
                self.mode = Mode::SavedQueries;
                self.last_status = Some("param entry cancelled".into());
            }
            KeyCode::Enter => {
                let val = pp.input.trimmed().to_string();
                if val.is_empty() {
                    // Empty would splice into broken SQL — make the
                    // operator type something (or esc to cancel).
                    self.last_status = Some("value required (esc to cancel)".into());
                    self.saved_ui.param_prompt = Some(pp);
                    return;
                }
                pp.values.push(val);
                pp.input = TextInput::new();
                pp.idx += 1;
                if pp.idx >= pp.params.len() {
                    let map: std::collections::HashMap<String, String> = pp
                        .params
                        .iter()
                        .cloned()
                        .zip(pp.values.iter().cloned())
                        .collect();
                    let sql = crate::query::params::substitute_params(&pp.template, &map);
                    let n = pp.params.len();
                    let name = pp.query_name.clone();
                    self.load_sql_into_editor(sql, format!("loaded '{name}' with {n} param(s)"));
                } else {
                    self.saved_ui.param_prompt = Some(pp);
                }
            }
            // All editing (insert / backspace / cursor move / word-delete)
            // routes through the shared single-line widget.
            _ => {
                pp.input.handle_key(key);
                self.saved_ui.param_prompt = Some(pp);
            }
        }
    }

    pub(super) fn on_saved_queries_key(&mut self, key: KeyEvent) {
        // Compute the filtered/visible index list once per keypress —
        // it lowercases every entry name + body, so re-deriving it for
        // the cursor clamp and again for the focused entry was wasteful.
        let visible = self.visible_saved_indices();
        let last = visible.len().saturating_sub(1);
        let focused = visible.get(self.saved_ui.cursor).copied();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                // Leaving the panel clears the filter so the next
                // open starts fresh.
                self.saved_ui.filter = None;
                self.mode = Mode::Normal;
                self.last_status = None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.saved_ui.cursor = (self.saved_ui.cursor + 1).min(last);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.saved_ui.cursor = self.saved_ui.cursor.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.saved_ui.cursor = 0,
            KeyCode::Char('G') | KeyCode::End => self.saved_ui.cursor = last,
            KeyCode::Char('/') => self.start_saved_queries_filter(),
            KeyCode::Char('r') => self.start_rename_query(),
            KeyCode::Enter => {
                if let Some(q) = focused
                    .and_then(|i| self.saved_queries.entries.get(i))
                    .cloned()
                {
                    // Keep the filter: load_saved_query may open the
                    // :param prompt, and Esc there returns to this
                    // (still-filtered) list. A fresh `open_saved_queries`
                    // clears the filter on its own.
                    self.load_saved_query(q);
                }
            }
            KeyCode::Char('d') => {
                // Delete focused entry (no separate confirm — the
                // file persists on next quit so an accidental
                // delete can be recovered if the operator quits
                // ungracefully; but practically once saved is
                // written, gone is gone). Status hints what
                // happened.
                if let Some(name) = focused
                    .and_then(|i| self.saved_queries.entries.get(i))
                    .map(|q| q.name.clone())
                {
                    self.saved_queries.remove(&name);
                    if let Err(e) =
                        crate::saved::save_to(&saved_queries_path(), &self.saved_queries)
                    {
                        self.last_error = Some(format!("delete failed: {e}"));
                    }
                    let last_after = self.visible_saved_indices().len().saturating_sub(1);
                    if self.saved_ui.cursor > last_after {
                        self.saved_ui.cursor = last_after;
                    }
                    self.last_status = Some(format!("deleted saved query '{name}'"));
                }
            }
            _ => {}
        }
    }

    pub(super) fn on_saved_queries_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                // Cancel the search: drop the filter, back to the
                // full list.
                self.saved_ui.filter = None;
                self.saved_ui.cursor = 0;
                self.mode = Mode::SavedQueries;
                self.last_status = None;
            }
            KeyCode::Enter => {
                // Accept: keep the filter applied, return to nav.
                self.mode = Mode::SavedQueries;
                self.last_status = None;
            }
            // Editing routes through the shared widget. Re-home the list
            // cursor only when the filter *text* changed (the visible set
            // may have shrunk) — not on bare cursor movement.
            _ => {
                let filter = self.saved_ui.filter.get_or_insert_with(TextInput::new);
                let before = filter.text().len();
                filter.handle_key(key);
                if filter.text().len() != before {
                    self.saved_ui.cursor = 0;
                }
            }
        }
    }

    pub(super) fn on_rename_query_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.saved_ui.rename_buf.clear();
                self.saved_ui.rename_from.clear();
                self.mode = Mode::SavedQueries;
                self.last_status = Some("rename cancelled".into());
            }
            KeyCode::Enter => {
                let to = self.saved_ui.rename_buf.trimmed().to_string();
                if to.is_empty() {
                    self.last_status = Some("name required (esc to cancel)".into());
                    return;
                }
                let from = self.saved_ui.rename_from.clone();
                match self.saved_queries.rename(&from, &to) {
                    Ok(true) => {
                        if let Err(e) =
                            crate::saved::save_to(&saved_queries_path(), &self.saved_queries)
                        {
                            self.last_error = Some(format!("rename save failed: {e}"));
                        }
                        self.last_status = Some(format!("renamed '{from}' → '{to}'"));
                        self.saved_ui.rename_buf.clear();
                        self.saved_ui.rename_from.clear();
                        self.mode = Mode::SavedQueries;
                    }
                    Ok(false) => {
                        // Source vanished (shouldn't happen mid-modal).
                        self.last_status = Some(format!("'{from}' no longer exists"));
                        self.mode = Mode::SavedQueries;
                    }
                    Err(crate::saved::RenameError::Exists) => {
                        self.last_status =
                            Some(format!("a saved query named '{to}' already exists"));
                    }
                }
            }
            // Editing routes through the shared single-line widget.
            _ => {
                self.saved_ui.rename_buf.handle_key(key);
            }
        }
    }

    pub(super) fn on_confirm_terminate_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(pid) = self.pending_terminate.take() {
                    self.spawn_terminate_session(pid);
                }
                self.mode = Mode::Sessions;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.pending_terminate = None;
                self.last_status = Some("terminate cancelled".into());
                self.mode = Mode::Sessions;
            }
            _ => {}
        }
    }

    pub(super) fn on_error_detail_key(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(2)) {
            self.mode = Mode::Normal;
        }
    }

    pub(super) fn on_about_key(&mut self, key: KeyEvent) {
        if matches!(
            key.code,
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q' | 'A')
        ) {
            self.mode = Mode::Normal;
        }
    }

    /// Row-detail modal: j/k navigate fields (renderer auto-scrolls so the
    /// focused field stays visible); g/G first/last field; PageUp/Down
    /// jump by 10 fields; `y` yanks the focused value; Enter zooms into
    /// the focused field (`Mode::CellDetail`); Esc/q close.
    pub(super) fn on_row_detail_key(&mut self, key: KeyEvent) {
        let last = self.row_detail.field_count.saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                self.row_detail.scroll = 0;
            }
            KeyCode::Enter => self.open_cell_detail(),
            KeyCode::Char('j') | KeyCode::Down => {
                self.row_detail.field = (self.row_detail.field + 1).min(last);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.row_detail.field = self.row_detail.field.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.row_detail.field = 0,
            KeyCode::Char('G') | KeyCode::End => self.row_detail.field = last,
            KeyCode::PageDown => {
                self.row_detail.field = (self.row_detail.field + 10).min(last);
            }
            KeyCode::PageUp => {
                self.row_detail.field = self.row_detail.field.saturating_sub(10);
            }
            KeyCode::Char('y') => self.yank_focused_field(),
            _ => {}
        }
    }

    /// Cell-detail modal. Two key maps depending on whether the cell
    /// parses as a JSON container:
    ///   - JSON view: j/k move the tree cursor, Enter / Space / h / l
    ///     toggle collapse on the focused container, `y` yanks the
    ///     jq-style path of the focused node.
    ///   - Text view: j/k scroll the wrapped value, `y` yanks the
    ///     whole value. Same shortcut, different semantics.
    /// Esc/q always pops back to the row view.
    pub(super) fn on_cell_detail_key(&mut self, key: KeyEvent) {
        if !self.cell_detail.json_rows.is_empty() {
            self.on_cell_detail_json_key(key);
        } else {
            self.on_cell_detail_text_key(key);
        }
    }

    pub(super) fn on_cell_detail_text_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                self.mode = Mode::RowDetail;
                self.cell_detail.scroll = 0;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.cell_detail.scroll = self
                    .cell_detail
                    .scroll
                    .saturating_add(1)
                    .min(self.cell_detail.max_scroll);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.cell_detail.scroll = self.cell_detail.scroll.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.cell_detail.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => {
                self.cell_detail.scroll = self.cell_detail.max_scroll;
            }
            KeyCode::PageDown => {
                self.cell_detail.scroll = self
                    .cell_detail
                    .scroll
                    .saturating_add(10)
                    .min(self.cell_detail.max_scroll);
            }
            KeyCode::PageUp => {
                self.cell_detail.scroll = self.cell_detail.scroll.saturating_sub(10);
            }
            KeyCode::Char('y') => self.yank_focused_field(),
            _ => {}
        }
    }

    pub(super) fn on_cell_detail_json_key(&mut self, key: KeyEvent) {
        let last = self.cell_detail.json_rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::RowDetail;
                self.cell_detail.scroll = 0;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.cell_detail.json_cursor = (self.cell_detail.json_cursor + 1).min(last);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.cell_detail.json_cursor = self.cell_detail.json_cursor.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.cell_detail.json_cursor = 0,
            KeyCode::Char('G') | KeyCode::End => self.cell_detail.json_cursor = last,
            KeyCode::PageDown => {
                self.cell_detail.json_cursor = (self.cell_detail.json_cursor + 10).min(last);
            }
            KeyCode::PageUp => {
                self.cell_detail.json_cursor = self.cell_detail.json_cursor.saturating_sub(10);
            }
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('h') | KeyCode::Char('l') => {
                self.toggle_json_cell_node()
            }
            KeyCode::Char('y') => self.yank_json_cell_path(),
            _ => {}
        }
    }

    /// Connection picker (startup): j/k navigate, Enter selects + connects,
    /// Esc/q quits since there's nothing else to do without a connection.
    pub(super) fn on_conn_pick_key(&mut self, key: KeyEvent) {
        match key.code {
            // q (and Ctrl-C) quit; Esc is a no-op so a reflex press
            // can't abandon the picker by accident.
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.conn_pick.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.conn_pick.select_prev(),
            KeyCode::Char('g') | KeyCode::Home => self.conn_pick.select_first(),
            KeyCode::Char('G') | KeyCode::End => self.conn_pick.select_last(),
            KeyCode::Enter => {
                if let Some(pick) = self.conn_pick.picks.get(self.conn_pick.index) {
                    let dsn = pick.dsn.clone();
                    // Re-resolve safety profile against the *picked* db name
                    // — the placeholder in App::new used the empty default.
                    let profile = self.safety_config.profile_for(&dsn.dbname);
                    self.read_only = profile.read_only;
                    self.statement_timeout_ms = profile.statement_timeout_ms;
                    self.dsn = Some(dsn);
                    self.dsn_origin = Some(format!(
                        "picked {} data source '{}'",
                        pick.origin, pick.name
                    ));
                    self.mode = Mode::Normal;
                    self.start_connect();
                }
            }
            _ => {}
        }
    }

    pub(super) fn on_help_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q' | '?') | KeyCode::Esc | KeyCode::F(1) => {
                // Restore the mode the operator was in when they
                // opened help. Legacy `?`-from-Normal (no origin
                // captured) falls back to Normal.
                self.mode = self.help.origin.take().unwrap_or(Mode::Normal);
                self.help.scroll = 0;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.help.scroll = self.help.scroll.saturating_add(1).min(self.help.max_scroll);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.help.scroll = self.help.scroll.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.help.scroll = 0;
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.help.scroll = self.help.max_scroll;
            }
            KeyCode::PageDown => {
                self.help.scroll = self
                    .help
                    .scroll
                    .saturating_add(10)
                    .min(self.help.max_scroll);
            }
            KeyCode::PageUp => {
                self.help.scroll = self.help.scroll.saturating_sub(10);
            }
            _ => {}
        }
    }

    pub(super) fn on_normal_key(&mut self, key: KeyEvent) {
        // Vim-style bookmarks: `m<a-z>` sets, `'<a-z>` jumps. The
        // FIRST keypress sets a pending flag; the NEXT keypress
        // is interpreted as the bookmark letter. If the next key
        // isn't an a-z, the pending flag clears silently — easy
        // to bail out of a misfired `m`.
        if self.pending_mark_set {
            self.pending_mark_set = false;
            if let KeyCode::Char(c) = key.code {
                if c.is_ascii_lowercase() {
                    let row = self.selected_grid_row_idx().unwrap_or(0);
                    self.bookmarks.insert(
                        c,
                        GridBookmark {
                            row,
                            col: self.grid_view.col_cursor,
                        },
                    );
                    self.last_status = Some(format!("bookmark '{c}' set"));
                    return;
                }
            }
            self.last_status = Some("bookmark cancelled (letter expected)".into());
            return;
        }
        if self.pending_mark_jump {
            self.pending_mark_jump = false;
            if let KeyCode::Char(c) = key.code {
                if let Some(bm) = self.bookmarks.get(&c).copied() {
                    self.jump_to_bookmark(bm);
                    self.last_status = Some(format!("jumped to '{c}'"));
                    return;
                }
                self.last_status = Some(format!("no bookmark at '{c}'"));
                return;
            }
            self.last_status = Some("jump cancelled (letter expected)".into());
            return;
        }
        // Failure-screen shortcuts — only active while we're showing the
        // "connection failed" body. `r` retries the same DSN; `p` re-opens
        // the picker when we have data sources to choose from.
        if matches!(self.conn_state, ConnState::Failed(_)) {
            match key.code {
                KeyCode::Char('r') => {
                    if self.dsn.is_some() {
                        self.start_connect();
                    }
                    return;
                }
                // Only offer "change connection" when there are at least
                // two candidates — otherwise the picker would just show
                // the same DSN that just failed, and Enter would retry it
                // (already on `r`).
                KeyCode::Char('p') if self.conn_pick.picks.len() >= 2 => {
                    self.mode = Mode::ConnPick;
                    self.conn_pick.index = 0;
                    return;
                }
                _ => {}
            }
        }
        match key.code {
            // q (and Ctrl-C) are the only quit keys. Esc used to also
            // quit, but a reflex Esc shouldn't ever lose the session —
            // overlays bind Esc to "close me", and in Normal mode Esc
            // is a no-op so an extra press from inside a closed overlay
            // is harmless.
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.open_help_from(Mode::Normal),
            KeyCode::Char('e') | KeyCode::Char('i') | KeyCode::Tab => {
                self.mode = Mode::Editor;
            }
            // `c` opens the connection picker mid-session — psql's
            // `\c` equivalent. Requires at least one discovered data
            // source to be useful; with zero we surface a status hint
            // rather than dropping into an empty picker.
            KeyCode::Char('c') => self.start_connection_change(),
            KeyCode::Char('j') | KeyCode::Down => self.scroll(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll(-1),
            KeyCode::Char('h') | KeyCode::Left => self.move_col_cursor(-1),
            KeyCode::Char('l') | KeyCode::Right => self.move_col_cursor(1),
            KeyCode::Char('s') => self.cycle_sort(),
            KeyCode::Char('Y') => self.export_grid_to_clipboard(),
            KeyCode::Char('/') => self.start_filter(),
            KeyCode::Char('f') => self.start_find(),
            KeyCode::Char('F') => self.navigate_fk_from_focused_cell(),
            KeyCode::Char('n') => self.filter_step(true),
            KeyCode::Char('N') => self.filter_step(false),
            KeyCode::Char('g') | KeyCode::Home => self.select_row(0),
            KeyCode::Char('G') | KeyCode::End => {
                self.select_row(self.grid.row_count().saturating_sub(1));
            }
            KeyCode::Enter => self.open_row_detail(),
            KeyCode::Char('A') => self.mode = Mode::About,
            KeyCode::Char('S') => self.start_schema_browser(),
            KeyCode::Char('T') => self.start_slow_queries(),
            KeyCode::Char('L') => self.start_sessions(),
            KeyCode::Char('W') => self.start_schema_lint(),
            KeyCode::Char('Q') => self.open_saved_queries(),
            KeyCode::Char('D') => self.pin_or_diff_result(),
            KeyCode::Char('I') => self.yank_row_as_insert(),
            KeyCode::Char('m') => {
                self.pending_mark_set = true;
                self.last_status = Some("set mark · press a-z".into());
            }
            KeyCode::Char('\'') => {
                self.pending_mark_jump = true;
                self.last_status = Some("jump to mark · press a-z".into());
            }
            _ => {}
        }
    }

    pub(super) fn on_editor_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        // Undo / redo shortcuts. Must run BEFORE the inner handler
        // so Ctrl-Z doesn't fall through to the char-insert arm and
        // type a literal 'z'. Ctrl-Y is the Windows-style redo;
        // Ctrl-Shift-Z is the mac/Emacs-style redo.
        if ctrl && matches!(key.code, KeyCode::Char('z')) && !shift {
            self.editor_undo();
            return;
        }
        if ctrl
            && (matches!(key.code, KeyCode::Char('y'))
                || (matches!(key.code, KeyCode::Char('z')) && shift))
        {
            self.editor_redo();
            return;
        }
        // Snapshot the pre-mutation state so we can push it to the
        // undo ring AFTER the inner handler runs, if (and only if)
        // the buffer actually changed.
        let pre_buf = self.editor.buffer.clone();
        let pre_cur = self.editor.cursor;
        let kind = match key.code {
            KeyCode::Char(_)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                EditorActionKind::CharInsert
            }
            _ => EditorActionKind::Other,
        };
        self.on_editor_key_inner(key);
        if self.editor.buffer != pre_buf {
            self.push_undo(pre_buf, pre_cur, kind);
        }
    }

    /// Handle a key while in Mode::HistorySearch. Char/Backspace edit
    /// the query and re-search from the latest match. Ctrl-R jumps to
    /// the next-older match. Enter accepts (stays in Editor with the
    /// matched buffer). Esc cancels (restores the snapshot).
    pub(super) fn on_history_search_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                if let Some(state) = self.history_search.take() {
                    self.editor.buffer = state.saved_buffer;
                    self.editor.cursor = state.saved_cursor;
                }
                self.last_status = None;
                self.mode = Mode::Editor;
            }
            KeyCode::Enter => {
                // Accept: keep whatever's in the buffer (the matched
                // history entry) and exit back to Editor.
                self.history_search = None;
                self.last_status = None;
                self.mode = Mode::Editor;
            }
            KeyCode::Char('r') if ctrl => {
                // Jump to the next-older match. Start from the
                // CURRENT match's index (exclusive) so we move
                // backward through history.
                let from = self.history_search.as_ref().and_then(|s| s.matched);
                self.history_search_step(from);
                self.refresh_history_search_status();
            }
            KeyCode::Char('d') if ctrl => {
                // Delete the currently-matched history entry —
                // useful after pasting a query with inline
                // secrets, then re-step the search so the next
                // match (or "no match") surfaces.
                let matched = self.history_search.as_ref().and_then(|s| s.matched);
                if let Some(idx) = matched {
                    if idx < self.history.len() {
                        self.history.remove(idx);
                    }
                    if let Some(state) = self.history_search.as_mut() {
                        state.matched = None;
                    }
                    // Re-search from the END of history so the
                    // step finds whatever's left.
                    self.history_search_step(None);
                    self.refresh_history_search_status();
                    self.last_status = Some(format!(
                        "history entry deleted · {}",
                        self.last_status
                            .as_deref()
                            .unwrap_or("(no remaining match)")
                    ));
                }
            }
            KeyCode::Backspace => {
                if let Some(state) = self.history_search.as_mut() {
                    state.query.pop();
                }
                self.history_search_step(None);
                self.refresh_history_search_status();
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                if let Some(state) = self.history_search.as_mut() {
                    state.query.push(c);
                }
                self.history_search_step(None);
                self.refresh_history_search_status();
            }
            _ => {}
        }
    }

    pub(super) fn on_confirm_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(pending) = self.pending_run.take() {
                    self.spawn_run(
                        pending.sql,
                        pending.kind,
                        pending.decision,
                        pending.is_batch,
                    );
                }
                self.mode = Mode::Editor;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.pending_run = None;
                self.mode = Mode::Editor;
                self.last_status = Some("cancelled".to_string());
            }
            _ => {}
        }
    }

    /// Tx-open prompt: `y` commits, `n` / `esc` rolls back.
    pub(super) fn on_tx_decision_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => self.close_tx(true),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.close_tx(false),
            _ => {}
        }
    }

    /// Log-pick browser: j/k navigate, Enter loads the selection into the
    /// editor, Esc cancels, `c` toggles cluster view.
    pub(super) fn on_log_pick_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.log_pick.picks.clear();
                self.log_pick.clusters.clear();
                self.mode = Mode::Editor;
            }
            KeyCode::Char('c') => self.toggle_log_pick_view(),
            KeyCode::Char('j') | KeyCode::Down => self.log_pick.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.log_pick.select_prev(),
            KeyCode::Char('g') | KeyCode::Home => self.log_pick.select_first(),
            KeyCode::Char('G') | KeyCode::End => self.log_pick.select_last(),
            KeyCode::Enter => {
                if let Some(sql) = self.focused_log_pick_sql() {
                    self.editor.buffer = sql;
                    self.editor.cursor = self.editor.buffer.len();
                    self.editor.preferred_col = None;
                    self.history_pos = None;
                    self.last_status = Some(format!(
                        "loaded query · {} char(s)",
                        self.editor.buffer.len()
                    ));
                }
                self.log_pick.picks.clear();
                self.log_pick.clusters.clear();
                self.mode = Mode::Editor;
            }
            _ => {}
        }
    }

    pub(super) fn on_result_diff_key(&mut self, key: KeyEvent) {
        let last = self
            .result_diff
            .active
            .as_ref()
            .map(|d| diff_row_count(&d.diff).saturating_sub(1))
            .unwrap_or(0);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                self.last_status = None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.result_diff.cursor = (self.result_diff.cursor + 1).min(last);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.result_diff.cursor = self.result_diff.cursor.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.result_diff.cursor = 0,
            KeyCode::Char('G') | KeyCode::End => self.result_diff.cursor = last,
            KeyCode::PageDown => {
                self.result_diff.cursor = (self.result_diff.cursor + 10).min(last);
            }
            KeyCode::PageUp => {
                self.result_diff.cursor = self.result_diff.cursor.saturating_sub(10);
            }
            // `r` re-pins the B side as the new baseline A, so the
            // operator can iterate: tweak → run → D → r → repeat.
            KeyCode::Char('r') => {
                if let Some(d) = self.result_diff.active.as_ref() {
                    self.result_diff.pinned = Some(PinnedResult {
                        columns: d.b_columns.clone(),
                        rows: d.b_rows.clone(),
                        label: d.b_label.clone(),
                    });
                    self.mode = Mode::Normal;
                    self.result_diff.active = None;
                    self.last_status = Some("re-pinned current result as A".into());
                }
            }
            // `c` clears the pinned baseline entirely.
            KeyCode::Char('c') => {
                self.result_diff.pinned = None;
                self.result_diff.active = None;
                self.mode = Mode::Normal;
                self.last_status = Some("cleared pinned result".into());
            }
            _ => {}
        }
    }

    pub(super) fn on_schema_lint_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                self.last_status = None;
            }
            KeyCode::Char('j') | KeyCode::Down => self.schema_lint.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.schema_lint.select_prev(),
            KeyCode::Char('g') | KeyCode::Home => self.schema_lint.select_first(),
            KeyCode::Char('G') | KeyCode::End => self.schema_lint.select_last(),
            KeyCode::PageDown => self.schema_lint.page_down(),
            KeyCode::PageUp => self.schema_lint.page_up(),
            KeyCode::Char('y') => self.yank_schema_lint_suggestion(),
            KeyCode::Char('r') => self.start_schema_lint(),
            _ => {}
        }
    }

    pub(super) fn on_schema_browser_key(&mut self, key: KeyEvent) {
        let rows = self.flattened_schema_browser();
        let last = rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                // Drop the in-tree filter on the way out so re-opening
                // the browser shows the full tree, not a stale
                // narrowed view.
                self.schema_browser.filter = None;
                self.mode = Mode::Normal;
                self.last_status = None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.schema_browser.cursor = (self.schema_browser.cursor + 1).min(last);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.schema_browser.cursor = self.schema_browser.cursor.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.schema_browser.cursor = 0,
            KeyCode::Char('G') | KeyCode::End => self.schema_browser.cursor = last,
            KeyCode::PageDown => {
                self.schema_browser.cursor = (self.schema_browser.cursor + 10).min(last);
            }
            KeyCode::PageUp => {
                self.schema_browser.cursor = self.schema_browser.cursor.saturating_sub(10);
            }
            KeyCode::Char(']') => {
                // Jump to the next Schema-level row; useful for
                // walking past a fully-expanded table's column
                // list in one keypress.
                if let Some(idx) =
                    next_schema_row_idx(&rows, self.schema_browser.cursor, Direction::Forward)
                {
                    self.schema_browser.cursor = idx;
                }
            }
            KeyCode::Char('[') => {
                if let Some(idx) =
                    next_schema_row_idx(&rows, self.schema_browser.cursor, Direction::Backward)
                {
                    self.schema_browser.cursor = idx;
                }
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                // `+` (and the unshifted `=` alias) — expand every
                // schema AND table in the cache. Cursor stays put.
                for t in &self.schema_cache.tables {
                    self.schema_browser.expanded.insert(t.schema.clone());
                    self.schema_browser
                        .expanded
                        .insert(schema_browser_table_key(&t.schema, &t.name));
                }
                self.last_status = Some("expanded all".into());
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                // `-` collapse all. Cursor clamps to the new last
                // row because the visible-row count crashes.
                self.schema_browser.expanded.clear();
                let new_last = self.flattened_schema_browser().len().saturating_sub(1);
                if self.schema_browser.cursor > new_last {
                    self.schema_browser.cursor = new_last;
                }
                self.last_status = Some("collapsed all".into());
            }
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Right | KeyCode::Left => {
                // Toggle the focused node's expanded state. Schema rows
                // key on `"schema"`; table rows key on `"schema.table"`.
                // Column / Constraint rows are leaves — no-op.
                match rows.get(self.schema_browser.cursor) {
                    Some(SchemaBrowserRow::Schema { name, expanded, .. }) => {
                        let name = name.clone();
                        if *expanded {
                            self.schema_browser.expanded.remove(&name);
                        } else {
                            self.schema_browser.expanded.insert(name);
                        }
                    }
                    Some(SchemaBrowserRow::Table {
                        schema,
                        name,
                        expanded,
                        ..
                    }) => {
                        let key = schema_browser_table_key(schema, name);
                        if *expanded {
                            self.schema_browser.expanded.remove(&key);
                        } else {
                            self.schema_browser.expanded.insert(key);
                        }
                    }
                    _ => {}
                }
                // Collapse shrinks the row list; re-clamp so the
                // cursor doesn't render out-of-range until the next
                // j/k press.
                let new_last = self.flattened_schema_browser().len().saturating_sub(1);
                if self.schema_browser.cursor > new_last {
                    self.schema_browser.cursor = new_last;
                }
            }
            KeyCode::Char('s') => self.yank_schema_browser_select(),
            KeyCode::Char('i') => self.yank_schema_browser_insert(),
            KeyCode::Char('/') => self.start_schema_browser_filter(),
            _ => {}
        }
    }

    pub(super) fn on_schema_browser_filter_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.schema_browser.filter = None;
                self.schema_browser.cursor = 0;
                self.last_status = Some("filter cleared".into());
                self.mode = Mode::SchemaBrowser;
            }
            KeyCode::Enter => {
                // Accept: keep whatever's in the filter and pop back
                // to SchemaBrowser navigation. An empty filter
                // collapses to None (no filter applied).
                if matches!(self.schema_browser.filter.as_deref(), Some("")) {
                    self.schema_browser.filter = None;
                }
                self.last_status = None;
                self.mode = Mode::SchemaBrowser;
            }
            KeyCode::Backspace => {
                if let Some(f) = self.schema_browser.filter.as_mut() {
                    f.pop();
                }
                self.schema_browser.cursor = 0;
                self.refresh_schema_browser_filter_status();
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                if let Some(f) = self.schema_browser.filter.as_mut() {
                    f.push(c);
                }
                self.schema_browser.cursor = 0;
                self.refresh_schema_browser_filter_status();
            }
            _ => {}
        }
    }

    pub(super) fn on_slow_queries_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                self.last_status = None;
            }
            KeyCode::Char('j') | KeyCode::Down => self.slow_queries.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.slow_queries.select_prev(),
            KeyCode::Char('g') | KeyCode::Home => self.slow_queries.select_first(),
            KeyCode::Char('G') | KeyCode::End => self.slow_queries.select_last(),
            KeyCode::Char('r') => self.refresh_slow_queries(),
            KeyCode::Char('R') => self.toggle_auto_refresh(),
            KeyCode::Enter => {
                // Copy the focused query into the editor for tuning,
                // then exit back to the editor. Empty when the
                // panel is empty.
                if let Some(row) = self.slow_queries.rows.get(self.slow_queries.cursor) {
                    self.editor.buffer = row.query.clone();
                    self.editor.cursor = self.editor.buffer.len();
                    self.editor.preferred_col = None;
                    self.draft_dirty = true;
                    self.mode = Mode::Editor;
                    self.last_status = Some(format!(
                        "loaded slow query · {} char(s)",
                        self.editor.buffer.len()
                    ));
                }
            }
            _ => {}
        }
    }

    pub(super) fn on_sessions_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                self.last_status = None;
            }
            KeyCode::Char('j') | KeyCode::Down => self.sessions.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.sessions.select_prev(),
            KeyCode::Char('g') | KeyCode::Home => self.sessions.select_first(),
            KeyCode::Char('G') | KeyCode::End => self.sessions.select_last(),
            KeyCode::Char('r') => self.refresh_sessions(),
            KeyCode::Char('R') => self.toggle_auto_refresh(),
            KeyCode::Char('K') => self.start_terminate_focused_session(),
            _ => {}
        }
    }

    pub(super) fn on_explain_tree_key(&mut self, key: KeyEvent) {
        let rows = self.flattened_explain_rows();
        let last = rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                self.last_status = None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.explain.cursor = (self.explain.cursor + 1).min(last);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.explain.cursor = self.explain.cursor.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.explain.cursor = 0,
            KeyCode::Char('G') | KeyCode::End => self.explain.cursor = last,
            KeyCode::Enter | KeyCode::Char(' ') => {
                // Toggle collapse on the focused node, IF it has
                // children. Leaf nodes stay open (collapsing them
                // would just hide the line they're on).
                if let Some(row) = rows.get(self.explain.cursor) {
                    if row.has_children && !self.explain.collapsed.remove(&row.path) {
                        self.explain.collapsed.insert(row.path.clone());
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn on_grid_find_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.grid_find.needle = None;
                self.grid_find.matches.clear();
                self.last_status = Some("find cleared".into());
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                // Accept — stay on the current match; clear the
                // input but keep the matches for `n`/`N` from
                // Normal mode? For v1, just exit — operator can
                // re-press `f` to keep stepping.
                self.grid_find.needle = None;
                self.grid_find.matches.clear();
                self.last_status = None;
                self.mode = Mode::Normal;
            }
            // n / N work while typing too — they don't conflict
            // with text since most find patterns aren't bare n/N.
            // But for safety we only treat them as step keys when
            // they're ALONE in the buffer (else, treat as a char
            // and extend the pattern). Simpler: always step on
            // n/N, and the operator can use Backspace if they
            // typed it by mistake. This matches vim's behaviour.
            KeyCode::Char('n') if !ctrl => self.step_grid_find(true),
            KeyCode::Char('N') if !ctrl => self.step_grid_find(false),
            KeyCode::Backspace => {
                if let Some(f) = self.grid_find.needle.as_mut() {
                    f.pop();
                }
                self.rebuild_grid_find();
                self.refresh_grid_find_status();
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                if let Some(f) = self.grid_find.needle.as_mut() {
                    f.push(c);
                }
                self.rebuild_grid_find();
                self.refresh_grid_find_status();
            }
            _ => {}
        }
    }

    pub(super) fn on_grid_filter_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.grid_view.filter = None;
                self.rebuild_visible_rows();
                self.last_status = Some("filter cleared".into());
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                self.last_status = None;
            }
            KeyCode::Backspace => {
                if let Some(f) = self.grid_view.filter.as_mut() {
                    f.pop();
                }
                self.rebuild_visible_rows();
                self.refresh_filter_status();
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                if let Some(f) = self.grid_view.filter.as_mut() {
                    f.push(c);
                }
                self.rebuild_visible_rows();
                self.refresh_filter_status();
            }
            _ => {}
        }
    }
}
