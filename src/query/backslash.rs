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
    let body = trimmed.strip_prefix('\\')?;
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
        _ => BackslashCmd::Unknown(raw),
    })
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
    fn parse_handles_multiline_buffer_with_leading_backslash() {
        // First non-blank token is `\d` — psql would route it.
        let buf = "\\d users\nthis would be ignored";
        assert_eq!(
            parse_backslash_command(buf),
            Some(BackslashCmd::Describe(Some("users".into())))
        );
    }
}
