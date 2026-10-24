use super::*;

/// Whether `spawn_run` sends `sql` to the pager, to run behind the
/// result cursor so `M` can fetch past the first page
/// (docs/backlog/paging.md): a plain `Run` of one statement
/// `safety::is_pageable_select` accepts, outside any auto-tx. Not a
/// `\watch` re-run: one every 2 s would hold a transaction open between
/// runs for no reader. Pure.
pub(crate) fn runs_behind_cursor(
    kind: RunKind,
    is_batch: bool,
    wrap_in_tx: bool,
    watching: bool,
    sql: &str,
) -> bool {
    matches!(kind, RunKind::Run)
        && !is_batch
        && !wrap_in_tx
        && !watching
        && crate::safety::is_pageable_select(sql)
}

/// Where `spawn_run` sends a statement: through the queue like any
/// database work, or to the pager to run behind a cursor.
enum RunRoute {
    Db(crate::query::paging::Db),
    Pager(tokio::sync::oneshot::Receiver<Result<crate::query::paging::Opened, conn::QueryErr>>),
}

impl App {
    /// Clear the state that belongs to the session being left: for a
    /// reconnect ([`App::start_connect`]) and a lost connection
    /// ([`App::on_connection_lost`]) alike.
    fn abandon_session_state(&mut self) {
        // Abandon any in-flight query state from the prior session. On a
        // reconnect the query's QueryOk/QueryFailed (which would normally
        // clear these) is tagged with the OLD generation and will be
        // dropped by the on_msg stale-generation filter, so without this
        // the flag would stay stuck true — a permanent spinner that blocks
        // every new run on the new connection.
        self.query_running = false;
        self.query_started = None;
        // Anything a decision modal was holding: an open transaction
        // belongs to the connection being dropped (the server rolls it
        // back), a guarded run was classified against the old database,
        // and a pid is meaningless on the new one. Left set, `tx_open`
        // refused `\watch` for the rest of the session and `pending_run`
        // could fire against the wrong DB.
        self.tx_open = false;
        if let Some(edit) = self.edit_awaiting_tx.take() {
            self.revert_cell_edit(&edit);
        }
        self.pending_run = None;
        self.cell_edit = None;
        self.pending_terminate = None;
    }

    /// The live connection closed under the App
    /// (`AppMsg::ConnectionLost`): drop its pager and cancel dispatcher,
    /// clear what belonged to the session as a reconnect does, and show
    /// the connection-failed card with the reason — whose `r` reconnects
    /// and `p` picks another connection, as after a failed connect. The
    /// dead client itself stays until a reconnect replaces it; `App::db`
    /// and `spawn_run` send nothing to it while the card is up.
    pub(super) fn on_connection_lost(&mut self, reason: String) {
        if !matches!(self.conn_state, ConnState::Connected { .. }) {
            // A connect that failed says so itself (`BootFailed`); one
            // still in flight checks its client when it lands, and says
            // what this one said if it is closed.
            if matches!(self.conn_state, ConnState::Connecting) {
                self.lost_while_connecting = Some(reason);
            }
            return;
        }
        tracing::warn!("{reason}");
        // The server ended the transaction with the session; there is no
        // cursor left to close.
        self.cursor = None;
        self.abandon_session_state();
        self.watch = None;
        self.pager = None;
        self.cancel_dispatcher = None;
        self.mode = Mode::Normal;
        // The last run's status ("… · M fetches more") and error speak of a
        // session that is gone; the card says what happened.
        self.last_status = None;
        self.last_error = None;
        self.last_error_detail = None;
        self.conn_state = ConnState::Failed(reason);
    }

    /// Spawn the connect + bootstrap-query task. The result returns as an
    /// `AppMsg` tagged with the current generation.
    pub(super) fn start_connect(&mut self) {
        // `--demo` promises "no database, no network". Every other
        // connect path is gated at the key handler, but `\c <name>`
        // reaches here directly — in a demo session that opened a real
        // TCP connection to whatever the fixture DSN names.
        if self.demo {
            self.last_status =
                Some("--demo has no server — restart without --demo to connect".into());
            return;
        }
        let Some(dsn) = self.dsn.clone() else {
            return;
        };
        // The database list belongs to the connection we're leaving.
        // Without this, a failed reconnect leaves `\l` (and the start
        // card's `databases` line) listing the *previous* server's
        // databases as if they were the new one's. Repopulated by the
        // bootstrap query on a successful connect.
        self.databases.clear();
        // Bump the generation so a late Booted/BootFailed from a prior
        // attempt can't clobber this one's state. The `on_msg` filter
        // already drops messages whose generation doesn't match; we just
        // need to make the field actually move.
        self.generation = self.generation.wrapping_add(1);
        // The cursor lives on the session being left: close it there
        // (the old pager queues the COMMIT) rather than leave it idle in
        // transaction until the old connection drops.
        self.close_cursor();
        self.abandon_session_state();
        self.conn_state = ConnState::Connecting;
        self.lost_while_connecting = None;
        self.in_recovery = None;
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        let read_only = self.read_only;
        let statement_timeout_ms = self.statement_timeout_ms;
        // Notice channel — server-emitted `RAISE NOTICE` / `WARNING` /
        // `INFO` flow through here. Forwarded into the App's main
        // message queue as `AppMsg::Notice` so a single select! loop
        // serves everything. Each connect gets a fresh pair so a
        // stale receiver from a prior session can't leak.
        let (notice_tx, mut notice_rx) = tokio::sync::mpsc::unbounded_channel::<conn::NoticeMsg>();
        let (notification_tx, mut notification_rx) =
            tokio::sync::mpsc::unbounded_channel::<conn::NotificationMsg>();
        let forward_tx = tx.clone();
        let notice_generation = generation;
        // The channel closes when the connection driver ends — the
        // session is over — so its end is the App's "connection lost",
        // with the server's parting FATAL (sent on it) as the reason.
        tokio::spawn(async move {
            let mut fatal = None;
            while let Some(notice) = notice_rx.recv().await {
                if notice.ends_session() {
                    fatal = Some(notice);
                    continue;
                }
                let msg = AppMsg::Notice {
                    generation: notice_generation,
                    notice,
                };
                if forward_tx.send(msg).is_err() {
                    return;
                }
            }
            let _ = forward_tx.send(AppMsg::ConnectionLost {
                generation: notice_generation,
                reason: super::msg::connection_lost_reason(fatal.as_ref()),
            });
        });
        // Same forwarding shape for LISTEN/NOTIFY arrivals. Each
        // connect gets its own channel pair so stale notifications
        // from a prior session can't leak into the new ring.
        let notify_forward_tx = tx.clone();
        let notify_generation = generation;
        tokio::spawn(async move {
            while let Some(n) = notification_rx.recv().await {
                let msg = AppMsg::Notification {
                    generation: notify_generation,
                    notification: n,
                };
                if notify_forward_tx.send(msg).is_err() {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            let msg = match conn::connect_and_bootstrap(
                dsn,
                read_only,
                statement_timeout_ms,
                BOOTSTRAP_SQL.to_string(),
                notice_tx,
                notification_tx,
            )
            .await
            {
                Ok(b) => AppMsg::Booted {
                    generation,
                    server_version: b.server_version,
                    in_recovery: b.in_recovery,
                    grid: b.grid,
                    client: b.client,
                    schema_cache: b.schema_cache,
                    tunnel: b.tunnel,
                },
                Err(error) => AppMsg::BootFailed { generation, error },
            };
            let _ = tx.send(msg);
        });
    }

    /// Enter `Mode::TapMonitor` and surface a one-line status
    /// summarising what the tap listener has seen so far. The
    /// status text covers both the "JAR connected, no traffic"
    /// case (when the ring is empty but heartbeats arrived) and
    /// the dominant "live stream" case.
    pub(super) fn start_tap_monitor(&mut self) {
        self.tap_nav.events_cursor = self
            .tap_nav
            .events_cursor
            .min(self.tap_events.len().saturating_sub(1));
        // The overlay is a layer of one visit: reopened, the monitor
        // starts on its list.
        self.tap_nav.event_detail = None;
        self.last_status = Some(self.tap_health.monitor_status(&self.tap_listeners));
        self.mode = Mode::TapMonitor;
    }

    /// Fire `SELECT pg_terminate_backend(<pid>)` against the
    /// live client. Result lands as a sessions refresh on
    /// success; error surfaces in `last_error` via the standard
    /// error pipeline. Routes around the safety guard because
    /// the operator just confirmed in the modal.
    pub(super) fn spawn_terminate_session(&mut self, pid: i32) {
        let Some(db) = self.db() else {
            self.last_error = Some(self.not_connected_message());
            return;
        };
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        self.last_status = Some(format!("terminating pid {pid}…"));
        tokio::spawn(async move {
            let client = db.ready().await;
            match conn::terminate_backend(&client, pid).await {
                Ok(()) => {
                    // Re-fetch sessions so the panel reflects the
                    // termination. Same panel SQL the `r` refresh
                    // uses.
                    let result =
                        match conn::run_query(&client, crate::query::sessions::PANEL_SQL).await {
                            Ok(grid) => Ok(crate::query::sessions::parse(&grid)),
                            Err(e) => Err(e),
                        };
                    let _ = tx.send(AppMsg::SessionsLoaded { generation, result });
                }
                Err(e) => {
                    let _ = tx.send(AppMsg::QueryFailed {
                        generation,
                        error: format!("terminate pid {pid} failed: {e}"),
                        position: None,
                        detail: None,
                    });
                }
            }
        });
    }

    /// Map the visible-row cursor (TableState index) to the actual
    /// `grid.rows` index, honouring any active filter. Returns
    /// `None` when nothing is selected or the visible set is empty.
    pub(crate) fn selected_grid_row_idx(&self) -> Option<usize> {
        let visible_idx = self.grid_state.selected()?;
        self.grid_view.visible_rows.get(visible_idx).copied()
    }

    /// Guard for the connection picker's Enter key and `\c <name>`:
    /// a pick whose url/username/password still carries an unresolved
    /// `${NAME}` (see `DataSourcePick::unresolved`) would otherwise
    /// hand `connect_and_bootstrap` a DSN with a literal `${NAME}`
    /// hostname — a DNS-shaped failure with no hint that the real
    /// cause was an unresolved Spring placeholder — or send the literal
    /// text as the password. Refuse up front instead, with a message
    /// naming a fix.
    ///
    /// `unresolved_host` is refused first and separately: a `${…}` in
    /// the host or port is never resolved at all, so "export it" would
    /// be wrong advice.
    ///
    /// Returns `true` (and sets `last_error`) when the pick was
    /// refused; `false` means the caller should proceed to
    /// `connect_to_pick`.
    pub(super) fn refuse_if_unresolved(&mut self, pick: &DataSourcePick) -> bool {
        if let Some(name) = pick.unresolved_host.first() {
            self.last_error = Some(format!(
                "${{{name}}} sits in the host of this connection — pgman never resolves \
                 a placeholder into a hostname (the value would leave the machine as a \
                 DNS lookup). Put a literal host in .pgman/pgman.toml"
            ));
            return true;
        }
        if let Some(name) = pick.unresolved.first() {
            self.last_error = Some(format!(
                "unresolved placeholder ${{{name}}} — export it, or put the connection in .pgman/pgman.toml"
            ));
            return true;
        }
        // Belt and braces. Every discovery source is supposed to have
        // marked its own placeholders above; this catches the one that
        // didn't. It matters because an unmarked `${…}` is not inert:
        // as a hostname it becomes a DNS lookup to whatever the
        // resolver makes of the literal text, and as a password it
        // goes on the wire.
        if let Some((field, body)) = crate::app::dsn_placeholder_field(pick.dsn.as_ref()) {
            self.last_error = Some(format!(
                "${{{body}}} is still a literal placeholder in the {field} of '{}' — \
                 export it, or put the connection in .pgman/pgman.toml",
                pick.name
            ));
            return true;
        }
        if pick.dsn.is_none() {
            // Belt and braces: discovery only keeps a DSN-less pick when
            // something above is also set, so this shouldn't be
            // reachable — but a silent no-op on Enter would be the worst
            // possible answer if it ever were.
            self.last_error = Some(format!(
                "'{}' has no usable connection URL — check the discovered config",
                pick.name
            ));
            return true;
        }
        false
    }

    /// The picker's Enter key and `\c <name>`: connect to a pick that
    /// came out of the working tree.
    ///
    /// Identical to `connect_to_pick` except for one gate: when the
    /// pick carries an `ssh_tunnel`, connecting spawns the system `ssh`
    /// binary — the operator's keys, agent and `~/.ssh/config` — against
    /// a bastion a committed file named, and it happens *before* any
    /// Postgres traffic, so "the database connection would have failed
    /// anyway" is no protection. That gets its own explicit `y`.
    ///
    /// A tunnel on a `--dsn` is the operator's own and never reaches
    /// here; nor does the `\c <dbname>` swap, whose tunnel (if any) was
    /// confirmed when the current connection was made.
    pub(super) fn connect_to_discovered_pick(&mut self, dsn: Dsn, origin: String) {
        if dsn.ssh_tunnel.is_some() {
            // The confirmation names where the credentials come from. The
            // pick is found by its DSN: the callers hand over only the DSN
            // and the origin string, and a discovered DSN is unique to its
            // pick in practice.
            let creds = self
                .conn_pick
                .picks
                .iter()
                .find(|p| p.dsn.as_ref() == Some(&dsn))
                .map(|p| p.creds.clone())
                .unwrap_or_default();
            self.pending_tunnel = Some(crate::app::PendingTunnel { dsn, origin, creds });
            // Stay in (or return to) the picker: it renders the
            // confirmation in place of the candidate list.
            self.mode = Mode::ConnPick;
            return;
        }
        self.connect_to_pick(dsn, origin);
    }

    /// Shared reconnect path for the connection picker's Enter key and
    /// `\c <name>`: resolve the safety profile against the picked
    /// dsn's database, install it, and hand off to `start_connect`.
    /// `origin` is the human-readable provenance shown in the status
    /// line / help ("picked X data source 'Y'", or a `\c`-driven
    /// database swap). Callers must check `refuse_if_unresolved` first.
    ///
    /// `--demo` is refused here, before anything moves. `start_connect`
    /// refuses it too, but by then this function had already swapped
    /// `dsn`, `dsn_origin`, `read_only` and `statement_timeout_ms` and
    /// dropped the operator into `Mode::Normal` — so `:connect` in a
    /// demo session reported "no server" while leaving the App
    /// pointing at a connection it never opened, with the *new*
    /// database's safety profile installed over the demo's.
    pub(super) fn connect_to_pick(&mut self, dsn: Dsn, origin: String) {
        self.connect_to_target(dsn, origin, false);
    }

    /// [`Self::connect_to_pick`], with `pin_read_only` opening the
    /// session read-only whatever the database's profile says — a
    /// replica, where a write can only fail.
    pub(super) fn connect_to_target(&mut self, dsn: Dsn, origin: String, pin_read_only: bool) {
        if self.demo {
            self.last_status =
                Some("--demo has no server — restart without --demo to connect".into());
            return;
        }
        let profile = self.safety_config.profile_for(&dsn.dbname);
        // The new database's profile decides the floor; a session
        // `:readonly on` still applies on top of it. Without the
        // override the toggle lasted exactly until the next connect
        // and then vanished with no message — see
        // `App::read_only_override`.
        self.read_only =
            profile.read_only || self.read_only_override == Some(true) || pin_read_only;
        self.replica_pin = pin_read_only;
        self.statement_timeout_ms = profile.statement_timeout_ms;
        self.dsn = Some(dsn);
        self.dsn_origin = Some(origin);
        self.mode = Mode::Normal;
        self.start_connect();
    }

    /// Open the data-source picker mid-session so the operator can
    /// switch connections without quitting. Requires at least one
    /// discovered data source — without that there's nothing
    /// meaningful to pick. Cancels any running query first so we
    /// don't waste a fire-and-forget run against a connection we're
    /// about to abandon. The picker's existing Enter handler does
    /// the actual reconnect.
    pub(super) fn start_connection_change(&mut self) {
        if self.conn_pick.picks.is_empty() {
            self.last_status = Some(no_picks_hint(self.dsn.is_some()).into());
            return;
        }
        if self.query_running {
            self.cancel_running_query();
        }
        // Opening the picker afresh must show the candidate list, never
        // a stale tunnel confirmation.
        self.pending_tunnel = None;
        self.conn_pick.index = 0;
        self.mode = Mode::ConnPick;
    }

    /// Send `EXPLAIN (FORMAT JSON)` for `sql`; the result lands as
    /// `AppMsg::CostPreviewLoaded`. The handler decides whether to
    /// confirm or proceed based on the row estimate vs threshold.
    pub(super) fn spawn_cost_preview(&mut self, sql: String, decision: Decision, threshold: u64) {
        let Some(db) = self.db() else {
            self.last_error = Some(self.not_connected_message());
            return;
        };
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        let explain_sql = format!("EXPLAIN (FORMAT JSON) {sql}");
        self.last_status = Some(format!(
            "pre-flight: explaining (threshold {threshold} rows)…"
        ));
        // Mark busy so the spinner shows, Ctrl-C cancel is offered,
        // and a second F5 doesn't fire while we're awaiting the
        // EXPLAIN. The CostPreviewLoaded handler clears the flag
        // before either spawning the real run (which sets it again)
        // or opening the Confirm modal.
        self.query_running = true;
        tokio::spawn(async move {
            let client = db.ready().await;
            let estimated = crate::query::explain::run_cost_explain(&client, &explain_sql).await;
            let _ = tx.send(AppMsg::CostPreviewLoaded {
                sql,
                decision,
                estimated,
                threshold,
                generation,
            });
        });
    }

    /// Push a statement that is about to run onto the history ring:
    /// consecutive duplicates skipped, capped at `HISTORY_CAP` entries
    /// (shared with the persistence side so the in-memory and on-disk
    /// rings can never drift), and any history browsing reset.
    pub(super) fn remember_in_history(&mut self, sql: &str) {
        if self.history.last().map(String::as_str) != Some(sql) {
            self.history.push(sql.to_string());
            if self.history.len() > HISTORY_CAP {
                self.history.remove(0);
            }
        }
        self.history_pos = None;
    }

    pub(super) fn spawn_run(
        &mut self,
        sql: String,
        kind: RunKind,
        decision: Decision,
        is_batch: bool,
    ) {
        if self.no_session() {
            self.last_error = Some(self.not_connected_message());
            return;
        }
        let paged = runs_behind_cursor(
            kind,
            is_batch,
            decision.wrap_in_tx,
            self.watch.is_some(),
            &sql,
        );
        let route = match self.pager.clone().filter(|_| paged) {
            Some(pager) => {
                // The pager closes the old cursor before it opens this one.
                self.cursor = None;
                RunRoute::Pager(pager.open(sql.clone(), crate::query::paging::PAGE_ROWS))
            }
            None => match self.db() {
                Some(db) => RunRoute::Db(db),
                None => return,
            },
        };
        self.remember_in_history(&sql);
        // Track the SQL of the most recent plain-Run so the
        // QueryOk handler can re-parse it for the source table.
        // EXPLAIN-wrapped runs hand back a JSON cell whose FROM is
        // the user's query — not the EXPLAIN itself — so we skip
        // them too; same for batch.
        self.last_run_sql = if matches!(kind, RunKind::Run) && !is_batch {
            Some(sql.clone())
        } else {
            None
        };
        // If this run changes the schema (DDL), mark the cache dirty so a
        // successful QueryOk triggers a background re-fetch — otherwise
        // completion / browser / lint / FK-nav stay stale until reconnect.
        // EXPLAIN kinds don't execute the wrapped DDL, so only plain Run counts.
        self.schema_dirty_after_run =
            matches!(kind, RunKind::Run) && crate::safety::changes_schema(&sql);
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        let wrap_in_tx = decision.wrap_in_tx;
        let is_run = matches!(kind, RunKind::Run);
        self.query_running = true;
        self.query_started = Some(Instant::now());
        self.last_error = None;
        self.last_status = Some(running_status(kind));
        tokio::spawn(async move {
            let (result, paging) = match route {
                RunRoute::Db(db) => {
                    let client = db.ready().await;
                    (
                        execute_with_origins(&client, &sql, kind, &decision, is_batch).await,
                        None,
                    )
                }
                RunRoute::Pager(rx) => match rx.await {
                    // A shadowed call ran unpaged and is no result for `M`
                    // to re-run (each run may write): it is an unpaged
                    // result, `capped at 1000` like the Db route's.
                    Ok(Ok(opened)) => (
                        Ok((opened.grid, opened.origins)),
                        (!opened.shadowed).then_some(RunPaging {
                            sql,
                            cursor: opened.cursor,
                            in_transaction: opened.in_transaction,
                            standby: opened.standby,
                        }),
                    ),
                    Ok(Err(e)) => (Err(e), None),
                    Err(_) => (
                        Err(conn::QueryErr::msg("the connection's pager stopped")),
                        None,
                    ),
                },
            };
            // Run + wrap_in_tx leaves the transaction open on success — the
            // caller will need to commit or rollback.
            let tx_open_after = is_run && wrap_in_tx && result.is_ok();
            let msg = match result {
                Ok((grid, origins)) => AppMsg::QueryOk {
                    generation,
                    grid,
                    origins,
                    kind_label: kind.label().to_string(),
                    tx_open_after,
                    paging,
                },
                Err(err) => AppMsg::QueryFailed {
                    generation,
                    error: err.msg,
                    position: err.position,
                    detail: err.detail,
                },
            };
            let _ = tx.send(msg);
        });
    }

    /// `--demo`'s counterpart to `spawn_run`: no client, no
    /// `tokio::spawn` — the grid comes from `demo::answer`
    /// synchronously and lands on the exact same `AppMsg::QueryOk`
    /// a real client's `execute` would produce, sent to `msg_tx`
    /// immediately. History, `\x`, tabs, and the status line all go
    /// through the same `on_msg` handler a live run uses, so they
    /// can't drift from it.
    ///
    /// Only reached from `request_run` / `request_run_batch` after
    /// `safety::evaluate` already said `Guard::Allow` — a demo
    /// DELETE without WHERE hits `Guard::Block` same as it would
    /// live and never gets here.
    pub(super) fn spawn_run_demo(&mut self, sql: String, kind: RunKind) {
        self.remember_in_history(&sql);
        self.last_run_sql = if matches!(kind, RunKind::Run) {
            Some(sql.clone())
        } else {
            None
        };
        // The demo schema cache never changes shape — nothing to
        // re-fetch after a (guard-permitting) DDL-shaped statement.
        self.schema_dirty_after_run = false;
        self.last_error = None;
        // `\timing` reads `query_started` in the QueryOk handler and
        // shows nothing without it — so `\timing on` in `--demo` used
        // to print no elapsed line at all, which is exactly the kind
        // of drift the demo exists to avoid. The clock starts here and
        // the answer is synthesized immediately below, so the figure
        // the demo shows is the real time this run took.
        self.query_started = Some(Instant::now());
        // EXPLAIN never reaches `demo::answer`: that projects *result*
        // columns, so the grid's first cell was a row value,
        // `explain::parse` failed on it, and the demo fell back to raw
        // text every time — EXPLAIN was the one feature `--demo` could
        // never show. A canned plan grid is the same shape Postgres
        // returns, so `AppMsg::QueryOk`'s handler opens
        // `Mode::ExplainTree` through the identical path.
        let grid = match kind {
            RunKind::Explain => crate::demo::explain_plan_grid(false, &sql, &self.schema_cache),
            RunKind::ExplainAnalyze => {
                crate::demo::explain_plan_grid(true, &sql, &self.schema_cache)
            }
            RunKind::Run => crate::demo::answer(&sql, &self.schema_cache),
        };
        let msg = AppMsg::QueryOk {
            generation: self.generation,
            grid,
            // A canned answer was read from no table.
            origins: Vec::new(),
            kind_label: kind.label().to_string(),
            // Demo never actually opens a transaction — there's no
            // connection for a COMMIT/ROLLBACK to run against.
            tx_open_after: false,
            paging: None,
        };
        let _ = self.msg_tx.send(msg);
    }

    // -- grid nav --

    /// Reset the per-grid view state — sort / filter / column cursor
    /// — so a fresh result set starts clean. Called whenever a new
    /// `Grid` lands on the App via `QueryOk` or `Booted`.
    pub(crate) fn reset_grid_view(&mut self) {
        self.grid_view.col_cursor = 0;
        self.grid_view.sort = None;
        self.grid_view.raw_rows = None;
        self.grid_view.filter = None;
        // Clear the inferred source table too. Otherwise a reconnect (Booted
        // calls reset_grid_view but never sets source) leaves the PREVIOUS
        // connection's `(schema, table)` in place, so `I` (row→INSERT) on the
        // new connection's bootstrap grid yanks an INSERT against a table from
        // the old database. QueryOk re-infers source immediately after this on
        // the query path, so behaviour there is unchanged.
        self.grid_view.source = None;
        self.grid_view.origins.clear();
        // A new grid landed — bookmarks keyed by the old grid's row indices
        // would resolve against unrelated rows, so drop them. (Per-tab
        // bookmarks are snapshotted/restored separately on tab switch.)
        self.bookmarks.clear();
        self.rebuild_visible_rows();
    }

    /// Re-fetch the schema cache in the background after a DDL run and
    /// deliver it as `AppMsg::SchemaRefreshed`. Keeps completion, the schema
    /// browser, lint, and FK-nav current without forcing a full reconnect.
    ///
    /// Lock-bounded as the connect's fetch is, so another session's lock
    /// on a catalog can't stall it for `statement_timeout` (or for good);
    /// the session is held meanwhile, so nothing of the operator's runs
    /// under the bound.
    pub(super) fn spawn_schema_refresh(&mut self) {
        let Some(db) = self.db_exclusive() else {
            return;
        };
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        tokio::spawn(async move {
            let (client, _held) = db.ready().await;
            let schema_cache = crate::query::schema::fetch_bounded(&client).await;
            let _ = tx.send(AppMsg::SchemaRefreshed {
                generation,
                schema_cache,
            });
        });
    }

    pub(super) fn spawn_slow_queries_load(&self, db: crate::query::paging::Db) {
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        tokio::spawn(async move {
            let client = db.ready().await;
            let result = match conn::run_query(&client, crate::query::slow_queries::PANEL_SQL).await
            {
                Ok(grid) => Ok(crate::query::slow_queries::parse(&grid)),
                Err(e) => Err(e),
            };
            let _ = tx.send(AppMsg::SlowQueriesLoaded { generation, result });
        });
    }

    /// Gather every environment with the installed loader: on a blocking
    /// thread for the real one (the aws CLI runs there), at once for a
    /// fixture. `false` when no loader is installed.
    pub(super) fn spawn_environments_load(&mut self) -> bool {
        match self.environments_loader.clone() {
            None => false,
            Some(EnvironmentsLoader::Immediate(reports)) => {
                self.on_msg(AppMsg::EnvironmentsLoaded(Ok(reports)));
                true
            }
            Some(EnvironmentsLoader::Blocking(load)) => {
                let tx = self.msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = tx.send(AppMsg::EnvironmentsLoaded(load()));
                });
                true
            }
        }
    }

    /// Read an environment connect's secret on a blocking thread (the
    /// aws CLI runs there). `false` when no fetcher is installed.
    pub(super) fn spawn_secret_fetch(
        &self,
        token: u64,
        secret: crate::creds::aws_secrets::SecretRef,
    ) -> bool {
        let Some(fetch) = self.secret_fetcher.clone() else {
            return false;
        };
        let tx = self.msg_tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(AppMsg::EnvSecretResolved {
                token,
                result: fetch(&secret),
            });
        });
        true
    }

    pub(super) fn spawn_sessions_load(&self, db: crate::query::paging::Db) {
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        tokio::spawn(async move {
            let client = db.ready().await;
            let result = match conn::run_query(&client, crate::query::sessions::PANEL_SQL).await {
                Ok(grid) => Ok(crate::query::sessions::parse(&grid)),
                Err(e) => Err(e),
            };
            let _ = tx.send(AppMsg::SessionsLoaded { generation, result });
        });
    }

    /// Fire the crates.io update check. Production spawns a real
    /// `tokio::spawn` awaiting `update_check::check_async()`; when a
    /// test has injected `update_check_spawn`, that hook runs
    /// instead — synchronously if the test wants it to be, so
    /// `run_with`'s ordering (spawn strictly after the first draw)
    /// is directly observable without touching the network.
    pub(super) fn spawn_update_check(&self) {
        let tx = self.msg_tx.clone();
        match &self.update_check_spawn {
            Some(hook) => hook(tx),
            None => {
                tokio::spawn(async move {
                    let latest = crate::update_check::check_async().await;
                    let _ = tx.send(AppMsg::UpdateCheck(latest));
                });
            }
        }
    }
}
