//! Convention guards: plain source-scanning tests that walk `src/` and
//! check the house rules from `CLAUDE.md` — no hardcoded `Color::*`
//! outside `theme.rs`, no `println!`/`eprintln!`/`print!` outside the
//! CLI-only entry points, no hardcoded config/cache/project paths
//! outside `util.rs`/`project.rs`, and that a `KeyCode::Char(c) if
//! <modifier>`-guarded arm always precedes the unguarded arm for the
//! same `c` (Rust tries match arms top to bottom, so the wrong order
//! makes the chord unreachable and the compiler stays silent about it).
//!
//! `syn` is not a dependency here, so unlike ebman's AST-based
//! `key_arm_order.rs` these are line-level scans. The one trap that
//! defeats a naive line scan — cutting a `//` "comment" out of a string
//! literal that happens to contain `//` (a URL, say) — is handled by
//! `strip_line_comment`, which tracks whether the scanner is inside a
//! `"…"` or `'…'` literal before treating `//` as a comment start.

use std::fs;
use std::path::Path;

// ---------------------------------------------------------------------
// Shared scanning primitives
// ---------------------------------------------------------------------

/// Split `line` into its code portion (any `//` line comment stripped,
/// without cutting inside a string or char literal) and the net
/// `{` minus `}` found in that code portion — also literal-aware, so a
/// pattern like `KeyCode::Char('{')` doesn't perturb brace counting.
///
/// Ported from the same fix in ebman's `app/tests/scan.rs`: a `'` is
/// only treated as starting/ending a char literal when it looks like
/// one (`'x'` or an escape `'\n'`), not a lifetime like `'a`.
fn scan_line(line: &str) -> (&str, i32) {
    let b = line.as_bytes();
    let mut i = 0;
    let mut in_str = false;
    let mut in_char = false;
    let mut depth = 0i32;
    let mut code_end = line.len();
    while i < b.len() {
        match b[i] {
            b'\\' if in_str || in_char => {
                i += 2;
                continue;
            }
            b'"' if !in_char => in_str = !in_str,
            b'\'' if !in_str => {
                let looks_like_char = b.get(i + 1) == Some(&b'\\')
                    || (b.get(i + 2) == Some(&b'\'') && b.get(i + 1).is_some());
                if in_char || looks_like_char {
                    in_char = !in_char;
                }
            }
            b'{' if !in_str && !in_char => depth += 1,
            b'}' if !in_str && !in_char => depth -= 1,
            b'/' if !in_str && !in_char && b.get(i + 1) == Some(&b'/') => {
                code_end = i;
                break;
            }
            _ => {}
        }
        i += 1;
    }
    (&line[..code_end], depth)
}

/// Just the comment-stripped code portion of `line`.
fn strip_line_comment(line: &str) -> &str {
    scan_line(line).0
}

/// Every `.rs` file under `src/`, as `(path, contents)` with forward
/// slashes so string comparisons against `"src/util.rs"` etc. work the
/// same on any OS. Carries its own sanity floor — a walk that finds
/// almost nothing is a broken walk, and a guard over zero files passes
/// vacuously.
fn source_files() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|x| x == "rs") {
                if let Ok(contents) = fs::read_to_string(&path) {
                    let display = path.to_string_lossy().replace('\\', "/");
                    out.push((display, contents));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(Path::new("src"), &mut out);
    assert!(
        out.len() > 30,
        "source walk under src/ found only {} files — the walk is broken, \
         and a guard over nothing passes vacuously",
        out.len()
    );
    out
}

// ---------------------------------------------------------------------
// Guard 1 — no hardcoded `Color::*` outside theme.rs
// ---------------------------------------------------------------------

/// `Color::` occurrences in `text`'s code (comments stripped), as
/// `path:line` strings.
fn colour_hits(path: &str, text: &str) -> Vec<String> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| strip_line_comment(line).contains("Color::"))
        .map(|(n, _)| format!("{path}:{}", n + 1))
        .collect()
}

/// CLAUDE.md: "No hardcoded colours. Use `theme::Theme` fields.
/// Hardcoded `Color::*` is a regression." `theme::Theme` is the one
/// place that is allowed — and required — to name concrete `Color`
/// values; everything else must read through a `Theme` field.
///
/// Checked the current tree by hand first (`grep -rn "Color::" src/`):
/// every hit is in `src/theme.rs` itself (its `Default`/`light`/`dark`
/// palettes plus `contrast_text` and its tests). `src/splash.rs` was
/// the other candidate CLAUDE.md flags as a possible exception, but it
/// has no `Color::` at all — it already draws through the theme — so
/// no allowlist beyond `theme.rs` was needed.
#[test]
fn no_hardcoded_colours_outside_theme() {
    const ALLOWED: &[&str] = &["src/theme.rs"];
    let mut offenders = Vec::new();
    for (path, text) in source_files() {
        if ALLOWED.contains(&path.as_str()) {
            continue;
        }
        offenders.extend(colour_hits(&path, &text));
    }
    assert!(
        offenders.is_empty(),
        "hardcoded Color::* outside src/theme.rs — read the colour from a \
         theme::Theme field instead:\n{}",
        offenders.join("\n")
    );
}

// ---------------------------------------------------------------------
// Guard 2 — no println!/eprintln!/print! in the running TUI
// ---------------------------------------------------------------------

fn print_macro_hits(path: &str, text: &str) -> Vec<String> {
    const NEEDLES: [&str; 3] = ["println!", "eprintln!", "print!("];
    let mut hits = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let code = strip_line_comment(line);
        for needle in NEEDLES {
            if code.contains(needle) {
                hits.push(format!("{path}:{}", n + 1));
            }
        }
    }
    hits
}

/// CLAUDE.md: "No `println!` / `eprintln!` in the running TUI — the
/// alternate screen swallows them. Use `tracing::*` ... (CLI-only code
/// before the TUI exists may print.)"
///
/// Verified the allowlist by grepping the whole tree first
/// (`grep -rn "println!\|eprintln!\|print!(" src/`): every hit is in
/// `src/main.rs` (argv handling, `--batch`/`--sql` error paths, all
/// before/instead of the alternate screen), `src/batch.rs` (the
/// `--batch` CLI path's own stdout/stderr — its whole job is printing
/// query output) and `src/upgrade.rs` (the `--upgrade` CLI path, which
/// execs `cargo install`/`brew` and reports progress to a real
/// terminal, never inside the TUI). No other file matches.
#[test]
fn no_println_in_the_tui() {
    const ALLOWED: &[&str] = &["src/main.rs", "src/batch.rs", "src/upgrade.rs"];
    let mut offenders = Vec::new();
    for (path, text) in source_files() {
        if ALLOWED.contains(&path.as_str()) {
            continue;
        }
        offenders.extend(print_macro_hits(&path, &text));
    }
    assert!(
        offenders.is_empty(),
        "println!/eprintln!/print! outside the CLI-only entry points — the \
         alternate screen swallows stdout/stderr in the running TUI, use \
         tracing::* instead:\n{}",
        offenders.join("\n")
    );
}

// ---------------------------------------------------------------------
// Guard 3 — no hardcoded config/cache/project paths outside util.rs /
// project.rs
// ---------------------------------------------------------------------

const PATH_NEEDLES: [&str; 5] = ["/Users/", "/home/", "~/.config", "~/.cache", ".pgman/"];

/// Hardcoded-path hits in `text`, skipping comment text and dropping
/// everything from an inline `#[cfg(test)] mod <name> { … }` onward.
///
/// That's "onward" rather than "until the module's closing brace"
/// because every `#[cfg(test)] mod tests { … }` in this codebase is the
/// last item in its file (checked by hand: `main.rs`, `ui.rs`,
/// `update_check.rs`, `project.rs`, `safety.rs` all end exactly there),
/// so treating it as "skip to EOF" is exact here and sidesteps having
/// to brace-match through raw strings like `r#"{"a":1}"#` (whose
/// embedded quotes and braces a naive counter would misread). A file
/// that put more production code after its test module would need the
/// smarter version; this one doesn't have one.
fn hardcoded_path_hits(path: &str, text: &str) -> Vec<String> {
    let mut hits = Vec::new();
    let mut pending_cfg_test = false;
    for (n, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if pending_cfg_test {
            pending_cfg_test = false;
            if trimmed.starts_with("mod ") && trimmed.contains('{') {
                break;
            }
        }
        if trimmed.starts_with("#[cfg(test)]") {
            pending_cfg_test = true;
            continue;
        }
        let code = strip_line_comment(line);
        for needle in PATH_NEEDLES {
            if code.contains(needle) {
                hits.push(format!("{path}:{} — contains {needle:?}", n + 1));
            }
        }
    }
    hits
}

/// CLAUDE.md: "No hardcoded paths. Use `util::config_dir()` /
/// `util::cache_dir()` / `util::config_file(...)`." `util.rs` holds
/// those helpers and `project.rs` holds the `.pgman/pgman.toml`
/// project-file convention, so both are exempt; nowhere else should
/// need to know the literal shape of these paths.
///
/// Checked the current tree by hand first. Three real violations, all
/// fixed in this change (not allowlisted — CLAUDE.md's own stance is
/// that widening an allowlist to quiet a guard is the wrong move):
///   - `src/ui.rs` (error-detail action bar) hand-typed
///     `~/.cache/pgman/pgman.log`; now built from
///     `util::cache_dir().join("pgman.log")`, so it stays correct if
///     the cache location ever moves.
///   - `src/ui.rs` (empty connection-picker hint) and `src/main.rs`
///     (`resolve_batch_dsn`'s no-DSN error) both spelled out
///     `.pgman/pgman.toml` as descriptive text; reworded to name the
///     `.pgman` directory without the trailing-slash path fragment,
///     which was the only thing the guard actually objects to — the
///     directory *name* isn't a hardcoded path, a duplicated
///     `<dir>/<file>` join outside `project.rs` would be.
///
/// The remaining hits are all in `#[cfg(test)]` fixtures (e.g.
/// `update_check.rs`'s `InstallChannel::detect` tests exercise real
/// install paths like `/Users/tom/.cargo/bin/pgman`) or doc comments,
/// both excluded above.
#[test]
fn no_hardcoded_paths() {
    const ALLOWED: &[&str] = &["src/util.rs", "src/project.rs"];
    let mut offenders = Vec::new();
    for (path, text) in source_files() {
        if ALLOWED.contains(&path.as_str()) {
            continue;
        }
        offenders.extend(hardcoded_path_hits(&path, &text));
    }
    assert!(
        offenders.is_empty(),
        "hardcoded config/cache/project paths outside util.rs / project.rs \
         — route through util::config_dir() / util::cache_dir() / \
         util::config_file(...), or project.rs's .pgman/pgman.toml \
         convention:\n{}",
        offenders.join("\n")
    );
}

// ---------------------------------------------------------------------
// Guard 4 — a guarded KeyCode::Char('c') arm must precede the
// unguarded arm for the same 'c'
// ---------------------------------------------------------------------

/// Parse a Rust char literal starting at `s` (which begins right after
/// `Char(`, possibly with leading whitespace): `'x'` or a simple escape
/// (`'\''`, `'\n'`, `'\\'`, …). Returns the char and nothing else — the
/// caller only needs to know it saw one.
fn char_literal_at(s: &str) -> Option<char> {
    let s = s.trim_start();
    let mut chars = s.chars();
    if chars.next()? != '\'' {
        return None;
    }
    let c = chars.next()?;
    if c == '\\' {
        let esc = chars.next()?;
        let val = match esc {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            '0' => '\0',
            other => other, // '\\' -> '\\', '\'' -> '\'', '"' -> '"', etc.
        };
        if chars.next()? == '\'' {
            Some(val)
        } else {
            None
        }
    } else if chars.next()? == '\'' {
        Some(c)
    } else {
        None
    }
}

/// Every char literal reached through a `KeyCode::Char(..)` construct
/// on this (already comment-stripped) line — handles `|`-alternated
/// arms like `KeyCode::Char('y') | KeyCode::Char('Y')` by finding every
/// `Char(` occurrence, not just the first.
fn chars_in_char_arms(code: &str) -> Vec<char> {
    let mut out = Vec::new();
    let mut search_from = 0;
    while let Some(rel) = code[search_from..].find("Char(") {
        let after = search_from + rel + "Char(".len();
        if let Some(c) = char_literal_at(&code[after..]) {
            out.push(c);
        }
        search_from = after;
    }
    out
}

/// Is `word` present in `text` as a standalone token (not part of a
/// longer identifier)?
fn contains_word(text: &str, word: &str) -> bool {
    let tb = text.as_bytes();
    let wb = word.as_bytes();
    if wb.is_empty() || wb.len() > tb.len() {
        return false;
    }
    let is_ident = |b: u8| (b as char).is_alphanumeric() || b == b'_';
    for i in 0..=(tb.len() - wb.len()) {
        if &tb[i..i + wb.len()] == wb {
            let before_ok = i == 0 || !is_ident(tb[i - 1]);
            let after = i + wb.len();
            let after_ok = after == tb.len() || !is_ident(tb[after]);
            if before_ok && after_ok {
                return true;
            }
        }
    }
    false
}

/// Does this arm's pattern + guard (the part of `code` before `=>`,
/// which is where a match guard always lives) test a key modifier?
/// The keymap's own convention is a local `let ctrl = key.modifiers
/// .contains(KeyModifiers::CONTROL);` reused across arms, so "guard
/// mentions ctrl" is the shape to look for, not just the raw
/// `KeyModifiers::CONTROL` spelling — plus that spelling and SHIFT /
/// ALT / SUPER / a bare `.modifiers` check, for anywhere that tests it
/// inline instead of through the local.
///
/// A non-modifier guard (`if self.conn_pick.picks.len() >= 2`) is
/// deliberately NOT this rule's business: it doesn't compete with an
/// unguarded arm for the same char the way a modifier chord does, and
/// treating it as one is exactly the false-positive shape a blunt
/// detector invents.
fn is_modifier_guarded(code: &str) -> bool {
    let head = match code.find("=>") {
        Some(idx) => &code[..idx],
        None => code,
    };
    let Some(if_pos) = head.find(" if ").or_else(|| {
        // also match "if " at the very start of the (trimmed) head
        let t = head.trim_start();
        if t.starts_with("if ") {
            Some(head.len() - t.len())
        } else {
            None
        }
    }) else {
        return false;
    };
    let guard_text = &head[if_pos..];
    ["ctrl", "CONTROL", "SHIFT", "ALT", "SUPER", "modifiers"]
        .iter()
        .any(|w| contains_word(guard_text, w))
}

#[derive(Debug, Clone)]
struct CharArm {
    ch: char,
    guarded: bool,
    line: usize, // 1-based
}

#[derive(Debug, Clone)]
struct Shadowed {
    ch: char,
    unguarded_line: usize,
    guarded_line: usize,
}

/// Line-range (1-based, inclusive) of every `match key.code { … }`
/// block in `text`, found by brace-counting from a line containing the
/// literal text `match key.code {` (which also catches `let kind =
/// match key.code {` etc. — the search is for the substring, not an
/// exact-line match).
fn match_key_code_blocks(lines: &[&str]) -> Vec<(usize, usize)> {
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].contains("match key.code {") {
            let mut depth = 0i32;
            let mut j = i;
            loop {
                let (_, delta) = scan_line(lines[j]);
                depth += delta;
                if depth <= 0 || j + 1 >= lines.len() {
                    break;
                }
                j += 1;
            }
            blocks.push((i + 1, j + 1));
            i = j + 1;
        } else {
            i += 1;
        }
    }
    blocks
}

/// Rule-4 violations in one file's source, the chars seen in both
/// guarded and unguarded form within the same block (the shape this
/// rule actually polices), and the total count of concrete-char
/// guarded arms found (a `KeyCode::Char(c) if ctrl` catch-all with a
/// *bound* `c` doesn't count — there's no literal to compare).
///
/// pgman's keymap turns out not to have same-block collisions: its
/// catch-all arms bind `c` rather than repeating a literal, and its
/// letter-specific ctrl chords (Ctrl-R history search, Ctrl-D DBUnit,
/// …) don't share a block with a plain, unguarded arm for the same
/// letter. So `both_forms` is expected to come back empty here — see
/// the guarded-arm-count floor in the caller for the non-vacuous check
/// instead.
fn shadowed_key_arms(text: &str) -> (Vec<Shadowed>, Vec<char>, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let mut violations = Vec::new();
    let mut both_forms = Vec::new();
    let mut guarded_concrete_arms = 0usize;
    for (start, end) in match_key_code_blocks(&lines) {
        let mut arms: Vec<CharArm> = Vec::new();
        for line_no in start..=end {
            let code = strip_line_comment(lines[line_no - 1]);
            if !code.contains("Char(") {
                continue;
            }
            let guarded = is_modifier_guarded(code);
            for ch in chars_in_char_arms(code) {
                arms.push(CharArm {
                    ch,
                    guarded,
                    line: line_no,
                });
            }
        }
        guarded_concrete_arms += arms.iter().filter(|a| a.guarded).count();
        for a in arms.iter().filter(|a| a.guarded) {
            if let Some(earlier) = arms
                .iter()
                .find(|b| b.ch == a.ch && !b.guarded && b.line < a.line)
            {
                violations.push(Shadowed {
                    ch: a.ch,
                    unguarded_line: earlier.line,
                    guarded_line: a.line,
                });
            }
            if arms.iter().any(|b| b.ch == a.ch && !b.guarded) && !both_forms.contains(&a.ch) {
                both_forms.push(a.ch);
            }
        }
    }
    (violations, both_forms, guarded_concrete_arms)
}

/// Match-arm order: "Guarded `KeyCode::Char(..) if Ctrl` arms come
/// before the unguarded arm for the same char." Rust tries arms top to
/// bottom and both are reachable *patterns* — only the guard makes one
/// a subset of the other — so the compiler stays quiet when the order
/// is wrong and the chord just silently falls through to the unguarded
/// arm's behaviour instead.
///
/// `syn` isn't a dependency here (checked `Cargo.toml`), so this scans
/// text rather than parsing an AST the way ebman's `key_arm_order.rs`
/// does. Scoped to the two files that dispatch on `key.code`:
/// `src/app/keys.rs` and `src/app/editor.rs`.
#[test]
fn ctrl_guarded_arms_precede_unguarded() {
    let mut offenders = Vec::new();
    let mut both_forms_total = Vec::new();
    let mut blocks_checked = 0usize;
    let mut guarded_concrete_total = 0usize;
    for path in ["src/app/keys.rs", "src/app/editor.rs"] {
        let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let lines: Vec<&str> = text.lines().collect();
        blocks_checked += match_key_code_blocks(&lines).len();
        let (violations, both_forms, guarded_concrete) = shadowed_key_arms(&text);
        guarded_concrete_total += guarded_concrete;
        for v in violations {
            offenders.push(format!(
                "{path}:{} — unguarded '{}' at :{} shadows the guarded arm",
                v.guarded_line, v.ch, v.unguarded_line
            ));
        }
        for c in both_forms {
            if !both_forms_total.contains(&c) {
                both_forms_total.push(c);
            }
        }
    }
    assert!(
        blocks_checked >= 20,
        "expected at least 20 `match key.code {{ }}` blocks across keys.rs \
         + editor.rs; found {blocks_checked} — the block finder is \
         probably broken"
    );
    // Non-vacuous check: prove the detector actually recognises modifier
    // guards on concrete char literals in the real keymap (Ctrl-R, Ctrl-D,
    // the tap monitor's Shift-B, …), rather than requiring a same-block
    // collision — this codebase's catch-all arms bind a variable instead
    // of repeating a literal, so `both_forms_total` (see
    // `shadowed_key_arms`) is legitimately empty here.
    assert!(
        guarded_concrete_total >= 15,
        "expected at least 15 modifier-guarded concrete-char arms across \
         keys.rs + editor.rs for this rule to have something to police; \
         found {guarded_concrete_total}"
    );
    assert!(
        offenders.is_empty(),
        "a guarded KeyCode::Char arm is shadowed by an earlier unguarded \
         arm for the same char — move the guarded arm above it:\n{}",
        offenders.join("\n")
    );
}

// ---------------------------------------------------------------------
// Unit tests for the trickier detectors, on small inline samples —
// CLAUDE.md: a guard is production code for the invariant it holds, so
// it needs a test of its own, not just the real-tree sweep that uses
// it.
// ---------------------------------------------------------------------

#[cfg(test)]
mod detector_tests {
    use super::*;

    #[test]
    fn a_url_inside_a_string_literal_is_not_a_comment() {
        let line = r#"    Some(format!("https://{}", host))"#;
        assert_eq!(
            strip_line_comment(line),
            line,
            "a `//` inside a string literal is not a comment"
        );
    }

    #[test]
    fn a_real_trailing_comment_is_stripped() {
        assert_eq!(strip_line_comment("let x = 1; // note"), "let x = 1; ");
    }

    #[test]
    fn a_brace_char_literal_does_not_perturb_brace_counting() {
        // This is exactly editor.rs's autoclose arm body: `'{'` and
        // `'}'` as char literals, inside a block that itself needs one
        // real matching pair to net to zero.
        let (code, delta) = scan_line("if matches!(c, '(' | '[' | '{') {");
        assert_eq!(code, "if matches!(c, '(' | '[' | '{') {");
        assert_eq!(delta, 1, "the literal '{{' must not count as a real brace");
        let (code, delta) = scan_line("} else if matches!(c, ')' | ']' | '}')");
        assert_eq!(code, "} else if matches!(c, ')' | ']' | '}')");
        assert_eq!(delta, -1, "the literal '}}' must not count as a real brace");
    }

    #[test]
    fn extracts_an_escaped_quote_char_literal() {
        assert_eq!(char_literal_at("'\\'') => {}"), Some('\''));
    }

    #[test]
    fn extracts_both_sides_of_an_or_alternation() {
        let chars = chars_in_char_arms("KeyCode::Char('y') | KeyCode::Char('Y') => self.yank(),");
        assert_eq!(chars, vec!['y', 'Y']);
    }

    #[test]
    fn a_bound_variable_is_not_a_char_literal() {
        assert!(chars_in_char_arms("KeyCode::Char(c) if ctrl => {}").is_empty());
    }

    #[test]
    fn ctrl_local_variable_guard_is_recognised() {
        assert!(is_modifier_guarded(
            "KeyCode::Char('r') if ctrl => self.start_history_search(),"
        ));
    }

    #[test]
    fn a_non_modifier_guard_is_not_treated_as_one() {
        assert!(!is_modifier_guarded(
            "KeyCode::Char('p') if self.conn_pick.picks.len() >= 2 => {"
        ));
    }

    #[test]
    fn an_unguarded_arm_shadowing_a_guarded_one_is_found() {
        let src = "
        fn f(key: KeyEvent) {
            match key.code {
                KeyCode::Char('d') => self.detail(),
                KeyCode::Char('d') if ctrl => self.dlq(),
                _ => {}
            }
        }
        ";
        let (v, _, _) = shadowed_key_arms(src);
        assert_eq!(v.len(), 1, "the shadowed ctrl-d arm must be found: {v:?}");
        assert_eq!(v[0].ch, 'd');
    }

    #[test]
    fn the_correct_order_is_clean() {
        let src = "
        fn f(key: KeyEvent) {
            match key.code {
                KeyCode::Char('d') if ctrl => self.dlq(),
                KeyCode::Char('d') => self.detail(),
                _ => {}
            }
        }
        ";
        assert!(shadowed_key_arms(src).0.is_empty());
    }

    #[test]
    fn arms_in_different_match_blocks_do_not_shadow_each_other() {
        let src = "
        fn f(key: KeyEvent) {
            match key.code {
                KeyCode::Char('k') => self.up(),
                _ => {}
            }
            match key.code {
                KeyCode::Char('k') if ctrl => self.top(),
                _ => {}
            }
        }
        ";
        assert!(
            shadowed_key_arms(src).0.is_empty(),
            "arms in separate match blocks are independent"
        );
    }

    #[test]
    fn cfg_test_module_is_skipped_to_eof() {
        let src = "fn f() {}\n\n#[cfg(test)]\nmod tests {\n    let p = \"/Users/tom/x\";\n}\n";
        assert!(hardcoded_path_hits("x.rs", src).is_empty());
    }

    #[test]
    fn a_hardcoded_path_outside_a_test_module_is_found() {
        let src = "let p = \"/Users/tom/x\";\n";
        assert_eq!(hardcoded_path_hits("x.rs", src).len(), 1);
    }

    #[test]
    fn a_doc_comment_mentioning_a_path_is_not_flagged() {
        let src = "/// lives at ~/.config/pgman/safety.toml\nfn f() {}\n";
        assert!(hardcoded_path_hits("x.rs", src).is_empty());
    }
}
