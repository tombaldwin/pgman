//! psql-style backslash command parsing.
//!
//! When the editor buffer starts with `\` and the operator hits
//! Run, route it as a meta-command instead of sending to the
//! server. Familiarity bridge for psql migrants — `\d users` /
//! `\dt` / `\?` / `\q` all do the obvious thing.
//!
//! Pure: this module just *parses*. The App-side dispatch in
//! `request_run` decides what to do with the result.

/// One recognised backslash command. Unknown commands return
/// `Some(BackslashCmd::Unknown(raw))` so the dispatcher can
/// surface a "unknown command \xyz" hint instead of silently
/// sending it to the server (which would error anyway).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackslashCmd {
    /// `\d` (no arg) — schema browser, default view.
    /// `\d <name>` — schema browser opened with filter set to
    /// `<name>` so the operator sees that object immediately.
    Describe(Option<String>),
    /// `\dt` — list tables. Routes to schema browser with all
    /// schemas expanded (operator wants the tables view).
    ListTables,
    /// `\dn` — list schemas. Routes to schema browser too.
    ListSchemas,
    /// `\df` / `\df <pattern>` — list functions as a result grid.
    /// The pattern is psql's: `*` and `?` wildcards, an optional
    /// `schema.` prefix, folded to lower case unless double-quoted.
    ListFunctions(Option<String>),
    /// `\?` — open the help cheatsheet.
    Help,
    /// `\q` — quit.
    Quit,
    /// `\timing` / `\timing on` / `\timing off` — toggle the
    /// per-query duration display. The optional bool comes from
    /// an explicit `on` / `off` arg; `None` means toggle.
    Timing(Option<bool>),
    /// `\report` / `\report <path>` — dump pgman's current
    /// advisor + tap insights to a shareable file (Markdown
    /// by default, HTML when the path ends in `.html`/`.htm`).
    /// `None` arg means "pick a default path under the cache
    /// dir."
    Report(Option<String>),
    /// `\fixture` / `\fixture <path>` — capture the current
    /// result grid as a DBUnit FlatXmlDataSet (the reverse of
    /// the apply script). `None` arg means "pick a default path
    /// under the cache dir." Needs a single-table result so the
    /// element name is known.
    Fixture(Option<String>),
    /// `\l` — list databases. Renders `App.databases` (name +
    /// on-disk size, already filled by the bootstrap query at
    /// connect time) as a result grid. No new query.
    ListDatabases,
    /// `\x` / `\x on` / `\x off` — toggle expanded (row-detail)
    /// output. `None` means toggle from the current state, same
    /// shape as `Timing`.
    Expanded(Option<bool>),
    /// `\c` — open the connection picker. `\c <name>` — connect
    /// to the picker entry matching `<name>`. `None` arg means
    /// no name was given (open the picker).
    Connect(Option<String>),
    /// `\i <path>` — read a SQL file into the editor buffer
    /// (replacing it) without running it. `None` when no path
    /// was given.
    Include(Option<String>),
    /// A recognised command whose *argument* is wrong. Carries the
    /// message to show verbatim — the parser knows what was expected,
    /// the dispatcher doesn't. Distinct from `Unknown`, which is a
    /// command nobody recognises.
    Invalid(String),
    /// Anything else starting with `\`. The dispatcher uses the
    /// inner string to compose a useful error.
    Unknown(String),
}

/// Parse the editor buffer as a backslash command. Returns
/// `None` when the buffer doesn't start with `\` (after leading
/// whitespace). Multi-line buffers where the first non-blank
/// character is `\` are still routed through here — psql's
/// behaviour.
pub fn parse_backslash_command(buf: &str) -> Option<BackslashCmd> {
    let trimmed = buf.trim();
    let body = trimmed.strip_prefix('\\')?.trim_start();
    let mut parts = body.split_whitespace();
    let cmd = parts.next()?;
    let arg1 = parts.next();
    // Anything past arg1 we currently ignore — psql is laxer
    // here too.

    let raw = format!(
        "\\{cmd}{}",
        arg1.map(|a| format!(" {a}")).unwrap_or_default()
    );
    Some(match cmd {
        "d" => BackslashCmd::Describe(arg1.map(str::to_string)),
        "dt" => BackslashCmd::ListTables,
        "dn" => BackslashCmd::ListSchemas,
        "df" => BackslashCmd::ListFunctions(arg1.map(str::to_string)),
        "?" | "h" => BackslashCmd::Help,
        "q" | "quit" => BackslashCmd::Quit,
        "timing" => BackslashCmd::Timing(match arg1.map(str::to_ascii_lowercase).as_deref() {
            Some("on") => Some(true),
            Some("off") => Some(false),
            _ => None,
        }),
        "report" => BackslashCmd::Report(arg1.map(str::to_string)),
        "fixture" => BackslashCmd::Fixture(arg1.map(str::to_string)),
        "l" => BackslashCmd::ListDatabases,
        "x" => BackslashCmd::Expanded(match arg1.map(str::to_ascii_lowercase).as_deref() {
            Some("on") => Some(true),
            Some("off") => Some(false),
            _ => None,
        }),
        // `c` reads its argument with `quoted_arg`, not `arg1`:
        // discovered data-source names contain spaces
        // (`dataSource (application)`), and the whitespace split
        // would have handed back `dataSource` — a name no pick has.
        "c" => match quoted_arg(body, cmd) {
            Ok(arg) => BackslashCmd::Connect(arg),
            Err(e) => BackslashCmd::Invalid(e),
        },
        "i" => BackslashCmd::Include(arg1.map(str::to_string)),
        _ => BackslashCmd::Unknown(raw),
    })
}

/// The catalog query behind `\df`. With no pattern it lists what the
/// operator wrote — everything outside `pg_catalog` and
/// `information_schema` — the way psql does; a pattern searches every
/// schema, system ones included, which is how psql answers `\df now`.
///
/// The pattern becomes anchored POSIX regexes ([`psql_pattern_regex`])
/// compared with `~`, inside `E''` literals so the backslashes the
/// escaping adds mean the same whatever `standard_conforming_strings`
/// is set to.
pub fn list_functions_sql(pattern: Option<&str>) -> String {
    let mut sql = String::from(
        "SELECT n.nspname AS schema, p.proname AS name, \
pg_catalog.pg_get_function_result(p.oid) AS result, \
pg_catalog.pg_get_function_arguments(p.oid) AS arguments, \
CASE p.prokind WHEN 'a' THEN 'agg' WHEN 'w' THEN 'window' WHEN 'p' THEN 'proc' ELSE 'func' END AS type \
FROM pg_catalog.pg_proc p \
JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace",
    );
    match pattern.map(str::trim).filter(|p| !p.is_empty()) {
        None => sql.push_str(" WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')"),
        Some(p) => {
            let (schema, name) = split_pattern(p);
            sql.push_str(&format!(
                " WHERE p.proname ~ {}",
                e_literal(&psql_pattern_regex(&name))
            ));
            if let Some(schema) = schema {
                sql.push_str(&format!(
                    " AND n.nspname ~ {}",
                    e_literal(&psql_pattern_regex(&schema))
                ));
            }
        }
    }
    sql.push_str(" ORDER BY 1, 2, 4");
    sql
}

/// Split a psql pattern into its optional schema part and its name
/// part at the last `.` outside double quotes, folding unquoted text
/// to lower case and dropping the quotes (`""` inside quotes is one
/// `"`). `"MySchema".f*` gives `(Some("MySchema"), "f*")`.
pub fn split_pattern(pattern: &str) -> (Option<String>, String) {
    let mut parts = vec![String::new()];
    let mut quoted = false;
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        let part = parts.last_mut().expect("parts is never empty");
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                part.push('"');
            }
            '"' => quoted = !quoted,
            '.' if !quoted => parts.push(String::new()),
            c if quoted => part.push(c),
            c => part.extend(c.to_lowercase()),
        }
    }
    let name = parts.pop().unwrap_or_default();
    // `db.schema.name`: psql takes the part before the name as the schema.
    let schema = parts.pop();
    (schema, name)
}

/// One psql pattern part as an anchored POSIX regex: `*` is any run,
/// `?` one character, and everything else is literal.
pub fn psql_pattern_regex(part: &str) -> String {
    let mut out = String::from("^(");
    for c in part.chars() {
        match c {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            c if "\\.^$|()[]{}+".contains(c) => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push_str(")$");
    out
}

/// `s` as an `E'…'` string literal: backslashes and quotes doubled.
fn e_literal(s: &str) -> String {
    format!("E'{}'", s.replace('\\', "\\\\").replace('\'', "''"))
}

/// Whether `name` matches one psql pattern part, the way
/// [`psql_pattern_regex`] would on the server. Used where there is no
/// server to ask (`--demo`). Folding is the caller's: see
/// [`split_pattern`].
pub fn glob_matches(part: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some(('*', rest)) => (0..=n.len()).any(|i| go(rest, &n[i..])),
            Some(('?', rest)) => !n.is_empty() && go(rest, &n[1..]),
            Some((c, rest)) => n.first() == Some(c) && go(rest, &n[1..]),
        }
    }
    let p: Vec<char> = part.chars().collect();
    let n: Vec<char> = name.chars().collect();
    go(&p, &n)
}

/// Message for a `\c` whose argument is more than one bare word.
pub const CONNECT_ONE_NAME: &str = "\\c takes one name — quote it if it has spaces: \\c \"my db\"";

/// The argument following `cmd` in `body`, honouring a
/// double-quoted span so a name containing spaces survives:
/// `c "dataSource (application)"` yields `dataSource (application)`.
///
/// Unquoted, it must be exactly one word. Taking the first and
/// dropping the rest is what `split_whitespace().next()` did, and it
/// meant `\c my db` silently connected to a *different* database
/// called `my` — the one class of typo where guessing is worse than
/// asking. Extra text after a closing quote (`\c "a" b`) is refused
/// for the same reason.
///
/// An unterminated quote still takes the rest of the line (the intent
/// is unambiguous, and refusing would only make the operator retype
/// it); an empty argument, or an empty quoted string, is `Ok(None)`.
fn quoted_arg(body: &str, cmd: &str) -> Result<Option<String>, String> {
    let Some(rest) = body.get(cmd.len()..) else {
        return Ok(None);
    };
    let rest = rest.trim();
    if let Some(after_open) = rest.strip_prefix('"') {
        let (inside, trailing) = match after_open.split_once('"') {
            Some((inside, trailing)) => (inside, trailing.trim()),
            None => (after_open, ""),
        };
        if !trailing.is_empty() {
            return Err(CONNECT_ONE_NAME.to_string());
        }
        return Ok((!inside.is_empty()).then(|| inside.to_string()));
    }
    let mut words = rest.split_whitespace();
    let Some(first) = words.next() else {
        return Ok(None);
    };
    if words.next().is_some() {
        return Err(CONNECT_ONE_NAME.to_string());
    }
    Ok(Some(first.to_string()))
}

/// How a connect-by-name argument resolved against the discovered
/// data-source names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameMatch {
    /// Exactly one candidate — its index into the input slice.
    One(usize),
    /// Several candidates share the prefix; the caller lists them
    /// rather than picking one. Indices, in input order.
    Ambiguous(Vec<usize>),
    /// Nothing matched. The caller falls back to treating the name
    /// as a database to swap onto the current DSN.
    None,
}

/// Resolve `name` against `candidates` (data-source names, in
/// picker order). Case-insensitive throughout.
///
/// An exact match always wins — `dev` picks `dev` even when `dev2`
/// also exists. Failing that, a unique case-insensitive PREFIX
/// match resolves, which is what makes the long discovered names
/// (`dataSource (application)`) reachable without quoting. Several
/// prefix matches are [`NameMatch::Ambiguous`]: choosing one of them
/// would be choosing which database the operator connects to.
pub fn match_pick_name(name: &str, candidates: &[&str]) -> NameMatch {
    let wanted = name.trim();
    if wanted.is_empty() {
        return NameMatch::None;
    }
    if let Some(i) = candidates
        .iter()
        .position(|c| c.eq_ignore_ascii_case(wanted))
    {
        return NameMatch::One(i);
    }
    let lower = wanted.to_lowercase();
    let hits: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.to_lowercase().starts_with(&lower))
        .map(|(i, _)| i)
        .collect();
    match hits.len() {
        0 => NameMatch::None,
        1 => NameMatch::One(hits[0]),
        _ => NameMatch::Ambiguous(hits),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_returns_none_for_non_backslash_buffers() {
        assert_eq!(parse_backslash_command(""), None);
        assert_eq!(parse_backslash_command("select 1"), None);
        assert_eq!(parse_backslash_command("  select 1\n"), None);
    }

    #[test]
    fn parse_describe_with_and_without_argument() {
        assert_eq!(
            parse_backslash_command("\\d"),
            Some(BackslashCmd::Describe(None))
        );
        assert_eq!(
            parse_backslash_command("\\d users"),
            Some(BackslashCmd::Describe(Some("users".into())))
        );
        // Leading whitespace tolerated.
        assert_eq!(
            parse_backslash_command("   \\d  orders\n"),
            Some(BackslashCmd::Describe(Some("orders".into())))
        );
    }

    #[test]
    fn parse_list_variants_are_distinct_from_describe() {
        assert_eq!(
            parse_backslash_command("\\dt"),
            Some(BackslashCmd::ListTables)
        );
        assert_eq!(
            parse_backslash_command("\\dn"),
            Some(BackslashCmd::ListSchemas)
        );
    }

    #[test]
    fn parse_help_and_quit_aliases() {
        assert_eq!(parse_backslash_command("\\?"), Some(BackslashCmd::Help));
        assert_eq!(parse_backslash_command("\\h"), Some(BackslashCmd::Help));
        assert_eq!(parse_backslash_command("\\q"), Some(BackslashCmd::Quit));
        assert_eq!(parse_backslash_command("\\quit"), Some(BackslashCmd::Quit));
    }

    #[test]
    fn parse_timing_toggle_and_explicit_on_off() {
        assert_eq!(
            parse_backslash_command("\\timing"),
            Some(BackslashCmd::Timing(None))
        );
        assert_eq!(
            parse_backslash_command("\\timing on"),
            Some(BackslashCmd::Timing(Some(true)))
        );
        assert_eq!(
            parse_backslash_command("\\timing OFF"),
            Some(BackslashCmd::Timing(Some(false)))
        );
        // Garbage arg falls back to toggle (psql's behaviour).
        assert_eq!(
            parse_backslash_command("\\timing foo"),
            Some(BackslashCmd::Timing(None))
        );
    }

    #[test]
    fn parse_unknown_command_carries_raw_for_error_message() {
        match parse_backslash_command("\\xyz") {
            Some(BackslashCmd::Unknown(raw)) => assert_eq!(raw, "\\xyz"),
            other => panic!("expected Unknown; got {other:?}"),
        }
    }

    #[test]
    fn parse_report_with_and_without_path() {
        assert_eq!(
            parse_backslash_command("\\report"),
            Some(BackslashCmd::Report(None))
        );
        assert_eq!(
            parse_backslash_command("\\report /tmp/r.md"),
            Some(BackslashCmd::Report(Some("/tmp/r.md".into())))
        );
        assert_eq!(
            parse_backslash_command("\\report report.html"),
            Some(BackslashCmd::Report(Some("report.html".into())))
        );
    }

    #[test]
    fn parse_fixture_with_and_without_path() {
        assert_eq!(
            parse_backslash_command("\\fixture"),
            Some(BackslashCmd::Fixture(None))
        );
        assert_eq!(
            parse_backslash_command("\\fixture users.xml"),
            Some(BackslashCmd::Fixture(Some("users.xml".into())))
        );
    }

    #[test]
    fn parse_list_databases() {
        assert_eq!(
            parse_backslash_command("\\l"),
            Some(BackslashCmd::ListDatabases)
        );
        // Extra token ignored — psql's `\l` also accepts (and ignores)
        // a pattern here.
        assert_eq!(
            parse_backslash_command("\\l ignored"),
            Some(BackslashCmd::ListDatabases)
        );
    }

    #[test]
    fn parse_expanded_toggle_and_explicit_on_off() {
        assert_eq!(
            parse_backslash_command("\\x"),
            Some(BackslashCmd::Expanded(None))
        );
        assert_eq!(
            parse_backslash_command("\\x on"),
            Some(BackslashCmd::Expanded(Some(true)))
        );
        assert_eq!(
            parse_backslash_command("\\x OFF"),
            Some(BackslashCmd::Expanded(Some(false)))
        );
        // Garbage arg falls back to toggle, same as `\timing`.
        assert_eq!(
            parse_backslash_command("\\x foo"),
            Some(BackslashCmd::Expanded(None))
        );
    }

    #[test]
    fn parse_connect_with_and_without_name() {
        assert_eq!(
            parse_backslash_command("\\c"),
            Some(BackslashCmd::Connect(None))
        );
        assert_eq!(
            parse_backslash_command("\\c prod"),
            Some(BackslashCmd::Connect(Some("prod".into())))
        );
    }

    #[test]
    fn parse_connect_refuses_a_trailing_unquoted_word() {
        // `\c my db` used to connect to a *different* database called
        // `my` — the first word, with the rest dropped on the floor.
        // Guessing there is worse than asking.
        assert_eq!(
            parse_backslash_command("\\c my db"),
            Some(BackslashCmd::Invalid(CONNECT_ONE_NAME.into()))
        );
        // Same for text after a closing quote.
        assert_eq!(
            parse_backslash_command("\\c \"my db\" extra"),
            Some(BackslashCmd::Invalid(CONNECT_ONE_NAME.into()))
        );
        // The message says how to fix it.
        assert!(CONNECT_ONE_NAME.contains("quote it if it has spaces"));
    }

    #[test]
    fn parse_connect_accepts_a_double_quoted_name_with_spaces() {
        // Spring / IntelliJ discovery names look like this, and the
        // whitespace split used to hand back just `dataSource`.
        assert_eq!(
            parse_backslash_command("\\c \"dataSource (application)\""),
            Some(BackslashCmd::Connect(Some(
                "dataSource (application)".into()
            )))
        );
        // Unterminated quote: take the rest of the line.
        assert_eq!(
            parse_backslash_command("\\c \"dataSource (application)"),
            Some(BackslashCmd::Connect(Some(
                "dataSource (application)".into()
            )))
        );
        // Empty quotes are no name at all — open the picker.
        assert_eq!(
            parse_backslash_command("\\c \"\""),
            Some(BackslashCmd::Connect(None))
        );
    }

    #[test]
    fn match_pick_name_prefers_exact_then_unique_prefix() {
        let names = ["dev", "dev2", "prod (application)"];
        // Exact wins even though `dev` is also a prefix of `dev2`.
        assert_eq!(match_pick_name("dev", &names), NameMatch::One(0));
        assert_eq!(match_pick_name("DEV2", &names), NameMatch::One(1));
        // Unique prefix resolves — this is what makes the long
        // discovered names typeable.
        assert_eq!(match_pick_name("prod", &names), NameMatch::One(2));
        assert_eq!(match_pick_name("PROD (app", &names), NameMatch::One(2));
    }

    #[test]
    fn match_pick_name_reports_ambiguity_instead_of_guessing() {
        let names = ["dataSource (application)", "dataSource (test)", "reports"];
        assert_eq!(
            match_pick_name("dataSource", &names),
            NameMatch::Ambiguous(vec![0, 1]),
            "two data sources share the prefix — the caller must list them"
        );
        assert_eq!(match_pick_name("dataSource (t", &names), NameMatch::One(1));
        assert_eq!(match_pick_name("nope", &names), NameMatch::None);
        assert_eq!(match_pick_name("", &names), NameMatch::None);
        assert_eq!(match_pick_name("x", &[]), NameMatch::None);
    }

    #[test]
    fn parse_include_with_and_without_path() {
        assert_eq!(
            parse_backslash_command("\\i"),
            Some(BackslashCmd::Include(None))
        );
        assert_eq!(
            parse_backslash_command("\\i /tmp/q.sql"),
            Some(BackslashCmd::Include(Some("/tmp/q.sql".into())))
        );
        // Extra tokens past the path ignored, same as every other
        // arg1-only command here.
        assert_eq!(
            parse_backslash_command("\\i /tmp/q.sql extra"),
            Some(BackslashCmd::Include(Some("/tmp/q.sql".into())))
        );
    }

    #[test]
    fn parse_handles_multiline_buffer_with_leading_backslash() {
        // First non-blank token is `\d` — psql would route it.
        let buf = "\\d users\nthis would be ignored";
        assert_eq!(
            parse_backslash_command(buf),
            Some(BackslashCmd::Describe(Some("users".into())))
        );
    }

    #[test]
    fn parse_df_with_and_without_a_pattern() {
        assert_eq!(
            parse_backslash_command("\\df"),
            Some(BackslashCmd::ListFunctions(None))
        );
        assert_eq!(
            parse_backslash_command("\\df  app.calc*"),
            Some(BackslashCmd::ListFunctions(Some("app.calc*".into())))
        );
    }

    #[test]
    fn split_pattern_folds_unquoted_text_and_takes_the_last_dot() {
        assert_eq!(split_pattern("Calc*"), (None, "calc*".into()));
        assert_eq!(
            split_pattern("App.Calc"),
            (Some("app".into()), "calc".into())
        );
        assert_eq!(
            split_pattern("\"MySchema\".\"Odd.Name\""),
            (Some("MySchema".into()), "Odd.Name".into())
        );
        assert_eq!(split_pattern("\"a\"\"b\""), (None, "a\"b".into()));
        assert_eq!(split_pattern("db.s.f"), (Some("s".into()), "f".into()));
    }

    #[test]
    fn psql_pattern_regex_anchors_and_escapes() {
        assert_eq!(psql_pattern_regex("calc"), "^(calc)$");
        assert_eq!(psql_pattern_regex("calc*"), "^(calc.*)$");
        assert_eq!(psql_pattern_regex("f?o"), "^(f.o)$");
        // A literal `.` / `$` / `(` must not be a regex operator.
        assert_eq!(psql_pattern_regex("a.b$(c)"), "^(a\\.b\\$\\(c\\))$");
    }

    #[test]
    fn list_functions_sql_without_a_pattern_hides_system_schemas() {
        let sql = list_functions_sql(None);
        assert!(sql.contains("NOT IN ('pg_catalog', 'information_schema')"));
        assert!(!sql.contains(" ~ "));
        assert!(list_functions_sql(Some("  ")).contains("NOT IN"));
    }

    #[test]
    fn list_functions_sql_with_a_pattern_matches_name_and_schema() {
        let sql = list_functions_sql(Some("app.calc*"));
        assert!(sql.contains("p.proname ~ E'^(calc.*)$'"), "{sql}");
        assert!(sql.contains("n.nspname ~ E'^(app)$'"), "{sql}");
        assert!(!sql.contains("NOT IN"));
    }

    #[test]
    fn list_functions_sql_quotes_cannot_escape_the_literal() {
        // A `'` in the pattern is doubled, and the escaping backslash
        // is doubled inside E'' — the statement stays one SELECT.
        let sql = list_functions_sql(Some("x'; DROP TABLE t; --.f"));
        assert!(sql.contains("E'^(x''; drop table t; --)$'"), "{sql}");
        let sql = list_functions_sql(Some("a.b\\c"));
        assert!(sql.contains("E'^(b\\\\\\\\c)$'"), "{sql}");
        let statements = crate::safety::split_verified(&sql).expect("one statement");
        assert_eq!(statements.len(), 1);
    }

    #[test]
    fn list_functions_sql_is_allowed_by_the_safety_guard() {
        let sql = list_functions_sql(Some("public.f*"));
        let decision =
            crate::safety::evaluate(&crate::safety::SafetyConfig::default(), "prod", &sql);
        assert_eq!(decision.guard, crate::safety::Guard::Allow);
    }

    #[test]
    fn glob_matches_follows_psql_wildcards() {
        assert!(glob_matches("calc*", "calc_total"));
        assert!(glob_matches("calc*", "calc"));
        assert!(!glob_matches("calc", "calc_total"));
        assert!(glob_matches("c?lc", "calc"));
        assert!(!glob_matches("c?lc", "clc"));
        assert!(glob_matches("*total", "calc_total"));
    }
}
