use super::*;

impl App {
    /// Copy the focused notification's payload to the clipboard.
    pub(super) fn yank_focused_notification(&mut self) {
        let Some(n) = self.notifications.items.get(self.notifications.cursor) else {
            return;
        };
        let text = n.payload.clone();
        match write_clipboard(&text) {
            Ok(()) => {
                self.last_status =
                    Some(format!("yanked payload · {} char(s)", text.chars().count()));
                self.last_error = None;
            }
            Err(e) => self.last_error = Some(format!("yank failed: {e}")),
        }
    }

    /// Yank the focused JSON node's jq-style path (`.foo[0].bar`) to
    /// the clipboard. The root node yanks `.` for convenience.
    pub(super) fn yank_json_cell_path(&mut self) {
        let Some(row) = self.cell_detail.json_rows.get(self.cell_detail.json_cursor) else {
            return;
        };
        let path = if row.path.is_empty() {
            ".".to_string()
        } else {
            row.path.clone()
        };
        match write_clipboard(&path) {
            Ok(()) => {
                self.last_status = Some(format!("yanked path '{path}'"));
                self.last_error = None;
            }
            Err(e) => {
                self.last_error = Some(format!("yank failed: {e}"));
            }
        }
    }

    /// Copy the currently-focused field's value to the system clipboard.
    /// Surfaces success / failure via `last_status` / `last_error`.
    pub(super) fn yank_focused_field(&mut self) {
        let Some(idx) = self.selected_grid_row_idx() else {
            return;
        };
        let Some(row) = self.grid.rows.get(idx) else {
            return;
        };
        let Some(value) = row.get(self.row_detail.field) else {
            return;
        };
        let column = self
            .grid
            .columns
            .get(self.row_detail.field)
            .cloned()
            .unwrap_or_default();
        // A NULL has no text to copy; copying "" would clobber the
        // clipboard with something that is not the value either.
        let Some(value) = value.as_deref() else {
            self.last_status = Some(format!("'{column}' is NULL · nothing yanked"));
            self.last_error = None;
            return;
        };
        match write_clipboard(value) {
            Ok(()) => {
                let chars = value.chars().count();
                self.last_status = Some(format!("yanked '{column}' · {chars} char(s)"));
                self.last_error = None;
            }
            Err(e) => {
                self.last_error = Some(format!("yank failed: {e}"));
            }
        }
    }

    pub(super) fn yank_row_as_insert(&mut self) {
        let Some((schema, table)) = self.grid_view.source.clone() else {
            self.last_error = Some(
                "can't infer source table — row-as-INSERT only works for single-table SELECTs"
                    .into(),
            );
            return;
        };
        let Some(idx) = self.selected_grid_row_idx() else {
            return;
        };
        let Some(row) = self.grid.rows.get(idx) else {
            return;
        };
        let sql = row_insert_sql(&schema, &table, &self.grid.columns, row);
        match write_clipboard(&sql) {
            Ok(()) => {
                self.last_status = Some(format!(
                    "copied INSERT for {schema}.{table} · {} char(s)",
                    sql.chars().count()
                ));
                self.last_error = None;
            }
            Err(e) => self.last_error = Some(format!("yank failed: {e}")),
        }
    }

    /// Yank the focused finding's `suggestion` (an SQL snippet)
    /// to the clipboard so the operator can paste it into the
    /// editor. Surfaces an actionable status when the finding has
    /// no suggestion (LINT002 / LINT003 / LINT004 are advisory).
    pub(super) fn yank_schema_lint_suggestion(&mut self) {
        let Some(finding) = self.schema_lint.findings.get(self.schema_lint.cursor) else {
            return;
        };
        let Some(snippet) = finding.suggestion.clone() else {
            self.last_status = Some(format!(
                "{}: no SQL suggestion — advisory finding",
                finding.code
            ));
            return;
        };
        match write_clipboard(&snippet) {
            Ok(()) => {
                self.last_status = Some(format!("yanked {} suggestion", finding.code));
                self.last_error = None;
            }
            Err(e) => self.last_error = Some(format!("yank failed: {e}")),
        }
    }

    /// Resolve the focused schema-browser row to its owning
    /// (schema, table) — returns None when focused on a Schema row
    /// (no table context) or when the cursor is out-of-bounds.
    /// Column / Constraint rows resolve to their parent table.
    pub(super) fn focused_schema_browser_table(&self) -> Option<(String, String)> {
        let rows = self.flattened_schema_browser();
        match rows.get(self.schema_browser.cursor)? {
            SchemaBrowserRow::Table { schema, name, .. } => Some((schema.clone(), name.clone())),
            SchemaBrowserRow::Column { schema, table, .. } => Some((schema.clone(), table.clone())),
            SchemaBrowserRow::Constraint { schema, table, .. }
            | SchemaBrowserRow::Index { schema, table, .. } => {
                Some((schema.clone(), table.clone()))
            }
            SchemaBrowserRow::Partitions { schema, table, .. } => {
                Some((schema.clone(), table.clone()))
            }
            // A partition is a table in its own right: s / i target it.
            SchemaBrowserRow::Partition { schema, name, .. } => {
                Some((schema.clone(), name.clone()))
            }
            SchemaBrowserRow::Schema { .. } => None,
        }
    }

    pub(super) fn yank_schema_browser_select(&mut self) {
        let Some((schema, table)) = self.focused_schema_browser_table() else {
            self.last_error = Some("focus a table, column, or constraint first".into());
            return;
        };
        let sql = build_select_all_template(&schema, &table);
        match write_clipboard(&sql) {
            Ok(()) => {
                self.last_status = Some(format!("yanked SELECT template for {schema}.{table}"));
                self.last_error = None;
            }
            Err(e) => self.last_error = Some(format!("yank failed: {e}")),
        }
    }

    pub(super) fn yank_schema_browser_insert(&mut self) {
        let Some((schema, table)) = self.focused_schema_browser_table() else {
            self.last_error = Some("focus a table, column, or constraint first".into());
            return;
        };
        let cols = self
            .schema_cache
            .columns_by_table
            .get(&(schema.clone(), table.clone()))
            .cloned()
            .unwrap_or_default();
        if cols.is_empty() {
            self.last_error = Some(format!(
                "no columns known for {schema}.{table} yet · S opens the schema browser"
            ));
            return;
        }
        let sql = build_insert_template(&schema, &table, &cols);
        match write_clipboard(&sql) {
            Ok(()) => {
                self.last_status = Some(format!(
                    "yanked INSERT template for {schema}.{table} · {} col(s)",
                    cols.len()
                ));
                self.last_error = None;
            }
            Err(e) => self.last_error = Some(format!("yank failed: {e}")),
        }
    }

    /// `d` in the schema browser: copy the focused table's `CREATE`
    /// statement. Live, the pieces come from the catalog
    /// (`query::ddl::fetch`, answered by `AppMsg::DdlLoaded`); `--demo`
    /// builds them from its own schema at once.
    pub(super) fn yank_schema_browser_ddl(&mut self) {
        let Some((schema, table)) = self.focused_schema_browser_table() else {
            self.last_error = Some("focus a table, column, or constraint first".into());
            return;
        };
        let label = format!("{schema}.{table}");
        if self.demo && self.client.is_none() {
            let result = crate::demo::table_ddl(&schema, &table)
                .ok_or_else(|| format!("{label}: no DDL in --demo"));
            self.on_ddl_loaded(&label, result);
            return;
        }
        let Some(db) = self.db() else {
            self.last_error = Some(self.not_connected_message());
            return;
        };
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        self.last_status = Some(format!("reading the DDL of {label}…"));
        tokio::spawn(async move {
            let client = db.ready().await;
            let result = crate::query::ddl::fetch(&client, &schema, &table).await;
            let _ = tx.send(AppMsg::DdlLoaded {
                generation,
                table: label,
                result,
            });
        });
    }

    /// The DDL arrived: copy the statement, or say why there isn't one.
    pub(super) fn on_ddl_loaded(
        &mut self,
        table: &str,
        result: Result<crate::query::ddl::TableDdl, String>,
    ) {
        let ddl = match result {
            Ok(ddl) => ddl,
            Err(e) => {
                self.last_error = Some(format!("DDL of {table}: {e}"));
                return;
            }
        };
        let sql = crate::query::ddl::create_statement(&ddl);
        match write_clipboard(&sql) {
            Ok(()) => {
                self.last_status = Some(format!(
                    "copied the DDL of {table} · {} line(s)",
                    sql.lines().count()
                ));
                self.last_error = None;
            }
            Err(e) => self.last_error = Some(format!("yank failed: {e}")),
        }
    }
}

/// Put `text` on the system clipboard. Unit tests never touch the real
/// one: under `cfg(test)` the text goes to a per-thread slot that
/// [`take_test_clipboard`] reads back.
#[cfg(not(test))]
pub(crate) fn write_clipboard(text: &str) -> Result<(), String> {
    arboard::Clipboard::new()
        .and_then(|mut cb| cb.set_text(text.to_string()))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
thread_local! {
    static TEST_CLIPBOARD: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn write_clipboard(text: &str) -> Result<(), String> {
    TEST_CLIPBOARD.with(|c| *c.borrow_mut() = Some(text.to_string()));
    Ok(())
}

/// What the last [`write_clipboard`] on this thread wrote, taken.
#[cfg(test)]
pub(crate) fn take_test_clipboard() -> Option<String> {
    TEST_CLIPBOARD.with(|c| c.borrow_mut().take())
}
