use super::*;

impl App {
    /// What the connect learned about the server's role
    /// (`pg_is_in_recovery()`). A standby is pinned read-only for the
    /// session exactly as a replica opened from the environments
    /// overview is (`replica_pin`), however it was reached — `--dsn`,
    /// `:connect`, a static environment — and whatever the profile
    /// says: the server refuses every write, so `u`, `:readonly off`
    /// and the header must say so up front rather than let a write be
    /// sent and blamed on `safety.toml`.
    pub(super) fn note_server_role(&mut self, in_recovery: Option<bool>) {
        self.in_recovery = in_recovery;
        if in_recovery == Some(true) {
            self.read_only = true;
            self.replica_pin = true;
        }
    }

    /// Why this session is read-only, for the hint a read-only refusal
    /// (SQLSTATE `25006`) carries: what the server said at connect, the
    /// replica pin, the database's profile, `:readonly on`.
    pub(super) fn read_only_cause(&self) -> conn::ReadOnlyCause {
        let db = self
            .dsn
            .as_ref()
            .map(|d| d.dbname.as_str())
            .unwrap_or("default");
        conn::ReadOnlyCause::for_session(
            &self.safety_config,
            db,
            crate::app::existing_safety_toml(),
            self.in_recovery,
            self.replica_pin,
            self.read_only_override == Some(true),
        )
    }

    /// Apply a finished message from a spawned task. Messages from a
    /// superseded connection generation are dropped, except the kinds
    /// that don't belong to a connection (update check, environments,
    /// terminate). Tap events don't come through here at all: they
    /// have their own bounded channel (`App::tap_tx_clone`).
    pub(super) fn on_msg(&mut self, msg: AppMsg) {
        if !matches!(
            msg,
            AppMsg::UpdateCheck(_)
                | AppMsg::Terminate
                | AppMsg::EnvironmentsLoaded(_)
                | AppMsg::EnvSecretResolved { .. }
                | AppMsg::EnvDiscovered { .. }
                | AppMsg::EnvProbed { .. }
                | AppMsg::EnvLogs { .. }
                | AppMsg::EnvPatchPlanned { .. }
                | AppMsg::EnvPatched { .. }
        ) && msg.generation() != self.generation
        {
            tracing::debug!(
                "dropping stale message from generation {}",
                msg.generation()
            );
            return;
        }
        match msg {
            AppMsg::Terminate => {
                tracing::info!("hangup or terminate signal: quitting");
                self.should_quit = true;
            }
            AppMsg::Booted {
                server_version,
                in_recovery,
                grid,
                client,
                schema_cache,
                tunnel,
                ..
            } => {
                if client.is_closed() {
                    // Closed between the bootstrap and now: its
                    // `ConnectionLost` landed while this was still
                    // connecting, and went unheeded. Its tunnel goes
                    // off-thread, like the one a Booted replaces below.
                    if let Some(dead) = tunnel {
                        tokio::task::spawn_blocking(move || drop(dead));
                    }
                    self.conn_state = ConnState::Failed(super::msg::connection_lost_reason(None));
                    return;
                }
                self.conn_state = ConnState::Connected { server_version };
                self.note_server_role(in_recovery);
                // Pre-build the cancel dispatcher so Ctrl-C can fire
                // without touching the Client. Replaced on every new
                // Booted so the dispatcher always matches the live
                // backend PID.
                self.cancel_dispatcher =
                    Some(Box::new(PgCancelDispatcher::new(client.cancel_token())));
                // A new pager for the new session; the old one (and any
                // cursor it held on the old session) ends as it drops.
                self.pager = Some(crate::query::paging::Pager::spawn(client.clone()));
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
            AppMsg::ConnectionLost { reason, .. } => self.on_connection_lost(reason),
            AppMsg::QueryOk {
                grid,
                origins,
                kind_label,
                tx_open_after,
                paging,
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
                // A new result: a paging record if it ran through the
                // pager, and the cursor if that stayed open. Any older
                // cursor is already closed — this run went through the
                // pager or `App::db`, and both close it first.
                let id = self.next_result_id();
                let now = Instant::now();
                self.cursor = paging
                    .as_ref()
                    .and_then(|p| p.cursor)
                    .map(|limits| CursorMirror {
                        result_id: id,
                        opened: now,
                        last_fetch: now,
                        limits,
                    });
                self.result_paging = paging.map(|p| ResultPaging {
                    id,
                    sql: p.sql,
                    ceiling: false,
                    in_transaction: p.in_transaction,
                    standby: p.standby,
                });
                // Infer the source table for the new grid — used by
                // row-as-INSERT yank (and, eventually, cell-edit-to-
                // UPDATE / FK nav). Single-table SELECT only; anything
                // else clears the source so the feature gates self-
                // disable.
                self.grid_view.source = self
                    .last_run_sql
                    .as_deref()
                    .and_then(infer_single_source_table);
                self.grid_view.origins = origins;
                // psql `\x` — expanded output. Land the new result in
                // the row-detail view for its first row instead of
                // the grid. No-op when the result is empty (nothing
                // to expand into).
                if self.expanded_on {
                    self.open_row_detail();
                }
                self.query_running = false;
                // A cancel that lost the race to the result.
                self.cancel_requested = false;
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
                // The cap note goes last: its CLI pointer is the way out,
                // and the footer's fitting keeps the last segment.
                let cap_suffix = if self.grid.truncated {
                    format!(
                        " · {}",
                        crate::grid::partial_status_note(self.result_more())
                    )
                } else {
                    String::new()
                };
                self.last_status = Some(format!(
                    "{kind_label} ok · {} row(s){timing_suffix}{cap_suffix}",
                    self.grid.row_count()
                ));
                // EXPLAIN / EXPLAIN ANALYZE: parse the JSON we asked
                // for and pop the tree visualiser. On parse failure
                // we fall back to the raw grid (the JSON text is
                // still readable that way), surface the parse error
                // in last_status so the operator sees what happened.
                if kind_label == "EXPLAIN" || kind_label == "EXPLAIN ANALYZE" {
                    if let Some(text) = self
                        .grid
                        .rows
                        .first()
                        .and_then(|r| r.first())
                        .map(|c| crate::grid::cell_text(c).to_string())
                    {
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
            AppMsg::PageLoaded {
                result_id, outcome, ..
            } => self.on_page_loaded(result_id, outcome),
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
                // The cancel Ctrl-C asked for comes back as an error;
                // shown as one, it read as the server failing the query.
                // A statement timeout is the same SQLSTATE without the
                // request, and stays an error.
                let asked = std::mem::take(&mut self.cancel_requested);
                if asked && detail.as_ref().and_then(|d| d.code.as_deref()) == Some("57014") {
                    self.last_error = None;
                    self.last_error_detail = None;
                    self.last_status = Some("cancelled".into());
                    return;
                }
                // A read-only refusal says why this session is
                // read-only; the server's message doesn't.
                let (error, detail) =
                    conn::explain_read_only_refusal(error, detail, &self.read_only_cause());
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
            AppMsg::CellUpdated {
                pending, value, tx, ..
            } => self.on_cell_updated(pending, value, tx),
            AppMsg::TxClosed {
                committed, error, ..
            } => {
                self.tx_open = false;
                self.query_running = false;
                // A cell edit's transaction lands back where the edit
                // began; a rollback (or a failed commit, which ends the
                // transaction too) puts the old value back.
                self.mode = match self.edit_awaiting_tx.take() {
                    Some(edit) => {
                        let landed = committed && error.is_none();
                        if !landed {
                            self.revert_cell_edit(&edit);
                        }
                        edit.return_to
                    }
                    None => Mode::Editor,
                };
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
                        // The footer shows an error over a status; the
                        // statement before this one's failure is old news.
                        self.last_error = None;
                        self.last_error_detail = None;
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
            AppMsg::SlowQueriesLoaded { result, .. } => self.on_slow_queries_loaded(result),
            AppMsg::DdlLoaded { table, result, .. } => self.on_ddl_loaded(&table, result),
            AppMsg::EnvironmentsLoaded(result) => {
                self.environments.loading = false;
                match result {
                    Ok(reports) => {
                        let dbs: usize = reports.iter().map(|r| r.targets.len()).sum();
                        let failed = reports.iter().filter(|r| r.error.is_some()).count();
                        // A refresh keeps the cursor on the same database,
                        // not the same row number: listings reorder.
                        let here = self.environments.selection();
                        self.environments.reports = reports;
                        self.environments.error = None;
                        self.environments_config_broken = false;
                        self.start_env_probes();
                        let restored = self
                            .environments
                            .restore
                            .take()
                            .or(here)
                            .and_then(|sel| self.environments.index_of(&sel));
                        self.environments.cursor = restored
                            .unwrap_or(self.environments.cursor)
                            .min(self.environments.target_count().saturating_sub(1));
                        self.last_status = Some(environments_status(
                            self.environments.reports.len(),
                            dbs,
                            failed,
                        ));
                    }
                    Err(e) => {
                        tracing::warn!("environments load failed: {e}");
                        self.environments.reports.clear();
                        self.environments.probes.clear();
                        self.environments.error = Some(e);
                        // The listing fails only when the config does.
                        self.environments_config_broken = true;
                        self.last_status = None;
                    }
                }
            }
            AppMsg::EnvSecretResolved { token, result } => self.on_env_secret(token, result),
            AppMsg::EnvDiscovered { token, result } => self.on_env_discovered(token, result),
            AppMsg::EnvPatchPlanned {
                environment,
                server,
                database,
                result,
            } => self.on_env_patch_planned(&environment, &server, &database, result),
            AppMsg::EnvPatched { plan, report } => self.on_env_patched(plan, report),
            AppMsg::EnvLogs {
                environment,
                server,
                result,
            } => self.on_env_logs(&environment, &server, result),
            AppMsg::EnvProbed {
                seq,
                report,
                outcomes,
            } => self.on_env_probed(seq, report, outcomes),
            AppMsg::SessionsLoaded { result, .. } => self.on_sessions_loaded(result),
            AppMsg::LiveLintLoaded { result, .. } => self.on_live_lint_loaded(result),
            AppMsg::AdvisorLoaded { result, .. } => self.on_advisor_loaded(result),
            AppMsg::ReadOnlyApplied { want, result, .. } => {
                let state = if want { "on" } else { "off" };
                match result {
                    Ok(()) => {
                        self.read_only = want;
                        self.last_error = None;
                        self.last_error_detail = None;
                        self.last_status = Some(format!(
                            "read-only {state} · this session from its next statement, and every later connect"
                        ));
                    }
                    Err(e) => {
                        // The header must say what the server will do.
                        self.read_only = !want;
                        self.last_error = Some(format!(
                            "read-only {state} not applied to this session ({e}) · it applies at the next connect"
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
                if std::mem::take(&mut self.schema_reload_announce) {
                    self.last_status = Some(schema_reloaded_status(&self.schema_cache));
                }
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
                            cell_edit: None,
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
        self.forget_result_paging();
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

/// What to do about a slow-queries failure, when pgman knows. Pure.
pub(crate) fn slow_queries_hint(msg: &str) -> String {
    if msg.contains("pg_stat_statements") {
        format!("\n\n{}", crate::query::slow_queries::PGSS_MISSING)
    } else {
        String::new()
    }
}

/// Handlers a demo fixture also calls directly.
impl App {
    pub(super) fn on_slow_queries_loaded(
        &mut self,
        result: Result<Vec<crate::query::slow_queries::SlowQueryRow>, crate::conn::QueryErr>,
    ) {
        match result {
            Ok(rows) => {
                // A load that worked supersedes whatever failed
                // before it (the previous `T`, a refresh) — the
                // footer shows this panel now, not that error.
                self.last_error = None;
                self.last_error_detail = None;
                self.slow_queries.error = None;
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
                let hint = slow_queries_hint(&e.msg);
                tracing::warn!(
                    code = e.detail.as_ref().and_then(|d| d.code.as_deref()),
                    "slow queries load failed: {}",
                    e.msg
                );
                // The panel stays open and says why it's empty: it
                // used to close at once, flashing past before the
                // footer's message could be read.
                self.last_status = None;
                self.slow_queries.rows.clear();
                self.slow_queries.error = Some(format!("{}{hint}", e.msg));
                self.last_error = Some(format!("slow queries load failed: {}", e.msg));
                self.last_error_detail = e.detail;
            }
        }
    }

    pub(super) fn on_sessions_loaded(
        &mut self,
        result: Result<Vec<crate::query::sessions::SessionRow>, crate::conn::QueryErr>,
    ) {
        match result {
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
        }
    }

    pub(super) fn on_live_lint_loaded(
        &mut self,
        result: Result<Vec<crate::query::lint::Finding>, String>,
    ) {
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
                self.schema_lint.live_note = Some(format!("live: +{}", live.len()));
                // Re-sorts (severity first) and clamps the cursor.
                self.schema_lint.merge(live);
            }
            Err(e) => {
                // Live check failed — leave the pure
                // findings in place. Surface the failure
                // so the operator knows the FK-index
                // check didn't run.
                tracing::warn!("schema lint live check failed: {e}");
                self.schema_lint.live_note =
                    Some(format!("live check failed: {e} (showing cached-only)"));
            }
        }
        self.last_status = Some(self.schema_lint.status());
    }

    /// The config advisor's findings landed: merged like the live lint,
    /// and dropped the same way once the wizard is closed. A check that
    /// couldn't run is named in the status line; the rest still show.
    pub(super) fn on_advisor_loaded(
        &mut self,
        result: Result<crate::query::advisor::Report, String>,
    ) {
        if self.mode != Mode::SchemaLint {
            return;
        }
        match result {
            Ok(report) => {
                for e in &report.errors {
                    tracing::warn!("config advisor partial: {e}");
                }
                let mut note = format!("advisor: +{}", report.findings.len());
                if !report.errors.is_empty() {
                    note.push_str(&format!(
                        " ({} couldn't run: {})",
                        report.errors.len(),
                        report.errors.join("; ")
                    ));
                }
                self.schema_lint.advisor_note = Some(note);
                self.schema_lint.merge(report.findings);
            }
            Err(e) => {
                tracing::warn!("config advisor failed: {e}");
                self.schema_lint.advisor_note = Some(format!("advisor failed: {e}"));
            }
        }
        self.last_status = Some(self.schema_lint.status());
    }
}
