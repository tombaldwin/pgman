//! Pending-patch discovery for projects that record applied schema
//! patches in a version table — examplecorp's `schema_updates/NNN-*.sql`, each
//! starting `INSERT INTO t_schema_version (schema_version_id, …)
//! VALUES (NNN, …)`. Configured, not hardcoded: a `[patches]` block names
//! the directory, the table and the id column.
//!
//! A patch's id is read from its own version-row `INSERT`, because that
//! is the value the database records — the filename's number is reported
//! beside it, and a disagreement between the two is flagged rather than
//! resolved. Pure: the caller lists the directory and reads the database.

use crate::query::preflight::plain_ident_path;

/// The `[patches]` block.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct PatchesConfig {
    /// Directory of patch files, relative to the project root.
    pub dir: String,
    /// The version table (`name` or `schema.name`).
    pub table: String,
    /// The column holding a patch's id.
    pub column: String,
}

/// One `.sql` file in the patch directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchFile {
    pub file: String,
    /// Leading digits of the filename (`248-…` → 248, `161a_…` → 161).
    pub filename_number: Option<i64>,
    /// The id its version-row `INSERT` records; `None` when it has none.
    pub version_id: Option<i64>,
}

/// Build a [`PatchFile`] from a file's name and text.
pub fn patch_file(file: &str, text: &str, cfg: &PatchesConfig) -> PatchFile {
    PatchFile {
        file: file.to_string(),
        filename_number: filename_number(file),
        version_id: version_id_in(text, &cfg.table, &cfg.column),
    }
}

pub fn filename_number(file: &str) -> Option<i64> {
    let digits: String = file.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// `sql` with `--` and `/* */` comments replaced by spaces, string
/// literals kept intact (a `--` inside `'…'` is not a comment).
fn strip_comments(sql: &str) -> String {
    let b: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            '\'' => {
                out.push('\'');
                i += 1;
                while i < b.len() {
                    out.push(b[i]);
                    if b[i] == '\'' {
                        if b.get(i + 1) == Some(&'\'') {
                            out.push('\'');
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            '-' if b.get(i + 1) == Some(&'-') => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
                out.push(' ');
            }
            '/' if b.get(i + 1) == Some(&'*') => {
                i += 2;
                while i < b.len() && !(b[i] == '*' && b.get(i + 1) == Some(&'/')) {
                    i += 1;
                }
                i += 2;
                out.push(' ');
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// The text inside the parenthesised group starting at `s[open]` (which
/// must be `(`), respecting quotes and nesting; `None` if unbalanced.
fn paren_group(s: &[char], open: usize) -> Option<(String, usize)> {
    let mut depth = 0usize;
    let mut i = open;
    let mut in_str = false;
    while i < s.len() {
        let c = s[i];
        if in_str {
            if c == '\'' {
                if s.get(i + 1) == Some(&'\'') {
                    i += 2;
                    continue;
                }
                in_str = false;
            }
        } else if c == '\'' {
            in_str = true;
        } else if c == '(' {
            depth += 1;
        } else if c == ')' {
            depth -= 1;
            if depth == 0 {
                return Some((s[open + 1..i].iter().collect(), i + 1));
            }
        }
        i += 1;
    }
    None
}

/// Split on commas at depth 0, outside quotes.
fn split_top_level(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let (mut depth, mut in_str) = (0usize, false);
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            cur.push(c);
            if c == '\'' {
                if chars.get(i + 1) == Some(&'\'') {
                    cur.push('\'');
                    i += 2;
                    continue;
                }
                in_str = false;
            }
        } else {
            match c {
                '\'' => {
                    in_str = true;
                    cur.push(c);
                }
                '(' => {
                    depth += 1;
                    cur.push(c);
                }
                ')' => {
                    depth = depth.saturating_sub(1);
                    cur.push(c);
                }
                ',' if depth == 0 => parts.push(std::mem::take(&mut cur)),
                _ => cur.push(c),
            }
        }
        i += 1;
    }
    parts.push(cur);
    parts.into_iter().map(|p| p.trim().to_string()).collect()
}

/// An identifier as Postgres would fold it: unquoted → lowercase,
/// `"quoted"` → verbatim.
fn fold_ident(s: &str) -> String {
    let s = s.trim();
    match s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        Some(q) => q.replace("\"\"", "\""),
        None => s.to_ascii_lowercase(),
    }
}

/// The id the file's first `INSERT INTO <table> (…) VALUES (…)` records
/// `sql` with each `$tag$ … $tag$` body replaced by a space. Pure.
fn blank_dollar_quotes(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    for span in crate::safety::scan(sql) {
        match span.kind {
            crate::safety::SpanKind::DollarQuoted => out.push(' '),
            _ => out.push_str(&sql[span.start..span.end]),
        }
    }
    out
}

/// in `column`, when that value is an integer literal.
pub fn version_id_in(sql: &str, table: &str, column: &str) -> Option<i64> {
    let (want_schema, want_table) = plain_ident_path(table).ok()?;
    let want_col = column.to_ascii_lowercase();
    // A dollar-quoted body is text (a function's source, say): an
    // `INSERT` in it isn't this file's version row.
    let text = strip_comments(&blank_dollar_quotes(sql));
    let lower = text.to_ascii_lowercase();
    let chars: Vec<char> = text.chars().collect();
    // Byte and char offsets agree only for ASCII; patch files are SQL
    // and overwhelmingly ASCII, but search on chars to stay correct.
    let lower_chars: Vec<char> = lower.chars().collect();
    let needle: Vec<char> = "insert".chars().collect();
    let mut i = 0;
    while i + needle.len() <= lower_chars.len() {
        // An `INSERT` inside a string literal is text, not a statement.
        if chars[i] == '\'' {
            i = past_string(&chars, i);
            continue;
        }
        if lower_chars[i..i + needle.len()] != needle[..]
            || (i > 0 && is_word(lower_chars[i - 1]))
            || lower_chars
                .get(i + needle.len())
                .is_some_and(|c| is_word(*c))
        {
            i += 1;
            continue;
        }
        let mut j = skip_ws(&chars, i + needle.len());
        let Some(after_into) = keyword_at(&lower_chars, j, "into") else {
            i += 1;
            continue;
        };
        j = skip_ws(&chars, after_into);
        let start = j;
        while j < chars.len() && (is_word(chars[j]) || matches!(chars[j], '.' | '"' | '$')) {
            j += 1;
        }
        let name: String = chars[start..j].iter().collect();
        let mut parts = name.split('.').map(fold_ident).collect::<Vec<_>>();
        let tbl = parts.pop().unwrap_or_default();
        let sch = parts.pop();
        let schema_ok = match (&want_schema, &sch) {
            (Some(w), Some(s)) => w == s,
            (Some(_), None) | (None, _) => true,
        };
        if tbl != want_table || !schema_ok {
            i += 1;
            continue;
        }
        j = skip_ws(&chars, j);
        if chars.get(j) != Some(&'(') {
            return None;
        }
        let (cols, after_cols) = paren_group(&chars, j)?;
        j = skip_ws(&chars, after_cols);
        let after_values = keyword_at(&lower_chars, j, "values")?;
        j = skip_ws(&chars, after_values);
        if chars.get(j) != Some(&'(') {
            return None;
        }
        let (vals, _) = paren_group(&chars, j)?;
        let cols: Vec<String> = split_top_level(&cols)
            .iter()
            .map(|c| fold_ident(c))
            .collect();
        let vals = split_top_level(&vals);
        let idx = cols.iter().position(|c| *c == want_col)?;
        return vals.get(idx)?.trim().parse::<i64>().ok();
    }
    None
}

/// The index after the `'…'` literal starting at `s[open]`.
fn past_string(s: &[char], open: usize) -> usize {
    let mut i = open + 1;
    while i < s.len() {
        if s[i] == '\'' {
            if s.get(i + 1) == Some(&'\'') {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    s.len()
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn skip_ws(s: &[char], mut i: usize) -> usize {
    while i < s.len() && s[i].is_whitespace() {
        i += 1;
    }
    i
}

/// If `kw` sits at `s[i..]` as a whole word, the index after it.
fn keyword_at(s: &[char], i: usize, kw: &str) -> Option<usize> {
    let k: Vec<char> = kw.chars().collect();
    let end = i + k.len();
    (end <= s.len() && s[i..end] == k[..] && !s.get(end).is_some_and(|c| is_word(*c)))
        .then_some(end)
}

/// `SELECT <column>::text FROM <table>` — both checked as plain
/// identifiers first, since they come from a config file.
pub fn applied_ids_sql(cfg: &PatchesConfig) -> Result<String, String> {
    let (schema, table) = plain_ident_path(&cfg.table)?;
    let (col_schema, column) = plain_ident_path(&cfg.column)?;
    if col_schema.is_some() {
        return Err(format!("{:?} is not a plain column name", cfg.column));
    }
    let table = match schema {
        Some(s) => format!("{s}.{table}"),
        None => table,
    };
    Ok(format!(
        "/* pgman:pending_patches */ SELECT {column}::text AS id FROM {table}"
    ))
}

/// The comparison of patch files with the ids the database recorded.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    /// Highest id the database has recorded.
    pub applied_max: Option<i64>,
    /// Versioned files whose id the database has not recorded, by id.
    pub pending: Vec<PatchFile>,
    /// Ids the database recorded that no file carries, not even by
    /// filename.
    pub recorded_without_file: Vec<i64>,
    /// Ids the database recorded whose file (matched by filename number)
    /// has no live version-row `INSERT` — commented out, say. The row got
    /// there some other way; running that file again would not add it.
    pub recorded_by_unversioned_file: Vec<(i64, String)>,
    /// Files with no version-row `INSERT`, numbered after the first file
    /// that has one — a recent patch missing its version row is a bug.
    pub unversioned_recent: Vec<PatchFile>,
    /// How many files have no version row at all (older ones included).
    pub unversioned_total: usize,
    /// Files whose filename number and recorded id disagree.
    pub mismatched: Vec<PatchFile>,
    /// Ids carried by more than one file.
    pub duplicate_ids: Vec<(i64, Vec<String>)>,
}

pub fn reconcile(mut files: Vec<PatchFile>, applied: &[i64]) -> Report {
    use std::collections::{BTreeMap, BTreeSet};
    files.sort_by(|a, b| {
        (a.version_id.or(a.filename_number), &a.file)
            .cmp(&(b.version_id.or(b.filename_number), &b.file))
    });
    let applied_set: BTreeSet<i64> = applied.iter().copied().collect();
    let first_versioned = files
        .iter()
        .filter(|f| f.version_id.is_some())
        .filter_map(|f| f.filename_number)
        .min();
    let mut by_id: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    let mut r = Report {
        applied_max: applied_set.iter().next_back().copied(),
        ..Report::default()
    };
    for f in &files {
        match f.version_id {
            Some(id) => {
                by_id.entry(id).or_default().push(f.file.clone());
                if !applied_set.contains(&id) {
                    r.pending.push(f.clone());
                }
                if f.filename_number.is_some_and(|n| n != id) {
                    r.mismatched.push(f.clone());
                }
            }
            None => {
                r.unversioned_total += 1;
                if let (Some(first), Some(n)) = (first_versioned, f.filename_number) {
                    if n > first {
                        r.unversioned_recent.push(f.clone());
                    }
                }
            }
        }
    }
    for id in applied_set.iter().filter(|id| !by_id.contains_key(id)) {
        let by_name = files
            .iter()
            .find(|f| f.version_id.is_none() && f.filename_number == Some(*id));
        match by_name {
            Some(f) => r.recorded_by_unversioned_file.push((*id, f.file.clone())),
            None => r.recorded_without_file.push(*id),
        }
    }
    r.duplicate_ids = by_id.into_iter().filter(|(_, v)| v.len() > 1).collect();
    r
}

/// Most version rows `pending_patches` and the environment probes will read. A version table is
/// one row per patch; past this something is not what it seems.
pub const MAX_APPLIED_IDS: usize = 100_000;

/// Largest patch file read. The version row sits at the top; a data
/// load can be huge, and only its head is needed.
pub const MAX_PATCH_BYTES: u64 = 1024 * 1024;

/// The recorded ids from the version-table query. A list cut at the row
/// cap is refused rather than reported on: a missing id would read as a
/// pending patch, which is the one wrong answer this tool must not give.
/// So is a list with an id that isn't an integer.
pub fn applied_ids(
    typed: &crate::conn::TypedRows,
    truncated: bool,
    cfg: &PatchesConfig,
) -> Result<Vec<i64>, String> {
    let (ids, other) = applied_ids_and_others(typed, truncated, cfg)?;
    if !other.is_empty() {
        return Err(format!(
            "{}.{} holds a non-integer id",
            cfg.table, cfg.column
        ));
    }
    Ok(ids)
}

/// [`applied_ids`], with the ids that aren't integers (`1.2`, `R7` in a
/// text column) kept aside instead of failing the list: for a probe,
/// which only shows a version. A list cut at the row cap is still
/// refused. A NULL id records nothing. Pure.
pub fn applied_ids_and_others(
    typed: &crate::conn::TypedRows,
    truncated: bool,
    cfg: &PatchesConfig,
) -> Result<(Vec<i64>, Vec<String>), String> {
    if truncated {
        return Err(format!(
            "{} holds more than {MAX_APPLIED_IDS} rows; refusing to report on a partial list",
            cfg.table
        ));
    }
    let mut ids = Vec::new();
    let mut other = Vec::new();
    for row in &typed.rows {
        if let Some(text) = row.first().and_then(|c| c.as_deref()) {
            match text.parse::<i64>() {
                Ok(id) => ids.push(id),
                Err(_) => other.push(text.to_string()),
            }
        }
    }
    Ok((ids, other))
}

/// Every `*.sql` file in `dir` (not recursive), parsed for its id.
pub fn read_patch_files(
    dir: &std::path::Path,
    cfg: &PatchesConfig,
) -> Result<Vec<PatchFile>, String> {
    use std::io::Read;
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if !file.to_ascii_lowercase().ends_with(".sql") || !path.is_file() {
            continue;
        }
        let mut text = String::new();
        std::fs::File::open(&path)
            .and_then(|f| f.take(MAX_PATCH_BYTES).read_to_string(&mut text))
            .map_err(|e| format!("cannot read {file}: {e}"))?;
        out.push(patch_file(&file, &text, cfg));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {

    #[test]
    fn an_insert_inside_a_dollar_quoted_body_is_not_the_version_row() {
        let sql = "SELECT $$INSERT INTO v (id) VALUES (999)$$;\n\
                   CREATE FUNCTION f() RETURNS void AS $fn$ INSERT INTO v (id) VALUES (998) $fn$ LANGUAGE sql;\n\
                   INSERT INTO v (id) VALUES (300);";
        assert_eq!(version_id_in(sql, "v", "id"), Some(300));
        assert_eq!(
            version_id_in("SELECT $$INSERT INTO v (id) VALUES (1)$$;", "v", "id"),
            None
        );
    }

    use super::*;

    fn cfg() -> PatchesConfig {
        PatchesConfig {
            dir: "schema_updates".into(),
            table: "t_schema_version".into(),
            column: "schema_version_id".into(),
        }
    }

    const ONE_LINE: &str = "-- APP-101\n\
        INSERT INTO t_schema_version (schema_version_id, schema_version_description, schema_version_added_by)\n  \
        VALUES (249, 'APP-101 Add user_external_id; user_password_hash nullable', 'Tom');\n\
        ALTER TABLE t_user ADD COLUMN x int;";

    #[test]
    fn reads_the_id_from_the_version_row_insert() {
        assert_eq!(
            version_id_in(ONE_LINE, "t_schema_version", "schema_version_id"),
            Some(249)
        );
    }

    #[test]
    fn handles_multi_line_comments_strings_and_column_order() {
        let sql = "/* header (with parens, and commas) */\n\
            insert into T_SCHEMA_VERSION \n\t(schema_version_description, schema_version_id, schema_version_added_on)\n\
            VALUES\n\t('it''s (a) -- test, really', 200, now());";
        assert_eq!(
            version_id_in(sql, "t_schema_version", "schema_version_id"),
            Some(200)
        );
        let qualified = "INSERT INTO public.t_schema_version (schema_version_id) VALUES (7);";
        assert_eq!(
            version_id_in(qualified, "t_schema_version", "schema_version_id"),
            Some(7)
        );
        assert_eq!(
            version_id_in(qualified, "public.t_schema_version", "schema_version_id"),
            Some(7)
        );
    }

    #[test]
    fn no_version_row_means_none_not_a_guess() {
        let cases = [
            "ALTER TABLE t_user ADD COLUMN x int;",
            "-- INSERT INTO t_schema_version (schema_version_id) VALUES (5);",
            "INSERT INTO t_other (schema_version_id) VALUES (5);",
            "INSERT INTO t_schema_version (schema_version_description) VALUES ('5');",
            "INSERT INTO t_schema_version (schema_version_id) VALUES ('5x');",
            "INSERT INTO t_schema_version (schema_version_id) SELECT 5;",
            "INSERT INTO t_schema_version_history (schema_version_id) VALUES (5);",
            "SELECT 'INSERT INTO t_schema_version (schema_version_id) VALUES (5)'; ",
        ];
        for sql in cases {
            assert_eq!(
                version_id_in(sql, "t_schema_version", "schema_version_id"),
                None,
                "{sql}"
            );
        }
    }

    #[test]
    fn filename_numbers() {
        assert_eq!(filename_number("248-APP-102-x.sql"), Some(248));
        assert_eq!(filename_number("161a_categoryONETData.sql"), Some(161));
        assert_eq!(filename_number("readme.sql"), None);
    }

    fn f(file: &str, version_id: Option<i64>) -> PatchFile {
        PatchFile {
            file: file.into(),
            filename_number: filename_number(file),
            version_id,
        }
    }

    #[test]
    fn reconcile_finds_pending_gaps_mismatches_and_duplicates() {
        let files = vec![
            f("100-old.sql", None),
            f("200-first.sql", Some(200)),
            f("201-second.sql", Some(201)),
            f("202-forgot-version-row.sql", None),
            f("203-wrong-id.sql", Some(230)),
            f("204-dup.sql", Some(201)),
            f("249-pending.sql", Some(249)),
            f("250-pending.sql", Some(250)),
        ];
        let r = reconcile(files, &[200, 201, 199, 202]);
        assert_eq!(r.applied_max, Some(202));
        let pending: Vec<&str> = r.pending.iter().map(|p| p.file.as_str()).collect();
        assert_eq!(
            pending,
            ["203-wrong-id.sql", "249-pending.sql", "250-pending.sql"]
        );
        assert_eq!(r.recorded_without_file, [199]);
        assert_eq!(
            r.recorded_by_unversioned_file,
            [(202, "202-forgot-version-row.sql".to_string())]
        );
        assert_eq!(r.unversioned_total, 2);
        let recent: Vec<&str> = r
            .unversioned_recent
            .iter()
            .map(|p| p.file.as_str())
            .collect();
        assert_eq!(recent, ["202-forgot-version-row.sql"]);
        let mism: Vec<&str> = r.mismatched.iter().map(|p| p.file.as_str()).collect();
        assert_eq!(mism, ["204-dup.sql", "203-wrong-id.sql"]);
        assert_eq!(
            r.duplicate_ids,
            vec![(
                201,
                vec!["201-second.sql".to_string(), "204-dup.sql".to_string()]
            )]
        );
    }

    #[test]
    fn nothing_pending_when_the_database_has_everything() {
        let r = reconcile(vec![f("1-a.sql", Some(1)), f("2-b.sql", Some(2))], &[1, 2]);
        assert!(r.pending.is_empty());
        assert!(r.recorded_without_file.is_empty());
    }

    #[test]
    fn ids_that_arent_integers_fail_applying_but_are_kept_for_a_probe() {
        let typed = crate::conn::TypedRows {
            columns: Vec::new(),
            rows: vec![vec![Some("7".into())], vec![None], vec![Some("1.2".into())]],
            affected: None,
        };
        assert_eq!(
            applied_ids_and_others(&typed, false, &cfg()),
            Ok((vec![7], vec!["1.2".to_string()]))
        );
        assert!(applied_ids(&typed, false, &cfg())
            .unwrap_err()
            .contains("non-integer"));
        assert!(
            applied_ids_and_others(&typed, true, &cfg()).is_err(),
            "a cut list is refused either way"
        );
    }

    #[test]
    fn applied_ids_sql_only_takes_plain_identifiers() {
        assert_eq!(
            applied_ids_sql(&cfg()).unwrap(),
            "/* pgman:pending_patches */ SELECT schema_version_id::text AS id FROM t_schema_version"
        );
        for (table, column) in [
            ("t; DROP TABLE x", "c"),
            ("t", "c FROM pg_shadow --"),
            ("t", "s.c"),
        ] {
            let c = PatchesConfig {
                dir: "d".into(),
                table: table.into(),
                column: column.into(),
            };
            assert!(applied_ids_sql(&c).is_err(), "{table} {column}");
        }
    }

    #[test]
    fn patch_file_combines_both_numbers() {
        let p = patch_file("249-x.sql", ONE_LINE, &cfg());
        assert_eq!((p.filename_number, p.version_id), (Some(249), Some(249)));
    }
}
