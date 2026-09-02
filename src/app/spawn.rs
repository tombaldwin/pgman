use super::*;

impl App {
    /// Spawn the connect + bootstrap-query task. The result returns as an
    /// `AppMsg` tagged with the current generation.
    pub(super) fn start_connect(&mut self) {
        let Some(dsn) = self.dsn.clone() else {
            return;
        };
        // Bump the generation so a late Booted/BootFailed from a prior
        // attempt can't clobber this one's state. The `on_msg` filter
        // already drops messages whose generation doesn't match; we just
        // need to make the field actually move.
        self.generation = self.generation.wrapping_add(1);
        // Abandon any in-flight query state from the prior generation. The
        // query's QueryOk/QueryFailed (which would normally clear these) is
        // tagged with the OLD generation and will be dropped by the on_msg
        // stale-generation filter, so without this the flag would stay stuck
        // true — a permanent spinner that blocks every new run on the new
        // connection. This is the single choke point for all reconnects.
        self.query_running = false;
        self.query_started = None;
        self.conn_state = ConnState::Connecting;
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
        tokio::spawn(async move {
            while let Some(notice) = notice_rx.recv().await {
                let msg = AppMsg::Notice {
                    generation: notice_generation,
                    notice,
                };
                if forward_tx.send(msg).is_err() {
                    break;
                }
            }
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
        let queries = self.tap_health.query_count;
        let beats = self.tap_health.heartbeat_count;
        let dropped = self.tap_health.dropped_events_total;
        self.last_status = Some(if queries == 0 && beats == 0 {
            "JDBC tap · no events yet · start pgman with --tap-listen and configure pgman-tap in the JVM".into()
        } else {
            let dropped_suffix = if dropped > 0 {
                format!(" · {dropped} dropped (JAR backpressure)")
            } else {
                String::new()
            };
            format!("JDBC tap · {queries} queries · {beats} heartbeats{dropped_suffix}")
        });
        self.mode = Mode::TapMonitor;
    }

    /// Fire `SELECT pg_terminate_backend(<pid>)` against the
    /// live client. Result lands as a sessions refresh on
    /// success; error surfaces in `last_error` via the standard
    /// error pipeline. Routes around the safety guard because
    /// the operator just confirmed in the modal.
    pub(super) fn spawn_terminate_session(&mut self, pid: i32) {
        let Some(client) = self.client.clone() else {
            self.last_error = Some(self.not_connected_message());
            return;
        };
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        self.last_status = Some(format!("terminating pid {pid}…"));
        tokio::spawn(async move {
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
    /// a pick whose url/username still carries an unresolved
    /// `${NAME}` (see `DataSourcePick::unresolved`) would otherwise
    /// hand `connect_and_bootstrap` a DSN with a literal `${NAME}`
    /// hostname — a DNS-shaped failure with no hint that the real
    /// cause was an unresolved Spring placeholder. Refuse up front
    /// instead, with a message naming a fix. Returns `true` (and sets
    /// `last_error`) when the pick was refused; `false` means the
    /// caller should proceed to `connect_to_pick`.
    pub(super) fn refuse_if_unresolved(&mut self, pick: &DataSourcePick) -> bool {
        let Some(name) = pick.unresolved.first() else {
            return false;
        };
        self.last_error = Some(format!(
            "unresolved placeholder ${{{name}}} — export it, or put the connection in .pgman/pgman.toml"
        ));
        true
    }

    /// Shared reconnect path for the connection picker's Enter key and
    /// `\c <name>`: resolve the safety profile against the picked
    /// dsn's database, install it, and hand off to `start_connect`.
    /// `origin` is the human-readable provenance shown in the status
    /// line / help ("picked X data source 'Y'", or a `\c`-driven
    /// database swap). Callers must check `refuse_if_unresolved` first.
    pub(super) fn connect_to_pick(&mut self, dsn: Dsn, origin: String) {
        let profile = self.safety_config.profile_for(&dsn.dbname);
        self.read_only = profile.read_only;
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
            self.last_status = Some(
                "no data sources to pick — pass --dsn or add `[[connections]]` to pgman.toml"
                    .into(),
            );
            return;
        }
        if self.query_running {
            self.cancel_running_query();
        }
        self.conn_pick.index = 0;
        self.mode = Mode::ConnPick;
    }

    /// Send `EXPLAIN (FORMAT JSON)` for `sql`; the result lands as
    /// `AppMsg::CostPreviewLoaded`. The handler decides whether to
    /// confirm or proceed based on the row estimate vs threshold.
    pub(super) fn spawn_cost_preview(&mut self, sql: String, decision: Decision, threshold: u64) {
        let Some(client) = self.client.clone() else {
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

    pub(super) fn spawn_run(
        &mut self,
        sql: String,
        kind: RunKind,
        decision: Decision,
        is_batch: bool,
    ) {
        let Some(client) = self.client.clone() else {
            self.last_error = Some(self.not_connected_message());
            return;
        };
        // Push to history (skip consecutive duplicates, cap at
        // HISTORY_CAP entries — shared with the persistence side
        // so the in-memory + on-disk rings can never drift).
        if self.history.last() != Some(&sql) {
            self.history.push(sql.clone());
            if self.history.len() > HISTORY_CAP {
                self.history.remove(0);
            }
        }
        self.history_pos = None;
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
        self.last_status = Some(format!("running {}…", kind.label()));
        tokio::spawn(async move {
            let result = execute(&client, &sql, kind, &decision, is_batch).await;
            // Run + wrap_in_tx leaves the transaction open on success — the
            // caller will need to commit or rollback.
            let tx_open_after = is_run && wrap_in_tx && result.is_ok();
            let msg = match result {
                Ok(grid) => AppMsg::QueryOk {
                    generation,
                    grid,
                    kind_label: kind.label().to_string(),
                    tx_open_after,
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
        // A new grid landed — bookmarks keyed by the old grid's row indices
        // would resolve against unrelated rows, so drop them. (Per-tab
        // bookmarks are snapshotted/restored separately on tab switch.)
        self.bookmarks.clear();
        self.rebuild_visible_rows();
    }

    /// Re-fetch the schema cache in the background after a DDL run and
    /// deliver it as `AppMsg::SchemaRefreshed`. Keeps completion, the schema
    /// browser, lint, and FK-nav current without forcing a full reconnect.
    pub(super) fn spawn_schema_refresh(&self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        tokio::spawn(async move {
            let schema_cache = crate::query::schema::fetch(&client).await;
            let _ = tx.send(AppMsg::SchemaRefreshed {
                generation,
                schema_cache,
            });
        });
    }

    pub(super) fn spawn_slow_queries_load(&self, client: std::sync::Arc<tokio_postgres::Client>) {
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        tokio::spawn(async move {
            let result = match conn::run_query(&client, crate::query::slow_queries::PANEL_SQL).await
            {
                Ok(grid) => Ok(crate::query::slow_queries::parse(&grid)),
                Err(e) => Err(e),
            };
            let _ = tx.send(AppMsg::SlowQueriesLoaded { generation, result });
        });
    }

    pub(super) fn spawn_sessions_load(&self, client: std::sync::Arc<tokio_postgres::Client>) {
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        tokio::spawn(async move {
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
