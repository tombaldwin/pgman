use super::*;

impl App {
    /// Bracketed paste: terminal delivered the entire pasted blob in one
    /// event. Only meaningful in the editor (the only typing surface);
    /// elsewhere we ignore so a stray paste on the grid doesn't trigger
    /// arbitrary keypress side effects.
    pub(super) fn on_paste(&mut self, text: String) {
        if self.mode != Mode::Editor {
            return;
        }
        // Splash, if still visible, was waiting on a key — dismiss it
        // so the paste lands on the actual editor surface, not an empty
        // pre-app frame.
        self.splash_visible = false;
        self.splash_until = None;
        // Drop any active completion cycle — a paste mid-cycle is a hard
        // commit / reset boundary.
        self.completion = None;
        // Pasted text is inserted verbatim and no closer in it is
        // ours to skip over.
        self.auto_closers.clear();
        self.editor_dirty();
        // Normalise line endings to LF: most terminals deliver CRLF on
        // Windows or `\r` from old-Mac sources. Don't collapse blank
        // lines — the operator pasted them deliberately.
        let cleaned = text.replace("\r\n", "\n").replace('\r', "\n");
        // Bulk insert (O(N)) — looping editor_insert char-by-char is
        // O(N²) because each `String::insert(idx, c)` shifts the tail
        // of the buffer. A 5MB schema-diff paste froze the UI for
        // multiple seconds; insert_str makes it instant.
        self.editor.buffer.insert_str(self.editor.cursor, &cleaned);
        self.editor.cursor += cleaned.len();
        // Tell the user a pasted log is exactly the way in: F8 / ctrl-l
        // (`start_log_import`) turn it into runnable queries. Scoped to the
        // pasted text, not the whole buffer — cheap even for a large paste,
        // and it's the paste that prompted the hint.
        if let Some(kind) = crate::query::logdetect::detect_log(&cleaned) {
            self.last_status = Some(format!(
                "looks like a {} log · ctrl-l / F8 to reconstruct queries",
                kind.label()
            ));
        }
    }

    /// Inner editor-key handler. Wrapper above adds undo/redo
    /// snapshotting around it; this body holds the original key
    /// dispatch.
    pub(super) fn on_editor_key_inner(&mut self, key: KeyEvent) {
        // Tab drives identifier completion — it's the only key that
        // reads the active cycle, so handle it before the universal
        // "non-Tab key cancels the cycle" reset below.
        if matches!(key.code, KeyCode::Tab)
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            self.editor_complete();
            return;
        }
        // Ctrl-Space is the industry-standard alias — IDEs and most
        // shells bind it to "open the completion popup". Same handler
        // as Tab; gives muscle-memory users a familiar shortcut without
        // pre-empting Tab's role as the indent / fast-cycle key.
        if matches!(key.code, KeyCode::Char(' ')) && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.editor_complete();
            return;
        }
        // Esc with an active cycle abandons completion *without* leaving
        // editor mode — restores the originally-typed prefix so the user
        // can keep typing. Without an active cycle, Esc still exits to
        // Normal (the existing behaviour) via the match below.
        if matches!(key.code, KeyCode::Esc) && self.completion.is_some() {
            self.editor_abandon_completion();
            return;
        }
        // Enter with the popup up accepts a candidate — the highlighted
        // one, or the first when nothing is highlighted yet — and
        // dismisses the popup. It never reaches the run-or-newline rule
        // below on the same press: the operator was answering the
        // popup, not the buffer.
        if matches!(key.code, KeyCode::Enter)
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && self.completion.is_some()
        {
            self.editor_accept_completion();
            return;
        }
        // While a completion popup is up in pre-selection state (LCP
        // expanded / popup-only, nothing committed via Tab yet),
        // narrowing keys — plain char insertion, Backspace, Delete —
        // should keep the popup live and re-narrow the candidate list
        // instead of clearing the cycle. Any other key (Enter, arrow
        // keys, Ctrl-*, etc.) drops the cycle as before.
        let was_pre_selected = self
            .completion
            .as_ref()
            .map(|c| c.selected.is_none())
            .unwrap_or(false);
        let is_narrowing_key = match key.code {
            KeyCode::Char(_) => !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT),
            KeyCode::Backspace | KeyCode::Delete => true,
            _ => false,
        };
        let preserve_cycle = was_pre_selected && is_narrowing_key;
        if !preserve_cycle {
            // Existing: clear cycle on any non-narrowing key. Also wipe
            // a stale `completion N/M …` status the footer was showing.
            if self.completion.is_some() {
                if let Some(s) = &self.last_status {
                    if s.starts_with("completion") {
                        self.last_status = None;
                    }
                }
            }
            self.completion = None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            // Run keys. Enter runs a *terminated* statement (see
            // `enter_runs`) and otherwise inserts a newline; Alt-Enter
            // runs regardless. F5 / Ctrl-Enter / Ctrl-J are aliases —
            // F-keys on a MacBook need fn+, and Ctrl-Enter is
            // terminal-dependent. Ctrl-R is reverse-incremental history
            // search (matches bash / readline / psql convention).
            KeyCode::Char('r') if ctrl => self.start_history_search(),
            KeyCode::Char('e') if ctrl => self.request_run(RunKind::Explain),
            KeyCode::Char('a') if ctrl => self.request_run(RunKind::ExplainAnalyze),
            KeyCode::Char('l') if ctrl => self.start_log_import(),
            KeyCode::Char('d') if ctrl => self.load_dbunit_fixture(),
            // Ctrl-W → start a \watch session against the editor's
            // current buffer (or, if it's empty, the most recent
            // history entry). Suppressed mid-query and during an
            // open auto_tx — watch would otherwise pile up runs on
            // a paused session.
            KeyCode::Char('w') if ctrl => self.start_watch(),
            // Ctrl-F → pretty-print the buffer: `pg_format` when it
            // is on PATH, the built-in formatter otherwise. The only
            // key that formats — never run, never paste.
            KeyCode::Char('f') if ctrl => self.reformat_buffer(),
            // Ctrl-X → `\e` external editor. Sets a flag so the main
            // `run()` loop can do the suspend / spawn / resume dance
            // (which needs `&mut Tui`).
            KeyCode::Char('x') if ctrl => self.external_edit_pending = true,
            // Ctrl-S — prompt for a name and persist the editor
            // buffer as a saved query.
            KeyCode::Char('s') if ctrl => self.start_save_query_prompt(),
            // Ctrl-O — open the saved-queries panel for loading.
            KeyCode::Char('o') if ctrl => self.open_saved_queries(),
            // Ctrl-/ — toggle a `-- ` line comment on the
            // current line. Some terminals deliver this as
            // Char('/') with CONTROL, others as Char('_') (the
            // ASCII control code for /) — accept either.
            KeyCode::Char('/') | KeyCode::Char('_') if ctrl => {
                self.editor_dirty();
                editor_toggle_line_comment(&mut self.editor.buffer, &mut self.editor.cursor);
            }
            // Alt-Enter runs whatever is in the buffer, terminated or
            // not — every terminal delivers Alt as an ESC prefix, so it
            // works where Ctrl-Enter does not (pgcli's Meta-Enter). Some
            // terminals report Ctrl-Enter; others fold it into Ctrl-J.
            // All three run.
            KeyCode::Enter if ctrl || alt => self.request_run(RunKind::Run),
            KeyCode::Char('j') if ctrl => self.request_run(RunKind::Run),
            // Ctrl-C while a query is in flight sends a PostgreSQL
            // CancelRequest to the same backend. No-op otherwise (we
            // run in raw mode so Ctrl-C doesn't quit).
            KeyCode::Char('c') if ctrl && self.query_running => self.cancel_running_query(),
            KeyCode::F(5) => self.request_run(RunKind::Run),
            KeyCode::F(6) => self.request_run(RunKind::Explain),
            KeyCode::F(7) => self.request_run(RunKind::ExplainAnalyze),
            KeyCode::F(8) => self.start_log_import(),
            KeyCode::F(9) => self.load_dbunit_fixture(),

            // History navigation.
            KeyCode::Char('p') if ctrl => self.history_prev(),
            KeyCode::Char('n') if ctrl => self.history_next(),
            KeyCode::Char('u') if ctrl => {
                self.editor.buffer.clear();
                self.editor.cursor = 0;
                self.editor_dirty();
            }

            // Plain typing — only when no Ctrl/Alt. Includes
            // bracket autoclose: `(` / `[` / `{` insert a pair
            // with the cursor between; `)` / `]` / `}` skip over
            // a matching close immediately after the cursor so
            // typing `(` then `)` exits the pair cleanly. Quote
            // autoclose (`'` / `"`) follows the same shape but
            // with a conservative neighbour-check so it doesn't
            // interfere with SQL `''` escaping or in-word
            // apostrophes.
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.editor_dirty();
                let at = self.editor.cursor;
                if let Some(close) = closer_of(c) {
                    editor_insert_pair(&mut self.editor.buffer, &mut self.editor.cursor, c);
                    self.auto_closers.shift_insert(at, 2);
                    self.auto_closers.push(at + 1, close);
                } else if matches!(c, ')' | ']' | '}' | '\'' | '"')
                    && self.auto_closers.take_at(at, c, &self.editor.buffer)
                {
                    // Skipped over a closer *we* inserted at exactly this
                    // offset. A closer the operator typed — or one that
                    // was here before a paste, an undo, a completion —
                    // is never skipped: that was how `'shipped'` became
                    // `'shipped''`.
                    self.editor.cursor += c.len_utf8();
                } else if matches!(c, '\'' | '"')
                    && editor_maybe_pair_quote(&mut self.editor.buffer, &mut self.editor.cursor, c)
                {
                    self.auto_closers.shift_insert(at, 2);
                    self.auto_closers.push(at + 1, c);
                } else {
                    editor_insert(&mut self.editor.buffer, &mut self.editor.cursor, c);
                    self.auto_closers.shift_insert(at, c.len_utf8());
                }
            }
            // Enter on a terminated statement (`… ;`) or a backslash
            // command runs it — the psql reflex, and the only run key a
            // MacBook keyboard delivers without a modifier or fn+. An
            // unterminated statement gets a newline instead, as does
            // Shift-Enter where a terminal distinguishes it.
            KeyCode::Enter if !shift && enter_runs(&self.editor.buffer) => {
                self.request_run(RunKind::Run)
            }
            KeyCode::Enter => {
                self.editor_dirty();
                let at = self.editor.cursor;
                editor_insert(&mut self.editor.buffer, &mut self.editor.cursor, '\n');
                self.auto_closers.shift_insert(at, 1);
            }
            KeyCode::Backspace => {
                self.editor_dirty();
                let end = self.editor.cursor;
                editor_backspace(&mut self.editor.buffer, &mut self.editor.cursor);
                self.auto_closers.shift_delete(self.editor.cursor, end);
            }
            KeyCode::Delete => {
                self.editor_dirty();
                let before = self.editor.buffer.len();
                editor_delete(&mut self.editor.buffer, &mut self.editor.cursor);
                let removed = before - self.editor.buffer.len();
                self.auto_closers
                    .shift_delete(self.editor.cursor, self.editor.cursor + removed);
            }
            KeyCode::Left => {
                self.editor.preferred_col = None;
                editor_move_left(&self.editor.buffer, &mut self.editor.cursor);
            }
            KeyCode::Right => {
                self.editor.preferred_col = None;
                editor_move_right(&self.editor.buffer, &mut self.editor.cursor);
            }
            KeyCode::Up => {
                editor_move_up(
                    &self.editor.buffer,
                    &mut self.editor.cursor,
                    &mut self.editor.preferred_col,
                );
            }
            KeyCode::Down => {
                editor_move_down(
                    &self.editor.buffer,
                    &mut self.editor.cursor,
                    &mut self.editor.preferred_col,
                );
            }
            KeyCode::Home => {
                self.editor.preferred_col = None;
                self.editor.cursor = line_start_byte(&self.editor.buffer, self.editor.cursor);
            }
            KeyCode::End => {
                self.editor.preferred_col = None;
                self.editor.cursor = line_end_byte(&self.editor.buffer, self.editor.cursor);
            }
            _ => {}
        }
        // If we kept the cycle alive across a narrowing key, recompute
        // the candidate set against the new buffer state so the popup
        // reflects what's now matching.
        if preserve_cycle {
            self.refresh_completion();
        }

        // Auto-trigger completion when the operator just typed `.` after
        // an identifier (e.g. `users.|` or `u.|`). Modern editors do
        // this to save a Tab keystroke for the common qualified-access
        // case. Suppressed when:
        //   - a cycle is already alive (refresh_completion handled it),
        //   - the char before the `.` isn't alphabetic / `_` (so we
        //     don't fire on `3.14`-style numeric literals),
        //   - completion fails (no schema cache, no matches) — the
        //     status message is restored so we don't yell at the user
        //     for typing `.` in normal text.
        let just_typed_dot = matches!(key.code, KeyCode::Char('.'))
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        // A pasted log is not SQL being typed: every `.` in a logger
        // name (`org.hibernate.SQL`) and every `FROM ` inside a logged
        // statement would pop the completion popup, once per keystroke,
        // over text the operator is about to hand to `ctrl-l` anyway.
        // Tab still completes on demand.
        if just_typed_dot && self.completion.is_none() && self.editor_log_kind().is_none() {
            // The `.` is at the byte position immediately before the
            // cursor. Walk back ONE char (not one byte) so identifiers
            // ending in non-ASCII letters — `café.`, `naïve.`,
            // quoted-name-style `"My Table".` once we support those —
            // still trigger. Reading `bytes[dot_byte - 1]` would catch
            // only ASCII suffixes.
            let dot_byte = self.editor.cursor.saturating_sub(1);
            let prev_char = self.editor.buffer[..dot_byte].chars().next_back();
            if matches!(prev_char, Some(c) if c.is_alphabetic() || c == '_') {
                let saved_status = self.last_status.clone();
                self.editor_complete();
                if self.completion.is_none() {
                    self.last_status = saved_status;
                }
            }
        }

        // Auto-trigger completion when the operator just typed a space
        // immediately after an identifier-introducing keyword. Keeps
        // the list of trigger keywords short and conservative so we
        // only fire where the popup is unambiguously useful — typing
        // `FROM <Tab>` saves one keystroke, but firing on every space
        // in `WHERE x = 5 ` would be noise. Skipped when a cycle is
        // already alive (which means `refresh_completion` is handling
        // the keystroke).
        const TRIGGER_KEYWORDS: &[&str] = &[
            "FROM", "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "CROSS", "INTO", "WHERE", "AND",
            "OR", "ON",
        ];
        let just_typed_space = matches!(key.code, KeyCode::Char(' '))
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if just_typed_space && self.completion.is_none() && self.editor_log_kind().is_none() {
            // The just-typed space is at `editor.cursor - 1`. Strip it
            // and any further trailing whitespace, then read back the
            // last alphanumeric / `_` word. Walk char_indices in reverse
            // so a multi-byte boundary char (en-dash, smart quote, NBSP,
            // …) doesn't land us mid-codepoint — `rfind(predicate) + 1`
            // would have panicked on those.
            let before_space = &self.editor.buffer[..self.editor.cursor.saturating_sub(1)];
            let trimmed = before_space.trim_end();
            let word_start = trimmed
                .char_indices()
                .rev()
                .find(|(_, c)| !c.is_alphanumeric() && *c != '_')
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(0);
            let last_word = &trimmed[word_start..];
            if !last_word.is_empty()
                && TRIGGER_KEYWORDS
                    .iter()
                    .any(|k| k.eq_ignore_ascii_case(last_word))
            {
                let saved_status = self.last_status.clone();
                self.editor_complete();
                if self.completion.is_none() {
                    self.last_status = saved_status;
                }
            }
        }
    }

    /// The memoised "does this buffer look like a pasted log" verdict,
    /// refreshed when the buffer's [`BufferFingerprint`] has moved.
    ///
    /// One owner for the cache: the editor block title reads it to show
    /// the `ctrl-l / F8 to reconstruct` hint, and the completion
    /// auto-triggers read it to stay quiet inside a log. Only the first
    /// `logdetect::DETECT_HEAD_BYTES` are scanned — a log announces
    /// itself in its opening lines, and this runs on every edit.
    pub(crate) fn editor_log_kind(&mut self) -> Option<crate::query::logdetect::LogKind> {
        let key = BufferFingerprint::of(&self.editor.buffer);
        if let Some((cached, kind)) = &self.editor_log_kind_cache {
            if *cached == key {
                return *kind;
            }
        }
        let kind = crate::query::logdetect::detect_log(
            crate::query::logdetect::head_for_detection(&self.editor.buffer),
        );
        self.editor_log_kind_cache = Some((key, kind));
        kind
    }

    /// Any edit / non-vertical motion exits history navigation and resets
    /// preferred-column tracking.
    fn editor_dirty(&mut self) {
        self.history_pos = None;
        self.editor.preferred_col = None;
        // Mark the buffer dirty for the periodic auto-save in run().
        // We don't persist inline because editor_dirty is called
        // BEFORE the actual mutation at most call sites — the run-
        // loop's "save when stable" pass picks up the post-mutation
        // state.
        self.draft_dirty = true;
    }

    /// Pop the most recent undo entry and restore. Push the current
    /// state to the redo ring so Ctrl-Y can flip back.
    pub fn editor_undo(&mut self) {
        let Some(prev) = self.editor.undo.pop() else {
            self.last_status = Some("nothing to undo".into());
            return;
        };
        let now = std::time::Instant::now();
        self.editor.redo.push(UndoEntry {
            buffer: std::mem::take(&mut self.editor.buffer),
            cursor: self.editor.cursor,
            kind: EditorActionKind::Other,
            merge_window_end: now,
        });
        self.editor.buffer = prev.buffer;
        self.editor.cursor = prev.cursor.min(self.editor.buffer.len());
        self.editor.preferred_col = None;
        self.history_pos = None;
        self.auto_closers.clear();
        // Undo replaced the buffer wholesale; any active completion cycle's
        // byte offsets now point past the restored (shorter) buffer. Drop it
        // so the next Tab starts fresh rather than `replace_range`-ing out of
        // bounds and panicking.
        self.completion = None;
        self.draft_dirty = true;
    }

    /// Pop the most recent redo entry and restore. Push the current
    /// state to the undo ring so Ctrl-Z can flip back. Mirror of
    /// [`Self::editor_undo`].
    pub fn editor_redo(&mut self) {
        let Some(next) = self.editor.redo.pop() else {
            self.last_status = Some("nothing to redo".into());
            return;
        };
        let now = std::time::Instant::now();
        self.editor.undo.push(UndoEntry {
            buffer: std::mem::take(&mut self.editor.buffer),
            cursor: self.editor.cursor,
            kind: EditorActionKind::Other,
            merge_window_end: now,
        });
        self.editor.buffer = next.buffer;
        self.editor.cursor = next.cursor.min(self.editor.buffer.len());
        self.editor.preferred_col = None;
        self.history_pos = None;
        self.auto_closers.clear();
        // See editor_undo: a buffer swap invalidates the completion cycle's
        // stored offsets.
        self.completion = None;
        self.draft_dirty = true;
    }

    /// Abandon an active completion cycle: restore the original buffer
    /// text the cycle replaced (including any chars that trailed the
    /// cursor when Tab fired) and put the cursor back where it was when
    /// the user pressed Tab. No-op when no cycle is active.
    fn editor_abandon_completion(&mut self) {
        let Some(cycle) = self.completion.take() else {
            return;
        };
        // The restore below rewrites a range; pending closers past it
        // would be off by the difference.
        self.auto_closers.clear();
        // If the operator backspaced past the cycle's start, the
        // stored range no longer points at valid bytes — bail on
        // the restore but still drop the cycle. Same for cursor: a
        // refresh-narrow may have shrunk the buffer below the
        // pre-Tab cursor position; clamp to current buffer length
        // (which is always a valid char boundary).
        if cycle.start <= self.editor.buffer.len()
            && cycle.end <= self.editor.buffer.len()
            && cycle.start <= cycle.end
            && self.editor.buffer.is_char_boundary(cycle.start)
            && self.editor.buffer.is_char_boundary(cycle.end)
        {
            self.editor
                .buffer
                .replace_range(cycle.start..cycle.end, &cycle.origin);
        }
        self.editor.cursor = cycle.origin_cursor.min(self.editor.buffer.len());
        self.last_status = Some("completion cancelled".to_string());
    }

    /// Accept the completion popup's candidate: the highlighted one
    /// stays in the buffer; when none is highlighted yet (LCP expanded
    /// / popup-only) the first candidate is inserted, exactly as a
    /// second Tab would. Either way the cycle ends and the popup
    /// closes. No-op when no cycle is active.
    fn editor_accept_completion(&mut self) {
        let Some(cycle) = self.completion.as_ref() else {
            return;
        };
        if cycle.selected.is_none() {
            self.editor_complete();
        }
        let Some(cycle) = self.completion.take() else {
            return;
        };
        let label = cycle
            .selected
            .and_then(|i| cycle.candidates.get(i))
            .map(|c| c.kind.label());
        self.last_status = label.map(|kind| format!("completion · accepted · {kind}"));
    }

    /// Tab-completion in the editor. Bash-style two-phase:
    ///
    /// - First Tab on a fresh prefix:
    ///   - 1 match: insert it.
    ///   - 2+ matches sharing a longer common prefix: insert just the
    ///     common prefix (so `t_` → `t_us` when every match starts with
    ///     `t_us`). The popup shows all candidates; no row highlighted.
    ///   - 2+ matches sharing no extra prefix: don't insert anything;
    ///     show the popup so the operator can see the options and type
    ///     more characters to narrow.
    /// - Second Tab (cycle present, no candidate selected): pick the
    ///   first match.
    /// - Third+ Tab: cycle through.
    ///
    /// Any non-Tab editor key drops the cycle so typing more characters
    /// reverts cleanly.
    pub(super) fn editor_complete(&mut self) {
        // Editor housekeeping (mirrors editor_dirty) — without clearing
        // the cycle, which we own here.
        self.history_pos = None;
        self.editor.preferred_col = None;
        // Wherever a branch below `replace_range`s the identifier under
        // the cursor, the pending closers are cleared first: one past
        // the range would be off by the difference in length. The
        // popup-only branches leave them alone — the `AND ` auto-trigger
        // fires inside a literal too, and its closer must stay skippable.

        if let Some(cycle) = self.completion.clone() {
            if cycle.candidates.is_empty() {
                return;
            }
            // Either advance to next candidate, or — if nothing's
            // selected yet (we expanded a common prefix or just showed
            // the popup) — pick the first match.
            let next = match cycle.selected {
                None => 0,
                Some(i) => (i + 1) % cycle.candidates.len(),
            };
            let cand = cycle.candidates[next].clone();
            self.auto_closers.clear();
            self.editor
                .buffer
                .replace_range(cycle.start..cycle.end, &cand.insert);
            let new_end = cycle.start + cand.insert.len();
            self.editor.cursor = new_end;
            self.last_status = Some(format!(
                "completion {}/{} · {}",
                next + 1,
                cycle.candidates.len(),
                cand.kind.label()
            ));
            self.completion = Some(CompletionCycle {
                start: cycle.start,
                end: new_end,
                origin: cycle.origin,
                origin_prefix: cycle.origin_prefix,
                origin_cursor: cycle.origin_cursor,
                candidates: cycle.candidates,
                selected: Some(next),
            });
            return;
        }

        // -- start a fresh cycle --
        let Some(id) = complete_q::extract_identifier(&self.editor.buffer, self.editor.cursor)
        else {
            return;
        };
        let cands =
            complete_q::candidates_for(&self.editor.buffer, self.editor.cursor, &self.schema_cache);
        if cands.is_empty() {
            // Tailor the message: empty-cache vs. nothing-to-suggest vs.
            // typed-prefix-but-no-match. SQL vocabulary (keywords,
            // operators) doesn't depend on the cache, so an empty cache
            // doesn't preclude *all* candidates — we only mention the
            // cache when there'd otherwise be no useful hint.
            let msg = if self.schema_cache.is_empty() && id.prefix.is_empty() {
                "completion: connect to a database for identifier suggestions".to_string()
            } else if id.prefix.is_empty() {
                match &id.qualifier {
                    Some(q) => format!("completion: no matches for {q}.…"),
                    None => "completion: nothing to suggest here".to_string(),
                }
            } else {
                format!("completion: no matches for {:?}", id.prefix)
            };
            self.last_status = Some(msg);
            return;
        }

        let prefix_start = self.editor.cursor.saturating_sub(id.prefix.len());
        let replace_end = id.end;
        let original_text = self.editor.buffer[prefix_start..replace_end].to_string();
        let original_cursor = self.editor.cursor;

        // 1) Exact-match fast path: the typed prefix already IS one of
        //    the candidates (case-insensitively). The operator typed the
        //    full name; commit and dismiss the popup. Runs BEFORE the
        //    single-match path so that a lone candidate matching the
        //    typed prefix exactly (e.g. cache has only `users`, operator
        //    typed `users`) also dismisses the popup rather than leaving
        //    a one-row cycle hanging. Empty prefix can't match (no
        //    candidate insert is empty), so this is a no-op there.
        if let Some(exact) = cands
            .iter()
            .find(|c| !c.insert.is_empty() && c.insert.eq_ignore_ascii_case(&id.prefix))
        {
            let cand = exact.clone();
            self.auto_closers.clear();
            self.editor
                .buffer
                .replace_range(prefix_start..replace_end, &cand.insert);
            let new_end = prefix_start + cand.insert.len();
            self.editor.cursor = new_end;
            self.last_status = Some(format!("completion · exact match · {}", cand.kind.label()));
            self.completion = None;
            return;
        }

        // 2) Empty unqualified prefix → always show the popup with no
        //    auto-insertion. The operator pressed Tab on whitespace
        //    asking "what can I type here?"; silently inserting a
        //    single candidate would be a footgun (e.g. `INSERT INTO t
        //    (<Tab>` with a one-column table would commit the column
        //    without the operator seeing the choice). Qualified-empty
        //    (`u.|`) still falls through to single-match — the
        //    qualifier IS the operator's signal of intent.
        if id.prefix.is_empty() && id.qualifier.is_none() {
            let cand_count = cands.len();
            self.last_status = Some(format!(
                "completion: {} match{} · Tab to pick",
                cand_count,
                if cand_count == 1 { "" } else { "es" }
            ));
            self.completion = Some(CompletionCycle {
                start: prefix_start,
                end: replace_end,
                origin: original_text,
                origin_prefix: id.prefix,
                origin_cursor: original_cursor,
                candidates: cands,
                selected: None,
            });
            return;
        }

        // 3) Single-match fast path: insert it and keep the cycle
        //    around so Esc undoes the auto-insert.
        if cands.len() == 1 {
            let cand = cands[0].clone();
            self.auto_closers.clear();
            self.editor
                .buffer
                .replace_range(prefix_start..replace_end, &cand.insert);
            let new_end = prefix_start + cand.insert.len();
            self.editor.cursor = new_end;
            self.last_status = Some(format!("completion 1/1 · {}", cand.kind.label()));
            self.completion = Some(CompletionCycle {
                start: prefix_start,
                end: new_end,
                origin: original_text,
                origin_prefix: id.prefix,
                origin_cursor: original_cursor,
                candidates: cands,
                selected: Some(0),
            });
            return;
        }

        // 4) Multi-match: compute the longest common prefix
        //    (case-insensitive) of all candidate inserts. If it extends
        //    past what the operator already typed, advance the buffer
        //    to that common prefix and show the popup — no specific
        //    row selected yet, so a second Tab picks the first match.
        let inserts: Vec<&str> = cands.iter().map(|c| c.insert.as_str()).collect();
        let lcp = complete_q::longest_common_prefix_ci(&inserts);
        let insert_text = if lcp.len() > id.prefix.len() {
            // Mirror the operator's case onto the LCP (so `t_` stays
            // lowercase; `T_` stays uppercase) — the LCP itself is
            // from the first candidate's case which may not match.
            complete_q::case_match(&lcp, &id.prefix)
        } else {
            // No common prefix to expand. Keep the operator's typed
            // text — don't insert anything yet.
            id.prefix.clone()
        };
        if insert_text != self.editor.buffer[prefix_start..replace_end] {
            self.auto_closers.clear();
        }
        self.editor
            .buffer
            .replace_range(prefix_start..replace_end, &insert_text);
        let new_end = prefix_start + insert_text.len();
        self.editor.cursor = new_end;
        self.last_status = Some(format!(
            "completion: {} match{} · Tab to pick",
            cands.len(),
            if cands.len() == 1 { "" } else { "es" }
        ));
        self.completion = Some(CompletionCycle {
            start: prefix_start,
            end: new_end,
            origin: original_text,
            origin_prefix: id.prefix,
            origin_cursor: original_cursor,
            candidates: cands,
            selected: None,
        });
    }
}

/// Does a bare Enter run this buffer, or insert a newline? It runs when
/// the statement is *terminated* — the trimmed buffer ends with `;` —
/// or is a backslash command (`\l`, `\d users`), which has no
/// terminator to wait for. Anything else is a statement still being
/// typed, and Enter is a newline. Alt-Enter runs regardless; this rule
/// is only for the unmodified key. Pure / testable.
pub fn enter_runs(buffer: &str) -> bool {
    let trimmed = buffer.trim();
    trimmed.ends_with(';') || trimmed.starts_with('\\')
}

/// A statement handed to the editor from a picker, terminated so a
/// bare Enter runs it (see [`enter_runs`]). Appends `;` unless the
/// trimmed text already ends with one, is a backslash command, or is
/// empty — an empty buffer must stay empty for the `editor is empty`
/// notice to fire. Pure / testable.
pub fn ensure_terminated(sql: &str) -> String {
    if enter_runs(sql) || sql.trim().is_empty() {
        return sql.to_string();
    }
    format!("{};", sql.trim_end())
}

pub(super) fn editor_insert(buffer: &mut String, cursor: &mut usize, c: char) {
    buffer.insert(*cursor, c);
    *cursor += c.len_utf8();
}

/// Bracket autoclose: insert the matching close-char after `c`
/// and leave the cursor between them. Pure / testable. Returns
/// `true` when the pair was inserted (so the caller knows the
/// edit happened); `false` for chars that aren't openers.
pub fn editor_insert_pair(buffer: &mut String, cursor: &mut usize, c: char) -> bool {
    let close = match c {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        _ => return false,
    };
    buffer.insert(*cursor, c);
    buffer.insert(*cursor + 1, close);
    // Cursor sits BETWEEN the pair: just past the opener.
    *cursor += 1;
    true
}

/// The closer [`editor_insert_pair`] adds for an opening bracket.
pub fn closer_of(c: char) -> Option<char> {
    match c {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        _ => None,
    }
}

/// The closers autoclose inserted and the operator has not yet typed
/// over: `(byte offset, char)`, oldest first. Typing the matching
/// closer with the cursor at exactly that offset skips over it; any
/// other closer is a literal insert. The stack follows the typing
/// path's edits — an insert before an entry shifts it, a delete
/// across it drops it — and is cleared wholesale by anything that
/// replaces buffer text some other way (paste, undo / redo,
/// completion, history, an external editor): a buffer whose length
/// moved without the stack hearing about it is treated as unknown.
///
/// The old rule skipped any quote that followed a letter or digit
/// as an "in-word apostrophe" and *inserted* the closer instead, so
/// hand-typed `'shipped'` came out `'shipped''` — the closer the
/// editor had added stayed behind. Pure / testable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutoClosers {
    entries: Vec<(usize, char)>,
    /// Buffer length after the last edit this stack was told about.
    seen_len: usize,
}

impl AutoClosers {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Forget every pending closer.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Drop the stack if the buffer changed length behind its back
    /// (an edit outside the typing path). Call before handling a key.
    pub fn sync(&mut self, buffer_len: usize) {
        if buffer_len != self.seen_len {
            self.clear();
            self.seen_len = buffer_len;
        }
    }

    /// Record the buffer length the stack is now consistent with.
    /// Call after handling a key.
    pub fn note_len(&mut self, buffer_len: usize) {
        self.seen_len = buffer_len;
    }

    /// A closer autoclose just inserted at byte `offset`.
    pub fn push(&mut self, offset: usize, close: char) {
        self.entries.push((offset, close));
    }

    /// `len` bytes were inserted at `at`: every closer at or past it
    /// moves right.
    pub fn shift_insert(&mut self, at: usize, len: usize) {
        for (offset, _) in &mut self.entries {
            if *offset >= at {
                *offset += len;
            }
        }
    }

    /// Bytes `start..end` were deleted: a closer inside the range is
    /// gone, one past it moves left.
    pub fn shift_delete(&mut self, start: usize, end: usize) {
        self.entries
            .retain(|(offset, _)| !(start..end).contains(offset));
        for (offset, _) in &mut self.entries {
            if *offset >= end {
                *offset -= end - start;
            }
        }
    }

    /// Is `close` a pending closer at exactly byte `at`, still
    /// present in `buffer`? Consumes it (and any entry behind the
    /// cursor, which typing can no longer reach) when it is.
    pub fn take_at(&mut self, at: usize, close: char, buffer: &str) -> bool {
        let present = buffer.get(at..).is_some_and(|rest| rest.starts_with(close));
        let hit = present
            && self
                .entries
                .iter()
                .any(|&(offset, c)| offset == at && c == close);
        if hit {
            self.entries.retain(|&(offset, _)| offset > at);
        }
        hit
    }
}

/// Quote autoclose: when `c` is `'` or `"`, decide whether to
/// insert a paired quote (cursor between) versus a single literal
/// character. The gate is conservative: only pair when the
/// character before is not an identifier character (a quote after
/// a letter or digit is an apostrophe in a word — `don't` — or a
/// doubled `''` escape) and the character after is neither an
/// identifier character nor any quote (typing `'` in front of an
/// existing `'` or `"` closes or escapes it; a pair there would
/// leave a stray closer behind).
///
/// Returns `true` when the pair was inserted; `false` lets the
/// caller fall back to inserting the literal character.
pub fn editor_maybe_pair_quote(buffer: &mut String, cursor: &mut usize, c: char) -> bool {
    if !matches!(c, '\'' | '"') {
        return false;
    }
    let prev_ok = match char_before(buffer, *cursor) {
        None => true,
        Some(p) => !p.is_alphanumeric() && p != '_',
    };
    let next_ok = match char_after(buffer, *cursor) {
        None => true,
        Some(n) => !n.is_alphanumeric() && n != '_' && !matches!(n, '\'' | '"'),
    };
    if !(prev_ok && next_ok) {
        return false;
    }
    buffer.insert(*cursor, c);
    buffer.insert(*cursor + 1, c);
    *cursor += 1;
    true
}

fn char_before(buffer: &str, cursor: usize) -> Option<char> {
    if cursor == 0 {
        return None;
    }
    let mut i = cursor - 1;
    while !buffer.is_char_boundary(i) {
        i -= 1;
    }
    buffer[i..cursor].chars().next()
}

fn char_after(buffer: &str, cursor: usize) -> Option<char> {
    if cursor >= buffer.len() {
        return None;
    }
    buffer[cursor..].chars().next()
}

/// Toggle a `-- ` line-comment at the start of the line
/// containing `cursor`. Pure: works on the (buffer, cursor)
/// pair the editor already has. The cursor is preserved
/// relative to its original line content (i.e., if removing
/// `-- ` shifts text left by 3 cols, the cursor shifts too).
pub fn editor_toggle_line_comment(buffer: &mut String, cursor: &mut usize) {
    let line_start = line_start_byte(buffer, *cursor);
    // Inspect the leading characters of the line.
    let rest = &buffer[line_start..];
    if let Some(stripped) = rest.strip_prefix("-- ") {
        // Drop 3 chars.
        let drop = 3;
        let _ = stripped; // unused — using `drop` length only.
        buffer.replace_range(line_start..line_start + drop, "");
        if *cursor >= line_start + drop {
            *cursor -= drop;
        } else if *cursor > line_start {
            // Cursor was inside the `-- ` prefix — clamp to start.
            *cursor = line_start;
        }
    } else if rest.starts_with("--") {
        // No trailing space — drop 2.
        let drop = 2;
        buffer.replace_range(line_start..line_start + drop, "");
        if *cursor >= line_start + drop {
            *cursor -= drop;
        } else if *cursor > line_start {
            *cursor = line_start;
        }
    } else {
        // Comment in — insert `-- ` at line start.
        buffer.insert_str(line_start, "-- ");
        if *cursor >= line_start {
            *cursor += 3;
        }
    }
}

/// Delete the character before the cursor (Backspace).
pub(super) fn editor_backspace(buffer: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let mut prev = *cursor - 1;
    while !buffer.is_char_boundary(prev) {
        prev -= 1;
    }
    buffer.replace_range(prev..*cursor, "");
    *cursor = prev;
}

/// Delete the character at the cursor (Delete / Del).
pub(super) fn editor_delete(buffer: &mut String, cursor: &mut usize) {
    if *cursor >= buffer.len() {
        return;
    }
    let mut next = *cursor + 1;
    while next < buffer.len() && !buffer.is_char_boundary(next) {
        next += 1;
    }
    buffer.replace_range(*cursor..next, "");
}

/// Move the cursor one character left, respecting UTF-8 boundaries.
pub(super) fn editor_move_left(buffer: &str, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let mut prev = *cursor - 1;
    while !buffer.is_char_boundary(prev) {
        prev -= 1;
    }
    *cursor = prev;
}

/// Move the cursor one character right, respecting UTF-8 boundaries.
pub(super) fn editor_move_right(buffer: &str, cursor: &mut usize) {
    if *cursor >= buffer.len() {
        return;
    }
    let mut next = *cursor + 1;
    while next < buffer.len() && !buffer.is_char_boundary(next) {
        next += 1;
    }
    *cursor = next;
}

/// Move the cursor up one line, preserving the preferred char-column.
pub(super) fn editor_move_up(buffer: &str, cursor: &mut usize, preferred_col: &mut Option<usize>) {
    let (line, col) = cursor_position(buffer, *cursor);
    if line == 0 {
        return;
    }
    let target = preferred_col.unwrap_or(col);
    *preferred_col = Some(target);
    *cursor = byte_offset_at_line_col(buffer, line - 1, target);
}

/// Move the cursor down one line, preserving the preferred char-column.
pub(super) fn editor_move_down(
    buffer: &str,
    cursor: &mut usize,
    preferred_col: &mut Option<usize>,
) {
    let (line, col) = cursor_position(buffer, *cursor);
    let total_lines = buffer.matches('\n').count() + 1;
    if line + 1 >= total_lines {
        return;
    }
    let target = preferred_col.unwrap_or(col);
    *preferred_col = Some(target);
    *cursor = byte_offset_at_line_col(buffer, line + 1, target);
}
