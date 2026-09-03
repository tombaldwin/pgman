use super::*;

impl App {
    /// Apply a finished message from a spawned task. Tap events
    /// bypass the generation filter — the tap listener is bound
    /// at startup and lives independently of the DB connection,
    /// so events arriving across a reconnect are still meaningful.
    pub(super) fn on_msg(&mut self, msg: AppMsg) {
        if !matches!(msg, AppMsg::TapEvent { .. } | AppMsg::UpdateCheck(_))
            && msg.generation() != self.generation
        {
            tracing::debug!(
                "dropping stale message from generation {}",
                msg.generation()
            );
            return;
        }
        match msg {
            AppMsg::Booted {
                server_version,
                grid,
                client,
                schema_cache,
                tunnel,
                ..
            } => {
                self.conn_state = ConnState::Connected { server_version };
                // Pre-build the cancel dispatcher so Ctrl-C can fire
                // without touching the Client. Replaced on every new
                // Booted so the dispatcher always matches the live
                // backend PID.
                self.cancel_dispatcher =
                    Some(Box::new(PgCancelDispatcher::new(client.cancel_token())));
                self.client = Some(client);
                // Hold the new tunnel (if any) so its Drop fires when
                // the App loses the client at quit / next reconnect.
                // The PREVIOUS tunnel — if there was one — must be
                // dropped off-thread: `SshTunnel::drop` does
                // `child.kill()` + blocking `child.wait()`, and a
                // wedged ssh subprocess (e.g. stuck ProxyCommand)
                // would otherwise freeze the UI loop here.
                if let Some(old) = self.tunnel.take() {
                    tokio::task::spawn_blocking(move || drop(old));
                }
                self.tunnel = tunnel;
                self.apply_bootstrap_grid(grid);
                self.schema_cache = schema_cache;
                // Schema changed → editor highlight (keyed on buffer only)
                // must be recomputed against the new cache.
                self.editor_highlight_cache = None;
                // Splash stays up — `tick_splash` honours the `SPLASH_MIN`
                // floor before dismissing on a resolved connection.
            }
            AppMsg::BootFailed { error, .. } => {
                self.conn_state = ConnState::Failed(error);
            }
            AppMsg::QueryOk {
                grid,
                kind_label,
                tx_open_after,
                ..
            } => {
                self.grid = grid;
                self.grid_state
                    .select(if self.grid.is_empty() { None } else { Some(0) });
                self.reset_grid_view();
                // If the run was DDL, re-fetch the schema so completion /
                // browser / lint / FK-nav reflect it. When the DDL is wrapped
                // in an open transaction (auto_tx), DEFER the refetch to
                // TxClosed — refreshing now would show objects a subsequent
                // rollback discards. Non-tx DDL refreshes immediately.
                if self.schema_dirty_after_run && !tx_open_after {
                    self.schema_dirty_after_run = false;
                    self.spawn_schema_refresh();
                }
                // Infer the source table for the new grid — used by
                // row-as-INSERT yank (and, eventually, cell-edit-to-
                // UPDATE / FK nav). Single-table SELECT only; anything
                // else clears the source so the feature gates self-
                // disable.
                self.grid_view.source = self
                    .last_run_sql
                    .as_deref()
                    .and_then(infer_single_source_table);
                // psql `\x` — expanded output. Land the new result in
                // the row-detail view for its first row instead of
                // the grid. No-op when the result is empty (nothing
                // to expand into).
                if self.expanded_on {
                    self.open_row_detail();
                }
                self.query_running = false;
                self.last_error = None;
                self.last_error_detail = None;
                let elapsed = self.query_started.take().map(|t0| t0.elapsed());
                let timing_suffix = if self.timing_on {
                    elapsed
                        .map(|d| format!(" · {:.0} ms", d.as_secs_f64() * 1000.0))
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let cap_suffix = if self.grid.truncated {
                    format!(" · capped at {}", crate::grid::MAX_ROWS)
                } else {
                    String::new()
                };
                self.last_status = Some(format!(
                    "{kind_label} ok · {} row(s){cap_suffix}{timing_suffix}",
                    self.grid.row_count()
                ));
                // EXPLAIN / EXPLAIN ANALYZE: parse the JSON we asked
                // for and pop the tree visualiser. On parse failure
                // we fall back to the raw grid (the JSON text is
                // still readable that way), surface the parse error
                // in last_status so the operator sees what happened.
                if kind_label == "EXPLAIN" || kind_label == "EXPLAIN ANALYZE" {
                    if let Some(text) = self.grid.rows.first().and_then(|r| r.first()).cloned() {
                        match crate::query::explain::parse(&text) {
                            Ok(plan) => {
                                self.explain.plan = Some(plan);
                                self.explain.cursor = 0;
                                self.explain.collapsed.clear();
                                self.mode = Mode::ExplainTree;
                            }
                            Err(e) => {
                                self.last_status = Some(format!(
                                    "{kind_label} parse: {e} — falling back to raw text"
                                ));
                            }
                        }
                    }
                }
                if tx_open_after {
                    self.tx_open = true;
                    self.mode = Mode::TxDecision;
                }
            }
            AppMsg::QueryFailed {
                error,
                position,
                detail,
                ..
            } => {
                self.query_running = false;
                self.query_started = None;
                // The run failed — it didn't change the schema, so drop the
                // pending refresh flag.
                self.schema_dirty_after_run = false;
                self.last_status = None;
                self.last_error = Some(error);
                self.last_error_detail = detail;
                // Postgres flagged a syntax error at a specific
                // character — move the editor cursor there so the
                // operator sees the offending token. The position is
                // 1-indexed CHARS into the SQL we submitted; convert
                // to a 0-indexed BYTE offset into `editor.buffer`.
                // Out-of-range positions are ignored (could happen
                // for batches where we sent a transformed string).
                if let Some(p) = position {
                    // Postgres reports a 1-indexed CHAR position into
                    // the string WE submitted. `request_run` trims
                    // leading whitespace before submitting, so we
                    // skip past the same trimmed prefix in the
                    // editor buffer before counting chars. Without
                    // this, `\n\nSELECT FROM x` with an error at
                    // submitted position 8 lands the cursor 2 chars
                    // off because the leading `\n\n` is in the
                    // buffer but not in the submitted SQL.
                    let trimmed_prefix_bytes =
                        self.editor.buffer.len() - self.editor.buffer.trim_start().len();
                    let target_chars = (p.saturating_sub(1)) as usize;
                    let after_trim = &self.editor.buffer[trimmed_prefix_bytes..];
                    let inner_byte = after_trim
                        .char_indices()
                        .nth(target_chars)
                        .map(|(b, _)| b)
                        .unwrap_or(after_trim.len());
                    let byte_offset = trimmed_prefix_bytes + inner_byte;
                    self.editor.cursor = byte_offset.min(self.editor.buffer.len());
                    self.editor.preferred_col = None;
                    if self.mode == Mode::Normal {
                        self.mode = Mode::Editor;
                    }
                }
            }
            AppMsg::TxClosed {
                committed, error, ..
            } => {
                self.tx_open = false;
                self.query_running = false;
                self.mode = Mode::Editor;
                // A DDL run wrapped in this transaction is only durable once
                // committed — refresh the schema cache then. On rollback (or a
                // failed close) just drop the pending flag without refetching.
                if std::mem::take(&mut self.schema_dirty_after_run) && committed && error.is_none()
                {
                    self.spawn_schema_refresh();
                }
                match error {
                    Some(e) => {
                        // A close failure carries no `DbError` fields —
                        // whatever detail the last query left is not
                        // this error's.
                        self.last_error = Some(format!("tx close failed: {e}"));
                        self.last_error_detail = None;
                    }
                    None => {
                        self.last_status = Some(
                            if committed {
                                "committed"
                            } else {
                                "rolled back"
                            }
                            .to_string(),
                        );
                    }
                }
            }
            AppMsg::SlowQueriesLoaded { result, .. } => match result {
                Ok(rows) => {
                    // A load that worked supersedes whatever failed
                    // before it (the previous `T`, a refresh) — the
                    // footer shows this panel now, not that error.
                    self.last_error = None;
                    self.last_error_detail = None;
                    self.slow_queries.rows = rows;
                    // Preserve the operator's selection across an auto-refresh
                    // tick (R) — clamp to the new length rather than zeroing,
                    // which would yank the cursor to the top every 5s. Fresh
                    // opens reset to 0 separately in start_slow_queries.
                    self.slow_queries.cursor = self
                        .slow_queries
                        .cursor
                        .min(self.slow_queries.rows.len().saturating_sub(1));
                    self.last_status = Some(format!(
                        "slow queries · {} row(s)",
                        self.slow_queries.rows.len()
                    ));
                }
                Err(e) => {
                    // pg_stat_statements not installed is the most
                    // common failure — point the operator at the
                    // `CREATE EXTENSION` they need. The server says
                    // `relation "pg_stat_statements" does not exist`
                    // (SQLSTATE 42P01); the message has to be the
                    // server's for this to fire — a bare `db error`
                    // named nothing, hinted nothing, and F2 had no
                    // detail to show.
                    // `CREATE EXTENSION` alone is not enough: the
                    // module has to be preloaded, and the session
                    // pgman opened is read-only by default.
                    let hint = if e.msg.contains("pg_stat_statements") {
                        " (try `CREATE EXTENSION pg_stat_statements` — needs shared_preload_libraries and a read-write session)"
                    } else {
                        ""
                    };
                    tracing::warn!(
                        code = e.detail.as_ref().and_then(|d| d.code.as_deref()),
                        "slow queries load failed: {}",
                        e.msg
                    );
                    self.last_error = Some(format!("slow queries load failed: {}{hint}", e.msg));
                    self.last_error_detail = e.detail;
                    self.mode = Mode::Normal;
                }
            },
            AppMsg::SessionsLoaded { result, .. } => match result {
                Ok(rows) => {
                    self.last_error = None;
                    self.last_error_detail = None;
                    let blocked = rows.iter().filter(|r| r.is_blocked()).count();
                    self.sessions.rows = rows;
                    // Preserve selection across auto-refresh (R) — clamp, don't
                    // zero (see SlowQueriesLoaded). Fresh opens reset in
                    // start_sessions.
                    self.sessions.cursor = self
                        .sessions
                        .cursor
                        .min(self.sessions.rows.len().saturating_sub(1));
                    self.last_status = Some(sessions_status(self.sessions.rows.len(), blocked));
                }
                Err(e) => {
                    tracing::warn!(
                        code = e.detail.as_ref().and_then(|d| d.code.as_deref()),
                        "sessions load failed: {}",
                        e.msg
                    );
                    self.last_error = Some(format!("sessions load failed: {}", e.msg));
                    self.last_error_detail = e.detail;
                    self.mode = Mode::Normal;
                }
            },
            AppMsg::LiveLintLoaded { result, .. } => {
                // Merge live findings into the existing pure list.
                // If the operator already left the lint panel,
                // silently drop — a fresh open re-fires the fetch.
                if self.mode != Mode::SchemaLint {
                    return;
                }
                match result {
                    Ok(live) => {
                        self.last_error = None;
                        self.last_error_detail = None;
                        let added = live.len();
                        self.schema_lint.findings.extend(live);
                        // Re-sort to keep severity ordering after
                        // merge. Same sort as `lint::run_all`.
                        self.schema_lint.findings.sort_by(|a, b| {
                            a.severity
                                .cmp(&b.severity)
                                .then_with(|| a.code.cmp(b.code))
                                .then_with(|| a.object.cmp(&b.object))
                        });
                        // Clamp the cursor — re-sort may have moved
                        // the focused row's index.
                        let last = self.schema_lint.findings.len().saturating_sub(1);
                        if self.schema_lint.cursor > last {
                            self.schema_lint.cursor = last;
                        }
                        let total = self.schema_lint.findings.len();
                        let high = self
                            .schema_lint
                            .findings
                            .iter()
                            .filter(|f| f.severity == crate::query::lint::Severity::High)
                            .count();
                        self.last_status = Some(format!(
                            "schema lint · {total} finding(s) · {high} high · live: +{added}"
                        ));
                    }
                    Err(e) => {
                        // Live check failed — leave the pure
                        // findings in place. Surface the failure
                        // so the operator knows the FK-index
                        // check didn't run.
                        tracing::warn!("schema lint live check failed: {e}");
                        self.last_status = Some(format!(
                            "schema lint · live check failed: {e} (showing cached-only)"
                        ));
                    }
                }
            }
            AppMsg::SchemaRefreshed { schema_cache, .. } => {
                // Post-DDL re-fetch landed (generation already checked above) —
                // swap in the fresh cache so completion / browser / lint /
                // FK-nav see the new shape.
                self.schema_cache = schema_cache;
                // Recompute editor highlighting against the new schema.
                self.editor_highlight_cache = None;
            }
            AppMsg::CostPreviewLoaded {
                sql,
                decision,
                estimated,
                threshold,
                ..
            } => {
                // Clear the pre-flight busy flag — spawn_run sets
                // its own when the real query goes; the Confirm
                // modal doesn't run a query so it should also clear.
                self.query_running = false;
                match estimated {
                    Ok(rows) if rows > threshold as f64 => {
                        // Over threshold — gate behind Confirm. Reuse
                        // the existing pending_run machinery so y/n
                        // wiring stays in one place.
                        let summary = format!(
                            "cost preview: estimated {} rows (threshold {threshold}) — proceed?",
                            format_row_estimate(rows),
                        );
                        self.last_status = Some(summary.clone());
                        self.pending_run = Some(PendingRun {
                            sql,
                            kind: RunKind::Run,
                            decision,
                            is_batch: false,
                            summary: Some(summary),
                        });
                        self.mode = Mode::Confirm;
                    }
                    Ok(rows) => {
                        // Under threshold — proceed silently.
                        self.last_status = Some(format!(
                            "pre-flight ok · est {} rows",
                            format_row_estimate(rows)
                        ));
                        self.spawn_run(sql, RunKind::Run, decision, false);
                    }
                    Err(e) => {
                        // EXPLAIN itself failed — don't block; surface
                        // and proceed (the real query will fail too if
                        // it's e.g. a syntax error).
                        tracing::warn!("cost preview EXPLAIN failed: {e}");
                        self.last_status = Some(format!("pre-flight skipped: {e}"));
                        self.spawn_run(sql, RunKind::Run, decision, false);
                    }
                }
            }
            AppMsg::Notice { notice, .. } => {
                // Surface server-emitted notices in the status footer,
                // and stash recent ones so a follow-up "show notices"
                // panel can render them. Severity goes first so a
                // `WARNING` reads visibly different from a `NOTICE`.
                self.last_status = Some(format!("[{}] {}", notice.severity, notice.message));
                tracing::info!(
                    "pg notice [{}]: {}{}{}",
                    notice.severity,
                    notice.message,
                    notice
                        .detail
                        .as_deref()
                        .map(|d| format!(" · detail: {d}"))
                        .unwrap_or_default(),
                    notice
                        .hint
                        .as_deref()
                        .map(|h| format!(" · hint: {h}"))
                        .unwrap_or_default(),
                );
                self.notices.push(notice);
                if self.notices.len() > 50 {
                    self.notices.remove(0);
                }
            }
            AppMsg::Notification { notification, .. } => {
                // Brief status flash so the operator notices an
                // arrival even without the `N` panel open. The
                // ring buffer carries the full history for the
                // panel to render later.
                let preview: String = notification.payload.chars().take(40).collect();
                self.last_status = Some(format!(
                    "NOTIFY {} (pid {}): {preview}",
                    notification.channel, notification.pid
                ));
                tracing::info!(
                    "pg notify · channel={} pid={} payload={}",
                    notification.channel,
                    notification.pid,
                    notification.payload,
                );
                self.notifications.items.push(notification);
                if self.notifications.items.len() > NOTIFICATION_CAP {
                    let drop = self.notifications.items.len() - NOTIFICATION_CAP;
                    self.notifications.items.drain(..drop);
                }
            }
            AppMsg::TapEvent { event } => self.on_tap_event(event),
            AppMsg::UpdateCheck(latest) => {
                self.update_check_done = true;
                self.update_available = latest;
            }
        }
    }

    /// Route the bootstrap query's result grid into `databases`
    /// instead of the results grid, so the start card — which only
    /// shows while `grid.columns` is empty — is what the operator
    /// lands on after every real connect, not a two-column grid of
    /// database names and sizes. Also resets the grid-view state a
    /// fresh connection warrants (sort/filter/bookmarks from the
    /// previous session don't carry over).
    pub(super) fn apply_bootstrap_grid(&mut self, grid: Grid) {
        self.databases = parse_bootstrap_databases(&grid);
        self.grid = Grid::default();
        self.grid_state.select(None);
        self.reset_grid_view();
    }

    pub(super) fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.on_key(key),
            Event::Paste(text) => self.on_paste(text),
            _ => {}
        }
    }
}
