use super::*;

impl App {
    /// Copy the focused notification's payload to the clipboard.
    pub(super) fn yank_focused_notification(&mut self) {
        let Some(n) = self.notifications.items.get(self.notifications.cursor) else {
            return;
        };
        let text = n.payload.clone();
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text.clone())) {
            Ok(()) => {
                self.last_status =
                    Some(format!("yanked payload ({} char(s))", text.chars().count()));
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
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(path.clone())) {
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
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(value.to_string())) {
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
        let cols = self
            .grid
            .columns
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let vals = row
            .iter()
            .map(|s| format_sql_literal(s))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("INSERT INTO {schema}.{table} ({cols}) VALUES ({vals});");
        match arboard::Clipboard::new() {
            Ok(mut cb) => match cb.set_text(sql.clone()) {
                Ok(()) => {
                    self.last_status = Some(format!(
                        "copied INSERT for {schema}.{table} · {} char(s)",
                        sql.len()
                    ));
                }
                Err(e) => self.last_error = Some(format!("clipboard write: {e}")),
            },
            Err(e) => self.last_error = Some(format!("clipboard init: {e}")),
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
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(snippet.clone())) {
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
    fn focused_schema_browser_table(&self) -> Option<(String, String)> {
        let rows = self.flattened_schema_browser();
        match rows.get(self.schema_browser.cursor)? {
            SchemaBrowserRow::Table { schema, name, .. } => Some((schema.clone(), name.clone())),
            SchemaBrowserRow::Column { schema, table, .. } => Some((schema.clone(), table.clone())),
            SchemaBrowserRow::Constraint { schema, table, .. } => {
                Some((schema.clone(), table.clone()))
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
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(sql.clone())) {
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
            self.last_error = Some(format!("no column info cached for {schema}.{table}"));
            return;
        }
        let sql = build_insert_template(&schema, &table, &cols);
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(sql.clone())) {
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
}
