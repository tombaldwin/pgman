//! Convention guards: plain source-scanning tests that walk `src/` and
//! check the house rules from `CLAUDE.md` — no hardcoded `Color::*`
//! outside `theme.rs`, no `println!`/`eprintln!`/`print!` outside
//! CLI-only code, no hardcoded config/cache/project paths outside
//! `util.rs`/`project.rs`, that a `KeyCode::Char(c) if <modifier>`-
//! guarded arm always precedes the unguarded (or catch-all) arm for the
//! same `c` — and a fifth guard, unrelated to `src/`, over what `cargo
//! package` would actually ship to crates.io.
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

/// String/char-literal aware net bracket delta of `code` (`(`,`[`,`{` =
/// +1, `)`,`]`,`}` = -1) — used to find where a match arm's body (a
/// braced block or a plain expression) ends on the lines following its
/// `=>`, without needing to distinguish which bracket kind opened it.
/// `code` is expected to already have any `//` comment stripped.
fn bracket_delta(code: &str) -> i32 {
    let b = code.as_bytes();
    let mut i = 0;
    let mut in_str = false;
    let mut in_char = false;
    let mut depth = 0i32;
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
            b'(' | b'[' | b'{' if !in_str && !in_char => depth += 1,
            b')' | b']' | b'}' if !in_str && !in_char => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    depth
}

/// 0-based index of the line containing the closing `}` matching the
/// first `{` found at/after `lines[start]`, tracked by *true* depth —
/// char-by-char, not a net-per-line delta — so a line that opens and
/// closes its own brace pair (a one-line function, `mod tests { … }`
/// all on one line) isn't mistaken for "never opened". String/char-
/// literal aware, and comment-aware for `//` (not block comments —
/// callers that need those stripped do it first).
///
/// Returns `None` if a top-level `;` is reached before any `{` — a
/// signature with no body (a trait method declaration) rather than a
/// block to skip. Scanning past it looking for some unrelated later
/// `{` would silently skip arbitrary amounts of real code.
fn brace_end_from(lines: &[&str], start: usize) -> Option<usize> {
    let mut in_str = false;
    let mut in_char = false;
    let mut depth = 0i32;
    let mut opened = false;
    for (li, line) in lines.iter().enumerate().skip(start) {
        let b = line.as_bytes();
        let mut i = 0;
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
                b'/' if !in_str && !in_char && b.get(i + 1) == Some(&b'/') => break,
                b';' if !in_str && !in_char && !opened => return None,
                b'{' if !in_str && !in_char => {
                    depth += 1;
                    opened = true;
                }
                b'}' if !in_str && !in_char => {
                    depth -= 1;
                    if opened && depth <= 0 {
                        return Some(li);
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
    None
}

/// Strip `/* … */` block comments from `text`, replacing their content
/// (newlines aside) with spaces so line numbers are unaffected. Doesn't
/// handle nested block comments (Rust allows them; this codebase
/// doesn't use them) or a `/*`-looking sequence inside a string
/// literal — good enough for a text-based guard, and checked against
/// the current tree by hand.
fn strip_block_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_block_comment = false;
    while let Some(c) = chars.next() {
        if in_block_comment {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block_comment = false;
                out.push(' ');
                out.push(' ');
            } else if c == '\n' {
                out.push('\n');
            } else {
                out.push(' ');
            }
            continue;
        }
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            in_block_comment = true;
            out.push(' ');
            out.push(' ');
            continue;
        }
        out.push(c);
    }
    out
}

/// Per-line mask over `lines`: `true` where the line is *not* inside a
/// `#[cfg(test)] mod <name> { … }` body. Each such module is skipped by
/// its own brace extent (via `brace_end_from`), not "to EOF" — so
/// production code that happens to follow a test module in the same
/// file (unlike today's tree, where it's always last) is still
/// covered. Used by both the hardcoded-path guard and the packaged-
/// files content grep, which exempt the same category of content for
/// the same reason.
fn non_test_module_mask(lines: &[&str]) -> Vec<bool> {
    let mut mask = vec![true; lines.len()];
    let mut i = 0usize;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if trimmed.starts_with("#[cfg(test)]") {
            // Only attributes/whitespace are expected between
            // `#[cfg(test)]` and the `mod` line it guards; give up
            // after a few lines rather than searching indefinitely.
            let mut j = i + 1;
            while j < lines.len() && j - i <= 5 && !lines[j].trim_start().starts_with("mod ") {
                j += 1;
            }
            if j < lines.len()
                && lines[j].trim_start().starts_with("mod ")
                && lines[j].contains('{')
            {
                if let Some(end) = brace_end_from(lines, j) {
                    for m in mask.iter_mut().take(end + 1).skip(i) {
                        *m = false;
                    }
                    i = end + 1;
                    continue;
                }
            }
            // `#[cfg(test)]` on something other than an inline `mod {
            // }` (e.g. a single `#[cfg(test)] fn helper() {}`, or `mod
            // tests;` pointing at a separate file) — nothing to skip
            // here; fall through and scan this line normally.
        }
        i += 1;
    }
    mask
}

/// `text` with every `#[cfg(test)] mod { … }` body's lines blanked out
/// — used wherever a scan should ignore known test-fixture content
/// (e.g. synthetic `/Users/tester` paths in install-channel-detection
/// tests) while still catching the same needle in production code.
fn strip_test_module_content(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mask = non_test_module_mask(&lines);
    lines
        .iter()
        .zip(mask.iter())
        .map(|(line, keep)| if *keep { *line } else { "" })
        .collect::<Vec<_>>()
        .join("\n")
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

/// Files where printing is CLI-only for the *whole* file: `batch.rs` is
/// the `--batch` CLI path's own stdout/stderr (its whole job is
/// printing query output), `upgrade.rs` is the `--upgrade` CLI path,
/// which execs `cargo install`/`brew` and reports progress to a real
/// terminal — neither ever runs inside the TUI event loop.
const PRINT_ALLOWED_WHOLE_FILES: &[&str] = &["src/batch.rs", "src/upgrade.rs"];

/// The identifier right after `fn ` in `s` (which starts immediately
/// after the `fn ` keyword), stopping at `(`, `<` (generics), or
/// whitespace. `None` if what follows isn't a plausible identifier —
/// i.e. this wasn't really a function header.
fn fn_name_at(s: &str) -> Option<String> {
    let end = s.find(|c: char| c == '(' || c == '<' || c.is_whitespace())?;
    let name = &s[..end];
    let mut chars = name.chars();
    let first = chars.next()?;
    if !(first.is_alphabetic() || first == '_') {
        return None;
    }
    if !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some(name.to_string())
}

/// Is `name` (a function defined in `path`) allowed to print, by the
/// same CLI-only reasoning as `PRINT_ALLOWED_WHOLE_FILES`?
///
/// - `main` and `run_batch`, but only in `src/main.rs`: `main` calls
///   `run_batch(&cli).await` for `--batch` and immediately
///   `std::process::exit`s on the result (the `if cli.batch { … }` arm
///   near the top of `main`, checked by hand) — never reaching the TUI
///   setup a few lines below. `run_batch` just isn't split into its own
///   file the way `--upgrade` is.
/// - any function named `*_cli`, anywhere — a documented naming
///   convention for a CLI-only helper not worth its own file.
fn is_print_allowed_function(path: &str, name: &str) -> bool {
    (path == "src/main.rs" && (name == "main" || name == "run_batch")) || name.ends_with("_cli")
}

/// `println!`/`eprintln!`/`print!` hits in `text` (declared at `path`),
/// as `path:line` strings — skipping `PRINT_ALLOWED_WHOLE_FILES`
/// entirely, and any line inside a function `is_print_allowed_function`
/// approves.
fn print_macro_hits(path: &str, text: &str) -> Vec<String> {
    const NEEDLES: [&str; 3] = ["println!", "eprintln!", "print!("];
    if PRINT_ALLOWED_WHOLE_FILES.contains(&path) {
        return Vec::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut exempt = vec![false; lines.len()];
    for i in 0..lines.len() {
        let code = strip_line_comment(lines[i]);
        if let Some(fn_pos) = code.find("fn ") {
            if let Some(name) = fn_name_at(&code[fn_pos + 3..]) {
                if is_print_allowed_function(path, &name) {
                    if let Some(end) = brace_end_from(&lines, i) {
                        for e in exempt.iter_mut().take(end + 1).skip(i) {
                            *e = true;
                        }
                    }
                }
            }
        }
    }
    let mut hits = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        if exempt[n] {
            continue;
        }
        let code = strip_line_comment(line);
        // `"eprintln!"` contains `"println!"` as a substring (it's
        // `e` + `println!`), so checking each needle independently and
        // pushing a hit per match double-reports every `eprintln!` line
        // as both. One hit per offending line is what callers actually
        // want.
        if NEEDLES.iter().any(|needle| code.contains(needle)) {
            hits.push(format!("{path}:{}", n + 1));
        }
    }
    hits
}

/// CLAUDE.md: "No `println!` / `eprintln!` in the running TUI ... (CLI-
/// only code before the TUI exists may print.)" Previously this
/// allowlisted `src/main.rs`, `src/batch.rs` and `src/upgrade.rs`
/// wholesale; `src/main.rs` is now narrowed to just `fn main` and
/// `fn run_batch` (see `is_print_allowed_function`) — any *other*
/// function later added to `main.rs` that prints is a real finding,
/// not something a whole-file allowlist would hide.
#[test]
fn no_println_in_the_tui() {
    let mut offenders = Vec::new();
    for (path, text) in source_files() {
        offenders.extend(print_macro_hits(&path, &text));
    }
    assert!(
        offenders.is_empty(),
        "println!/eprintln!/print! outside CLI-only code — the alternate \
         screen swallows stdout/stderr in the running TUI, use tracing::* \
         instead:\n{}",
        offenders.join("\n")
    );
}

// ---------------------------------------------------------------------
// Guard 3 — no hardcoded config/cache/project paths outside util.rs /
// project.rs
// ---------------------------------------------------------------------

const PATH_NEEDLES: [&str; 4] = ["/Users/", "/home/", "~/.config", "~/.cache"];

/// Hardcoded-path hits in `text`, skipping comment text (both `//` and
/// `/* … */`) and skipping each `#[cfg(test)] mod { … }` body by its
/// own brace extent (see `non_test_module_mask`) rather than bailing to
/// EOF at the first one.
fn hardcoded_path_hits(path: &str, text: &str) -> Vec<String> {
    let stripped = strip_block_comments(text);
    let lines: Vec<&str> = stripped.lines().collect();
    let mask = non_test_module_mask(&lines);
    let mut hits = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        if !mask[n] {
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
/// `src/app/tests.rs` and `src/ui/tests.rs` are exempt too — they're
/// test code (`#[cfg(test)] mod tests;` in `app.rs` / `ui.rs` points at
/// them), so a hardcoded path there is a fixture, not a regression —
/// but the attribute lives in the *parent* file, not in these files
/// themselves, so the `#[cfg(test)]`-module skip above never sees it.
/// Same category as an inline test module; just needs its own entry.
#[test]
fn no_hardcoded_paths() {
    const ALLOWED: &[&str] = &[
        "src/util.rs",
        "src/project.rs",
        "src/app/tests.rs",
        "src/ui/tests.rs",
    ];
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
// Guard 4 — a guarded KeyCode::Char('c') arm (or catch-all) must not
// shadow, or be shadowed by, an earlier/later arm for the same char
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
/// in `code` — handles `|`-alternated arms like `KeyCode::Char('y') |
/// KeyCode::Char('Y')` by finding every `Char(` occurrence, not just
/// the first. A `Char(<ident>)` or `Char(_)` occurrence (no literal)
/// contributes nothing here — see `finalize_arm_head`, which treats an
/// empty result as a catch-all rather than "no Char at all".
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

/// Does this arm's pattern + guard (all of `head` — the `=>` has
/// already been stripped by the caller) test a key modifier? The
/// keymap's own convention is a local `let ctrl = key.modifiers
/// .contains(KeyModifiers::CONTROL);` reused across arms, so "guard
/// mentions ctrl" is the shape to look for, not just the raw
/// `KeyModifiers::CONTROL` spelling — plus that spelling and SHIFT /
/// ALT / SUPER / a bare `.modifiers` check, for anywhere that tests it
/// inline instead of through the local.
///
/// A non-modifier guard (`if self.conn_pick.picks.len() >= 2`) is
/// deliberately NOT this rule's business.
fn is_modifier_guarded(head: &str) -> bool {
    // Defensive: callers pass an already `=>`-stripped head, but this
    // is also exercised directly by unit tests with a full arm line.
    let head = match head.find("=>") {
        Some(idx) => &head[..idx],
        None => head,
    };
    let Some(if_pos) = head.find(" if ").or_else(|| {
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
struct ParsedArm {
    /// `None` means a catch-all: `Char(<ident>)` or `Char(_)` — no
    /// literal to compare, but it matches *every* char, so it can still
    /// shadow a later concrete arm.
    ch: Option<char>,
    guarded: bool,
    line: usize, // 1-based, where this arm's pattern started
}

#[derive(Debug, Clone)]
struct Shadowed {
    ch: char,
    culprit_line: usize,
    guarded_line: usize,
    /// Shadowed by a `Char(..)` catch-all, rather than by an unguarded
    /// literal arm for the same char.
    catchall: bool,
}

/// True if `code` (comment-stripped) contains a `match <expr>.code {`
/// header — the expr can be any identifier chain (`key`, `tab_key`,
/// `self.pending.key`, …), found by locating each `match ` and checking
/// the text up to the following `{` ends with `.code` once trimmed.
fn is_match_code_header(code: &str) -> bool {
    let mut search_from = 0;
    while let Some(rel) = code[search_from..].find("match ") {
        let after = search_from + rel + "match ".len();
        if let Some(brace_rel) = code[after..].find('{') {
            let between = code[after..after + brace_rel].trim();
            if between.ends_with(".code") {
                return true;
            }
        }
        search_from = after;
    }
    false
}

/// Line-range (1-based, inclusive) of every `match <expr>.code { … }`
/// block in `lines`, found by true brace depth from each header line —
/// found by a plain forward scan (not skipping past a block once
/// found), so a `match … .code { … }` nested inside another one's arm
/// body is discovered as its own, separate block rather than being
/// silently skipped or merged into the outer one's arm list.
fn match_code_blocks(lines: &[&str]) -> Vec<(usize, usize)> {
    let mut blocks = Vec::new();
    for i in 0..lines.len() {
        let code = strip_line_comment(lines[i]);
        if is_match_code_header(code) {
            if let Some(end) = brace_end_from(lines, i) {
                blocks.push((i + 1, end + 1));
            }
        }
    }
    blocks
}

/// Extract chars/guard-status from one arm's assembled head text (the
/// pattern plus an optional guard — the caller has already cut off
/// `=>` and everything after it) and push the result(s) onto `arms`.
/// A head with no `Char(` at all contributes nothing; one whose every
/// `Char(` is a catch-all (`chars_in_char_arms` came back empty)
/// contributes a single `ch: None` "matches everything" entry.
fn finalize_arm_head(head: &str, line: usize, arms: &mut Vec<ParsedArm>) {
    if !head.contains("Char(") {
        return;
    }
    let guarded = is_modifier_guarded(head);
    let chars = chars_in_char_arms(head);
    if chars.is_empty() {
        arms.push(ParsedArm {
            ch: None,
            guarded,
            line,
        });
    } else {
        for ch in chars {
            arms.push(ParsedArm {
                ch: Some(ch),
                guarded,
                line,
            });
        }
    }
}

/// Parse every arm in the `match … .code { … }` block spanning 1-based
/// lines `(start, end)` (the header and closing-brace lines
/// themselves are not scanned as arm content).
///
/// This is a small state machine, not a per-line scan: it accumulates
/// "head" text (pattern + optional guard) across as many lines as
/// needed until it finds `=>` — so a guard on a continuation line
/// (`Char('d')` / newline / `if ctrl =>`) and a multi-line `|`
/// alternation both parse correctly — then switches to "body" tracking
/// via `bracket_delta` until the arm's body (braced block or plain
/// expression) closes, so text after `=>` (including a `Char(` mention
/// inside the body, or an entire nested `match … .code { … }`) is never
/// mistaken for the next arm's head.
fn parse_arms_in_block(lines: &[&str], start: usize, end: usize) -> Vec<ParsedArm> {
    let mut arms = Vec::new();
    let mut head = String::new();
    let mut head_start_line = 0usize;
    let mut in_body = false;
    let mut body_depth = 0i32;
    let mut body_seen_content = false;

    for line_no in (start + 1)..end {
        let code = strip_line_comment(lines[line_no - 1]);
        if in_body {
            if !code.trim().is_empty() {
                body_seen_content = true;
            }
            body_depth += bracket_delta(code);
            if body_seen_content && body_depth <= 0 {
                in_body = false;
            }
            continue;
        }
        if head.is_empty() {
            head_start_line = line_no;
        }
        if let Some(idx) = code.find("=>") {
            head.push_str(&code[..idx]);
            finalize_arm_head(&head, head_start_line, &mut arms);
            head.clear();
            let after = &code[idx + 2..];
            if after.trim().is_empty() {
                // Nothing after `=>` on this line — the body (almost
                // always a `{ … }` block, given rustfmt's conventions)
                // starts on a later line. Enter body mode at baseline
                // depth and let the following lines' bracket_delta
                // establish it.
                in_body = true;
                body_depth = 0;
                body_seen_content = false;
            } else {
                let delta = bracket_delta(after);
                if delta > 0 {
                    in_body = true;
                    body_depth = delta;
                    body_seen_content = true;
                }
                // delta <= 0 with non-empty content: a single-line body
                // (`self.up(),` or even `{ self.dlq(); }` fully closed
                // on one line) — already done, stay out of body mode.
            }
        } else {
            head.push_str(code);
            head.push(' ');
        }
    }
    arms
}

/// Rule-4 violations in one file's source, plus the total count of
/// concrete-char guarded arms found (a `KeyCode::Char(c) if ctrl`
/// catch-all with a *bound* `c` doesn't count toward this — there's no
/// literal to compare, though it's still tracked as a potential
/// shadower of a later concrete arm).
fn shadowed_key_arms(text: &str) -> (Vec<Shadowed>, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let mut violations = Vec::new();
    let mut guarded_concrete_arms = 0usize;
    for (start, end) in match_code_blocks(&lines) {
        let arms = parse_arms_in_block(&lines, start, end);
        guarded_concrete_arms += arms.iter().filter(|a| a.ch.is_some() && a.guarded).count();
        for (i, a) in arms.iter().enumerate() {
            let Some(ch) = a.ch else { continue };
            if !a.guarded {
                continue;
            }
            for b in &arms[..i] {
                match b.ch {
                    Some(bch) if bch == ch && !b.guarded => {
                        violations.push(Shadowed {
                            ch,
                            culprit_line: b.line,
                            guarded_line: a.line,
                            catchall: false,
                        });
                        break;
                    }
                    None => {
                        violations.push(Shadowed {
                            ch,
                            culprit_line: b.line,
                            guarded_line: a.line,
                            catchall: true,
                        });
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
    (violations, guarded_concrete_arms)
}

/// Match-arm order: "Guarded `KeyCode::Char(..) if Ctrl` arms come
/// before the unguarded arm for the same char" — and, just as fatally,
/// before any `Char(<ident>)` / `Char(_)` catch-all, which matches
/// *every* char and so shadows a guarded arm for any specific one just
/// as completely as a literal unguarded arm would. Rust tries arms top
/// to bottom and both are reachable *patterns* — only the guard makes
/// one a subset of the other — so the compiler stays quiet when the
/// order is wrong and the chord just silently falls through to
/// whichever arm actually wins.
///
/// `syn` isn't a dependency here (checked `Cargo.toml`), so this scans
/// text rather than parsing an AST the way ebman's `key_arm_order.rs`
/// does. Scans every file under `src/`, not just the two known
/// dispatchers — a `match … .code { }` added anywhere else is covered
/// automatically.
#[test]
fn ctrl_guarded_arms_precede_unguarded() {
    let mut offenders = Vec::new();
    let mut blocks_checked = 0usize;
    let mut guarded_concrete_total = 0usize;
    for (path, text) in source_files() {
        let lines: Vec<&str> = text.lines().collect();
        blocks_checked += match_code_blocks(&lines).len();
        let (violations, guarded_concrete) = shadowed_key_arms(&text);
        guarded_concrete_total += guarded_concrete;
        for v in violations {
            let cause = if v.catchall {
                format!("a Char(..) catch-all at :{}", v.culprit_line)
            } else {
                format!("unguarded '{}' at :{}", v.ch, v.culprit_line)
            };
            offenders.push(format!(
                "{path}:{} — {cause} shadows the guarded arm",
                v.guarded_line
            ));
        }
    }
    assert!(
        blocks_checked >= 20,
        "expected at least 20 `match <expr>.code {{ }}` blocks across \
         src/; found {blocks_checked} — the block finder is probably \
         broken"
    );
    // Non-vacuous check: prove the detector actually recognises modifier
    // guards on concrete char literals in the real keymap (Ctrl-R, Ctrl-D,
    // the tap monitor's Shift-B, …), rather than requiring a same-block
    // collision — this codebase's catch-all arms bind a variable instead
    // of repeating a literal, and are correctly positioned after every
    // guarded arm, so no *violations* are expected here.
    assert!(
        guarded_concrete_total >= 15,
        "expected at least 15 modifier-guarded concrete-char arms across \
         src/ for this rule to have something to police; found \
         {guarded_concrete_total}"
    );
    assert!(
        offenders.is_empty(),
        "a guarded KeyCode::Char arm is shadowed by an earlier unguarded \
         or catch-all arm for the same char — move the guarded arm above \
         it:\n{}",
        offenders.join("\n")
    );
}

// ---------------------------------------------------------------------
// Guard 5 — cargo package ships only Cargo.toml's include allow-list,
// and no packaged file's content leaks a machine-specific path
// ---------------------------------------------------------------------

/// Files `cargo package` adds itself during packaging, outside
/// Cargo.toml's `include` allow-list: a normalised copy of Cargo.toml
/// (workspace-inheritance resolved) and a small JSON blob recording the
/// git commit the package was built from. Neither is a working-tree
/// file, and neither carries any risk.
const CARGO_PACKAGE_GENERATED: &[&str] = &["Cargo.toml.orig", ".cargo_vcs_info.json"];

/// Test-only files exempt from the machine-path content grep below, for
/// the same reason `no_hardcoded_paths` exempts them: their content is
/// synthetic install-channel fixtures (`/Users/tester`,
/// `/home/linuxbrew/.linuxbrew/bin/pgman`), not real ones.
const CONTENT_GREP_EXEMPT_FILES: &[&str] = &["src/app/tests.rs", "src/ui/tests.rs"];

/// True if `path` (a `cargo package --list` entry) matches one of the
/// banned shapes: backup/reject/stale-snapshot files, OS cruft,
/// mutation-testing output, or the dev-only docs / test tree / CI
/// config that Cargo.toml's `include` allow-list exists to keep out.
fn is_banned_packaged_path(path: &str) -> bool {
    const BANNED_SUFFIXES: &[&str] = &[".bak", ".orig", ".rej", ".snap.new"];
    const BANNED_EXACT: &[&str] = &[".DS_Store", "PLAN.md", "BACKLOG.md", "CLAUDE.md"];
    const BANNED_PREFIXES: &[&str] = &["mutants.out", "docs/", "tests/", ".github/", ".candor/"];
    BANNED_SUFFIXES.iter().any(|s| path.ends_with(s))
        || BANNED_EXACT.contains(&path)
        || BANNED_PREFIXES.iter().any(|p| path.starts_with(p))
}

/// CLAUDE.md / Cargo.toml: the published crate ships source + the docs
/// cargo needs, nothing else. Nothing enforced that claim — a widened
/// `include`, a typo, or cargo's old exclude-list default creeping back
/// in would silently start shipping PLAN.md / BACKLOG.md / CLAUDE.md /
/// docs/ / tests/ / .github/ again with no test catching it. Also
/// checked here: no packaged file's contents leak this build machine's
/// home directory or hostname.
///
/// Skips (with a clear message, not a failure) if `cargo` isn't on
/// PATH — this guard shells out to `cargo package --list`, unlike every
/// other guard in this file, which only reads the filesystem.
#[test]
fn cargo_package_ships_only_the_allowlist() {
    let output = match std::process::Command::new("cargo")
        .args(["package", "--list", "--allow-dirty"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!(
                "cargo not available ({e}) — skipping \
                 cargo_package_ships_only_the_allowlist"
            );
            return;
        }
    };
    assert!(
        output.status.success(),
        "cargo package --list --allow-dirty failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listing = String::from_utf8_lossy(&output.stdout).into_owned();
    let files: Vec<&str> = listing
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert!(
        files.len() > 50,
        "cargo package --list returned only {} files — the listing is \
         probably broken, and a guard over nothing passes vacuously",
        files.len()
    );

    let offenders: Vec<&str> = files
        .iter()
        .copied()
        .filter(|f| !CARGO_PACKAGE_GENERATED.contains(f) && is_banned_packaged_path(f))
        .collect();
    assert!(
        offenders.is_empty(),
        "cargo package would ship a path Cargo.toml's `include` allow-list \
         should be keeping out:\n{}",
        offenders.join("\n")
    );

    // /etc/hostname doesn't exist on macOS (this repo's usual dev
    // machine) — skip that needle there rather than fail to find it.
    let hostname = fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut needles: Vec<&str> = vec!["/Users/", "/home/"];
    if let Some(h) = hostname.as_deref() {
        needles.push(h);
    }

    let mut leaks = Vec::new();
    for f in &files {
        if CARGO_PACKAGE_GENERATED.contains(f) || CONTENT_GREP_EXEMPT_FILES.contains(f) {
            continue;
        }
        let Ok(contents) = fs::read_to_string(f) else {
            continue; // binary (demo.gif) or otherwise unreadable as text
        };
        let scoped = strip_test_module_content(&contents);
        for needle in &needles {
            if scoped.contains(needle) {
                leaks.push(format!("{f} contains {needle:?}"));
            }
        }
    }
    assert!(
        leaks.is_empty(),
        "a packaged file's contents leak a machine-specific path — check \
         for a stray absolute path outside a #[cfg(test)] fixture:\n{}",
        leaks.join("\n")
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
        let (v, _) = shadowed_key_arms(src);
        assert_eq!(v.len(), 1, "the shadowed ctrl-d arm must be found: {v:?}");
        assert_eq!(v[0].ch, 'd');
        assert!(!v[0].catchall);
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

    // --- shapes the old scanner missed (item 5 of the release-risk
    // review) ---------------------------------------------------------

    #[test]
    fn a_catchall_above_a_guarded_arm_shadows_it() {
        // `KeyCode::Char(c)` with no guard matches *any* char, including
        // 'r' — the guarded arm below it is unreachable regardless of
        // whether ctrl is held.
        let src = "
        fn f(key: KeyEvent) {
            match key.code {
                KeyCode::Char(c) => self.type_char(c),
                KeyCode::Char('r') if ctrl => self.start_history_search(),
                _ => {}
            }
        }
        ";
        let (v, _) = shadowed_key_arms(src);
        assert_eq!(v.len(), 1, "the catch-all must be found to shadow: {v:?}");
        assert_eq!(v[0].ch, 'r');
        assert!(v[0].catchall);
    }

    #[test]
    fn a_wildcard_catchall_above_a_guarded_arm_shadows_it() {
        let src = "
        fn f(key: KeyEvent) {
            match key.code {
                KeyCode::Char(_) => {}
                KeyCode::Char('d') if ctrl => self.dlq(),
                _ => {}
            }
        }
        ";
        let (v, _) = shadowed_key_arms(src);
        assert_eq!(v.len(), 1);
        assert!(v[0].catchall);
    }

    #[test]
    fn a_catchall_after_every_guarded_arm_is_fine() {
        // The real keymap's actual convention: specific guarded arms
        // first, catch-all last. No violation.
        let src = "
        fn f(key: KeyEvent) {
            match key.code {
                KeyCode::Char('r') if ctrl => self.start_history_search(),
                KeyCode::Char('d') if ctrl => self.dlq(),
                KeyCode::Char(c) => self.type_char(c),
                _ => {}
            }
        }
        ";
        assert!(shadowed_key_arms(src).0.is_empty());
    }

    #[test]
    fn a_guard_on_a_continuation_line_is_still_recognised() {
        // The pattern and its `if ctrl =>` are split across lines — a
        // per-line scan would see the pattern line as unguarded.
        let src = "
        fn f(key: KeyEvent) {
            match key.code {
                KeyCode::Char('d') => self.detail(),
                KeyCode::Char('d')
                    if ctrl =>
                {
                    self.dlq();
                }
                _ => {}
            }
        }
        ";
        let (v, _) = shadowed_key_arms(src);
        assert_eq!(
            v.len(),
            1,
            "the continuation-line guard must still be recognised as a \
             guard, so the earlier unguarded 'd' shadows it: {v:?}"
        );
        assert_eq!(v[0].ch, 'd');
    }

    #[test]
    fn multiline_alternation_is_parsed() {
        let src = "
        fn f(key: KeyEvent) {
            match key.code {
                KeyCode::Char('y')
                    | KeyCode::Char('Y') => self.yank(),
                _ => {}
            }
        }
        ";
        let (_, guarded_concrete) = shadowed_key_arms(src);
        // Neither side of the alternation is guarded, so this doesn't
        // add to the guarded count — the real assertion is that parsing
        // a multi-line `|` doesn't panic or lose an arm; confirmed via
        // the no-violations check below instead.
        assert_eq!(guarded_concrete, 0);
        assert!(shadowed_key_arms(src).0.is_empty());
    }

    #[test]
    fn a_char_mention_inside_an_arm_body_is_not_a_new_arm() {
        // The body of the first arm *mentions* `KeyCode::Char('d')` in a
        // comment-free expression (e.g. logging what was pressed) — it
        // must not be read as a second, competing pattern for 'd'.
        let src = "
        fn f(key: KeyEvent) {
            match key.code {
                KeyCode::Char('x') => {
                    self.log_press(KeyCode::Char('d'));
                }
                KeyCode::Char('d') if ctrl => self.dlq(),
                _ => {}
            }
        }
        ";
        let (v, _) = shadowed_key_arms(src);
        assert!(
            v.is_empty(),
            "a Char('d') mentioned inside the 'x' arm's body must not be \
             treated as an unguarded 'd' arm: {v:?}"
        );
    }

    #[test]
    fn nested_match_code_blocks_are_their_own_list() {
        // An outer match key.code whose 'x' arm dispatches into another,
        // inner match key.code for a sub-mode. The inner block's
        // unguarded 'd' must not be merged into the outer block's arms
        // (where it would wrongly appear to shadow the outer's guarded
        // 'd' arm) — nor should the outer's arms leak into the inner.
        let src = "
        fn f(key: KeyEvent) {
            match key.code {
                KeyCode::Char('x') => {
                    match key.code {
                        KeyCode::Char('d') => self.inner_detail(),
                        _ => {}
                    }
                }
                KeyCode::Char('d') if ctrl => self.dlq(),
                _ => {}
            }
        }
        ";
        let lines: Vec<&str> = src.lines().collect();
        let blocks = match_code_blocks(&lines);
        assert_eq!(
            blocks.len(),
            2,
            "expected the outer and inner match key.code blocks to both \
             be found as separate blocks: {blocks:?}"
        );
        let (v, _) = shadowed_key_arms(src);
        assert!(
            v.is_empty(),
            "the inner block's unguarded 'd' must not shadow the outer \
             block's guarded 'd' — they're different match statements: \
             {v:?}"
        );
    }

    #[test]
    fn scans_every_src_file_not_two_hardcoded_ones() {
        // `ctrl_guarded_arms_precede_unguarded` walks `source_files()`
        // (all of src/**/*.rs) rather than a hardcoded two-file list —
        // this just pins that `match_code_blocks` + `shadowed_key_arms`
        // work the same regardless of which file the text came from,
        // by exercising them directly against a solitary literal.
        let src = "
        fn f(key: KeyEvent) {
            match key.code {
                KeyCode::Char('q') if ctrl => self.quit(),
                _ => {}
            }
        }
        ";
        let lines: Vec<&str> = src.lines().collect();
        assert_eq!(match_code_blocks(&lines).len(), 1);
        let (violations, guarded_concrete) = shadowed_key_arms(src);
        assert!(violations.is_empty());
        assert_eq!(guarded_concrete, 1);
    }

    // --- cfg(test) module skip / block comments (guard 3) ------------

    #[test]
    fn cfg_test_module_is_skipped_by_its_own_extent() {
        let src = "fn f() {}\n\n#[cfg(test)]\nmod tests {\n    let p = \"/Users/tom/x\";\n}\n";
        assert!(hardcoded_path_hits("x.rs", src).is_empty());
    }

    #[test]
    fn production_code_after_a_cfg_test_module_is_still_checked() {
        // Unlike the old "skip to EOF" behaviour, code after the test
        // module's closing brace is still scanned.
        let src = "#[cfg(test)]\nmod tests {\n    fn helper() {}\n}\n\nfn g() {\n    let p = \"/Users/tom/x\";\n}\n";
        let hits = hardcoded_path_hits("x.rs", src);
        assert_eq!(
            hits.len(),
            1,
            "the hardcoded path after the test module must still be \
             found: {hits:?}"
        );
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

    #[test]
    fn a_block_comment_mentioning_a_path_is_not_flagged() {
        let src = "/* old default was /Users/tom/.pgman */\nfn f() {}\n";
        assert!(hardcoded_path_hits("x.rs", src).is_empty());
    }

    #[test]
    fn a_block_comment_spanning_lines_is_stripped() {
        let src = "/*\n * see /home/tom/notes.txt\n */\nfn f() {}\n";
        assert!(hardcoded_path_hits("x.rs", src).is_empty());
    }

    // --- println exemption narrowing (guard 2) ------------------------

    #[test]
    fn println_inside_fn_main_is_allowed_in_main_rs() {
        let src = "fn main() {\n    println!(\"hi\");\n}\n";
        assert!(print_macro_hits("src/main.rs", src).is_empty());
    }

    #[test]
    fn println_outside_fn_main_is_flagged_in_main_rs() {
        let src = "fn helper() {\n    println!(\"hi\");\n}\n\nfn main() {}\n";
        let hits = print_macro_hits("src/main.rs", src);
        assert_eq!(
            hits.len(),
            1,
            "a println! in a function other than main/run_batch/*_cli \
             must still be flagged: {hits:?}"
        );
    }

    #[test]
    fn println_in_run_batch_is_allowed_only_in_main_rs() {
        let src = "fn run_batch() {\n    eprintln!(\"e\");\n}\n";
        assert!(print_macro_hits("src/main.rs", src).is_empty());
        assert_eq!(print_macro_hits("src/other.rs", src).len(), 1);
    }

    #[test]
    fn println_in_a_cli_suffixed_function_is_allowed_anywhere() {
        let src = "fn report_cli() {\n    println!(\"hi\");\n}\n";
        assert!(print_macro_hits("src/report.rs", src).is_empty());
    }
}
