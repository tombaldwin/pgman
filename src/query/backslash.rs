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
        "c" => BackslashCmd::Connect(quoted_arg(body, cmd)),
        "i" => BackslashCmd::Include(arg1.map(str::to_string)),
        _ => BackslashCmd::Unknown(raw),
    })
}

/// The argument following `cmd` in `body`, honouring a
/// double-quoted span so a name containing spaces survives:
/// `c "dataSource (application)"` yields `dataSource (application)`.
///
/// Unquoted, it is the first whitespace-delimited word, exactly as
/// `split_whitespace` would have produced — so nothing that parsed
/// before parses differently now. An unterminated quote takes the
/// rest of the line (the intent is unambiguous, and refusing would
/// only make the operator retype it); an empty argument, or an
/// empty quoted string, is `None`.
fn quoted_arg(body: &str, cmd: &str) -> Option<String> {
    let rest = body.get(cmd.len()..)?.trim();
    if let Some(after_open) = rest.strip_prefix('"') {
        let inside = match after_open.split_once('"') {
            Some((inside, _)) => inside,
            None => after_open,
        };
        return (!inside.is_empty()).then(|| inside.to_string());
    }
    rest.split_whitespace().next().map(str::to_string)
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
        // Extra tokens past the name ignored.
        assert_eq!(
            parse_backslash_command("\\c prod extra"),
            Some(BackslashCmd::Connect(Some("prod".into())))
        );
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
}
