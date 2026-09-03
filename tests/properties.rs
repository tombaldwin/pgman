//! Property-based tests with `proptest`. Each property describes an
//! invariant that should hold across millions of random inputs.
//! When a property fails proptest shrinks the input to the minimum
//! that still breaks the invariant — far more pointed than the
//! example-based unit tests we'd otherwise write.

use proptest::prelude::*;

use pgman::app::{compute_visible_rows, history_search_next, next_sort_state};
use pgman::conn::{redact_url, Dsn};
use pgman::grid::{cmp_cells, Grid};
use pgman::query::highlight::{tokenize, TokenClass};
use pgman::safety::{classify, split_statements, StatementKind};

// ----- Highlighter -----------------------------------------------------------

proptest! {
    /// The tokenizer must produce non-overlapping spans that, in order,
    /// cover every byte of the input. The renderer slices the input
    /// straight from these ranges; a gap or overlap would either drop
    /// glyphs or render the same byte twice.
    #[test]
    fn highlight_tokenize_covers_every_byte_in_order(buf in ".{0,200}") {
        let spans = tokenize(&buf);
        let mut next = 0;
        for s in &spans {
            prop_assert_eq!(s.start, next, "gap or overlap before {:?}", s);
            prop_assert!(s.start <= s.end, "inverted span {:?}", s);
            next = s.end;
        }
        prop_assert_eq!(next, buf.len(), "spans don't reach end of buffer");
    }

    /// Every span must slice at char boundaries — the renderer does
    /// `&buf[s.start..s.end]` and would panic mid-codepoint otherwise.
    #[test]
    fn highlight_spans_land_on_char_boundaries(buf in ".{0,200}") {
        let spans = tokenize(&buf);
        for s in spans {
            prop_assert!(buf.is_char_boundary(s.start), "start {} not boundary", s.start);
            prop_assert!(buf.is_char_boundary(s.end), "end {} not boundary", s.end);
        }
    }

    /// A pure ASCII identifier (alphanumeric + `_`, starts non-digit)
    /// MUST tokenize as a single Identifier or Keyword span — never
    /// fragmented or misclassified as Operator.
    #[test]
    fn highlight_identifier_is_one_span(
        s in "[a-z][a-z0-9_]{0,20}"
    ) {
        let spans = tokenize(&s);
        // Exactly one non-whitespace span.
        let ident_spans: Vec<_> = spans
            .iter()
            .filter(|sp| sp.class != TokenClass::Whitespace)
            .collect();
        prop_assert_eq!(ident_spans.len(), 1);
        let class = ident_spans[0].class;
        prop_assert!(
            matches!(
                class,
                TokenClass::Identifier | TokenClass::Keyword | TokenClass::Function
            ),
            "expected Identifier/Keyword/Function, got {:?}",
            class
        );
    }
}

// ----- DSN parser ------------------------------------------------------------

proptest! {
    /// Whatever input we throw at `Dsn::parse`, it never panics. Any
    /// scheme other than `postgres://`/`postgresql://` errors cleanly.
    #[test]
    fn dsn_parse_never_panics(s in ".{0,300}") {
        // Just don't panic — Err is fine.
        let _ = Dsn::parse(&s);
    }

    /// A well-formed `postgres://user@host:port/db` round-trips: parse
    /// → fields populated → no information loss (modulo password,
    /// which `redacted` masks).
    #[test]
    fn dsn_parse_round_trips_well_formed(
        user in "[a-z][a-z0-9_]{0,16}",
        host in "[a-z][a-z0-9.\\-]{0,32}",
        port in 1u16..=65535,
        db in "[a-z][a-z0-9_]{0,16}",
    ) {
        let dsn_str = format!("postgres://{user}@{host}:{port}/{db}");
        let dsn = Dsn::parse(&dsn_str).expect("well-formed");
        prop_assert_eq!(dsn.user.as_deref(), Some(user.as_str()));
        prop_assert_eq!(dsn.host, host);
        prop_assert_eq!(dsn.port, port);
        prop_assert_eq!(dsn.dbname, db);
    }

    /// Security-review regression: a user/password drawn from *any*
    /// printable ASCII — including the URL-structural characters
    /// `/ @ : ? #` — must round-trip through `Dsn::parse` once the
    /// caller percent-encodes it (pgman decodes userinfo, matching
    /// libpq), and `redact_url` on the same string must never leak
    /// any fragment of it. Before the fix, a raw (unencoded) `/` or
    /// `@` in the password broke both the authority/path split and
    /// the userinfo-masking scan — this property pins the fix, not
    /// just the reproduction cases.
    #[test]
    fn dsn_userinfo_round_trips_through_percent_encoding(
        user in "[\\x20-\\x7e]{1,24}",
        password in "[\\x20-\\x7e]{1,24}",
    ) {
        let dsn_str = format!(
            "postgres://{}:{}@h:5432/d",
            pct_encode(&user),
            pct_encode(&password),
        );
        let dsn = Dsn::parse(&dsn_str).expect("well-formed");
        prop_assert_eq!(dsn.user.as_deref(), Some(user.as_str()));
        prop_assert_eq!(dsn.password.as_deref(), Some(password.as_str()));

        // The authority/path (`h:5432/d`) never contains a raw '@',
        // '/', '?' or '#' introduced by the userinfo — so a correct
        // redactor must mask the ENTIRE userinfo down to a fixed,
        // password-independent string. Any leftover fragment of the
        // password here is a leak.
        let masked = redact_url(&dsn_str);
        prop_assert_eq!(masked, "postgres://***@h:5432/d");
    }

    /// The same round-trip, but with `/`, `@`, and `:` embedded *raw*
    /// (unescaped) rather than percent-encoded — this is what the
    /// security-review reproductions actually looked like, and it's
    /// the case the percent-encoded property above can't catch: with
    /// full encoding there's only ever one literal '@' in the string
    /// (the delimiter), so a "first '@'" bug and a "last '@'" fix
    /// behave identically. `:` is excluded from `user` (an unescaped
    /// `:` there is a genuine, unavoidable URI ambiguity — the first
    /// `:` in userinfo IS the user/password separator by definition);
    /// `?`/`#` are excluded from both *here* and covered by the two
    /// properties below instead, because a password holding one of
    /// them alongside a `/` is genuinely un-splittable. `%`
    /// is also excluded from both: since `Dsn::parse` now decodes
    /// percent-escapes (see `userinfo_is_percent_decoded`), a raw `%`
    /// followed by two hex digits is — correctly — itself an escape,
    /// not literal text; representing a literal `%` requires the
    /// caller to encode it as `%25`, same as `?`/`#`.
    #[test]
    fn dsn_userinfo_round_trips_with_raw_slash_and_at(
        user in "[\\x20-\\x22\\x24\\x26-\\x39\\x3b-\\x3e\\x40-\\x7e]{1,20}",
        password in "[\\x20-\\x22\\x24\\x26-\\x3e\\x40-\\x7e]{1,20}",
    ) {
        let dsn_str = format!("postgres://{user}:{password}@h:5432/d");
        let dsn = Dsn::parse(&dsn_str).expect("well-formed");
        prop_assert_eq!(dsn.user.as_deref(), Some(user.as_str()));
        prop_assert_eq!(dsn.password.as_deref(), Some(password.as_str()));

        let masked = redact_url(&dsn_str);
        prop_assert_eq!(masked, "postgres://***@h:5432/d");
    }

    /// Security-review regression: `?` and `#` are as legal in a raw
    /// password as `/` and `@`, and the userinfo scan used to cut the
    /// string at the first one of them *anywhere* — landing before the
    /// real `@`, so `postgres://u:p?ss@h/d` parsed `p` as the port and
    /// redacted to itself.
    ///
    /// `/` is excluded from the password here (and only here): a `?`
    /// after a `/` inside a password is indistinguishable from the
    /// query separator of a real path, so that one combination cannot
    /// round-trip. The redaction property below covers it anyway,
    /// which is the half that matters for a log.
    #[test]
    fn dsn_password_with_query_or_fragment_round_trips(
        password in "[\\x20-\\x24\\x26-\\x2e\\x30-\\x7e]{1,20}",
    ) {
        let dsn_str = format!("postgres://u:{password}@h:5432/d");
        let dsn = Dsn::parse(&dsn_str).expect("well-formed");
        prop_assert_eq!(dsn.user.as_deref(), Some("u"));
        prop_assert_eq!(dsn.password.as_deref(), Some(password.as_str()));
        prop_assert_eq!(dsn.host.as_str(), "h");
        prop_assert_eq!(dsn.port, 5432);
        prop_assert_eq!(dsn.dbname.as_str(), "d");

        prop_assert_eq!(redact_url(&dsn_str), "postgres://***@h:5432/d");
    }

    /// The unconditional half, and the one that decides whether a
    /// password reaches a log file: whatever printable ASCII the
    /// password is made of — `/`, `?`, `#`, `@`, `:` in any mixture,
    /// including the combinations `Dsn::parse` cannot take apart —
    /// `redact_url` masks all of it. `redact_url` is what runs on a
    /// DSN that *failed* to parse, so "unparseable" must never mean
    /// "printed verbatim".
    #[test]
    fn redact_url_never_leaks_any_raw_password(
        password in "[\\x20-\\x24\\x26-\\x7e]{1,24}",
    ) {
        let dsn_str = format!("postgres://u:{password}@h:5432/d");
        prop_assert_eq!(redact_url(&dsn_str), "postgres://***@h:5432/d");
    }
}

/// Percent-encode every byte outside RFC 3986's "unreserved" set —
/// test-fixture support only. `Dsn::parse`'s decoder (the pure logic
/// actually under test) is exercised by feeding it this URL.
fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ----- Safety classifier -----------------------------------------------------

proptest! {
    /// Any statement should classify into exactly one kind. Empty
    /// input is the one edge that returns `None`/Unknown — make sure
    /// the rest of the input space doesn't trip an assertion.
    #[test]
    fn safety_classify_never_panics(s in ".{0,300}") {
        let _ = classify(&s);
    }

    /// `SELECT …` is always classified as `Select`. The classifier
    /// shouldn't ever decide that a leading-SELECT statement is
    /// somehow a write — that's how a buffer ends up bypassing the
    /// read-only safety guard. `is_write()` on the result must be
    /// false.
    #[test]
    fn safety_select_is_always_a_read(tail in "[a-z0-9_*,. ]{0,80}") {
        let stmt = format!("SELECT {tail}");
        let kind = classify(&stmt);
        prop_assert_eq!(kind, StatementKind::Select);
        prop_assert!(!kind.is_write());
    }

    /// `split_statements` is idempotent on already-single statements.
    #[test]
    fn split_statements_keeps_single_intact(
        s in "[A-Z][A-Za-z0-9 _,]{0,80}"
    ) {
        let parts = split_statements(&s);
        // No `;` in the input means at most one part.
        prop_assert!(parts.len() <= 1);
    }
}

// ----- Pure decision helpers -------------------------------------------------

proptest! {
    /// `next_sort_state` is a 3-step cycle on a single column.
    #[test]
    fn sort_cycles_back_to_off_in_three_steps(col in 0usize..16) {
        let s0 = None;
        let s1 = next_sort_state(s0, col);
        prop_assert_eq!(s1, Some((col, true)));
        let s2 = next_sort_state(s1, col);
        prop_assert_eq!(s2, Some((col, false)));
        let s3 = next_sort_state(s2, col);
        prop_assert_eq!(s3, None);
    }

    /// `compute_visible_rows(None)` is the identity.
    #[test]
    fn unfiltered_visible_rows_are_identity(
        rows in proptest::collection::vec(
            proptest::collection::vec(".{0,8}", 0..4),
            0..30,
        ),
    ) {
        let v = compute_visible_rows(&rows, None);
        prop_assert_eq!(v, (0..rows.len()).collect::<Vec<_>>());
    }

    /// Filter is monotone: an unrelated pattern returns a SUBSET of
    /// the unfiltered set (in fact, a sub-sequence preserving order).
    #[test]
    fn filter_preserves_relative_order(
        rows in proptest::collection::vec(
            proptest::collection::vec("[a-z]{0,6}", 1..4),
            0..30,
        ),
        pattern in "[a-z]{1,3}",
    ) {
        let v = compute_visible_rows(&rows, Some(&pattern));
        // Indices ascending.
        for w in v.windows(2) {
            prop_assert!(w[0] < w[1]);
        }
        // Each index is in range.
        for i in &v {
            prop_assert!(*i < rows.len());
        }
    }

    /// `history_search_next` only ever returns indices < `from`.
    #[test]
    fn history_search_walks_strictly_backward(
        history in proptest::collection::vec("[a-z ]{0,12}", 0..20),
        needle in "[a-z]{1,4}",
        from in proptest::option::of(0usize..30),
    ) {
        let bounded_from = from.map(|f| f.min(history.len()));
        if let Some(i) = history_search_next(&history, &needle, bounded_from) {
            prop_assert!(i < bounded_from.unwrap_or(history.len()));
        }
    }
}

// ----- Grid cmp_cells --------------------------------------------------------

proptest! {
    /// `cmp_cells` is antisymmetric.
    #[test]
    fn cmp_cells_is_antisymmetric(a in ".{0,16}", b in ".{0,16}") {
        let ab = cmp_cells(&a, &b);
        let ba = cmp_cells(&b, &a);
        prop_assert_eq!(ab.reverse(), ba);
    }

    /// `cmp_cells` is reflexive (a == a is Equal).
    #[test]
    fn cmp_cells_is_reflexive(a in ".{0,16}") {
        prop_assert_eq!(cmp_cells(&a, &a), std::cmp::Ordering::Equal);
    }

    /// Empty cell is always `Greater` than a non-empty one (NULLS LAST).
    #[test]
    fn cmp_cells_null_last(b in "[^\\s].{0,16}") {
        prop_assert_eq!(cmp_cells("", &b), std::cmp::Ordering::Greater);
        prop_assert_eq!(cmp_cells(&b, ""), std::cmp::Ordering::Less);
    }
}

// ----- App state-machine fuzzer ----------------------------------------------

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pgman::app::{App, ConnState, Mode};
use pgman::theme::Theme;

/// Random key event covering the keys an operator can plausibly hit.
/// Avoid keys that interact with external state (the dispatcher /
/// process). Includes Tab/Enter/Esc/Backspace/Char + Ctrl/Shift mods.
fn arbitrary_key_event() -> impl Strategy<Value = KeyEvent> {
    let codes = prop_oneof![
        Just(KeyCode::Tab),
        Just(KeyCode::Enter),
        Just(KeyCode::Esc),
        Just(KeyCode::Backspace),
        Just(KeyCode::Delete),
        Just(KeyCode::Up),
        Just(KeyCode::Down),
        Just(KeyCode::Left),
        Just(KeyCode::Right),
        Just(KeyCode::Home),
        Just(KeyCode::End),
        // ASCII printable subset — covers the editor / normal /
        // grid keybindings without dragging in every codepoint.
        (32u8..=126u8).prop_map(|b| KeyCode::Char(b as char)),
    ];
    let mods = prop_oneof![
        Just(KeyModifiers::NONE),
        Just(KeyModifiers::SHIFT),
        Just(KeyModifiers::CONTROL),
    ];
    (codes, mods).prop_map(|(c, m)| KeyEvent::new(c, m))
}

fn settled_app() -> App {
    let mut a = App::new(
        Theme::default(),
        None,
        Vec::new(),
        pgman::safety::SafetyConfig::default(),
    );
    // Random key sequences reach Ctrl-S / Enter, which persists saved
    // queries — never to the operator's real ~/.local/share/pgman.
    let scratch = std::env::temp_dir().join(format!(
        "pgman-{}-{}-{}",
        "props",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    a.draft_file = scratch.join("draft.sql");
    a.history_file = scratch.join("history.log");
    a.saved_queries_file = scratch.join("saved.toml");
    a.splash_visible = false;
    a.splash_until = None;
    a.conn_state = ConnState::Connected {
        server_version: "16.0".into(),
    };
    a
}

proptest! {
    /// The app state machine should never panic regardless of what
    /// keys the operator mashes, and a handful of invariants should
    /// hold after every single keystroke.
    #[test]
    fn app_key_sequence_preserves_invariants(
        events in proptest::collection::vec(arbitrary_key_event(), 0..120),
        seed_buffer in "[a-zA-Z0-9 \n_]{0,40}",
        seed_history in proptest::collection::vec("[a-z ]{0,30}", 0..6),
    ) {
        let mut a = settled_app();
        a.mode = Mode::Editor;
        // Seed non-trivial starting state so the test exercises
        // history search, filter, completion, and cursor placement
        // over a wider envelope than "empty buffer, no history".
        a.editor.buffer = seed_buffer;
        a.editor.cursor = a.editor.buffer.len();
        a.history = seed_history;
        for ev in events {
            a.on_key(ev);

            // Cursor never leaves a char boundary in the editor
            // buffer (we slice by byte; this is the panic-prone
            // invariant).
            prop_assert!(
                a.editor.buffer.is_char_boundary(a.editor.cursor),
                "cursor {} not on char boundary in {:?}",
                a.editor.cursor,
                a.editor.buffer
            );
            // Cursor is in range.
            prop_assert!(a.editor.cursor <= a.editor.buffer.len());

            // grid_view.visible_rows is a subset of 0..rows.len() (the
            // filter helper guarantees this, but the random key
            // sequence has typed `/` and `n`/`N` etc. — make sure
            // it's still respected).
            for &i in &a.grid_view.visible_rows {
                prop_assert!(i < a.grid.rows.len().max(1));
            }
            // Selected row (if any) points into the visible set.
            if let Some(sel) = a.grid_state.selected() {
                prop_assert!(
                    sel < a.grid_view.visible_rows.len().max(1),
                    "selected {sel} out of visible_rows len {}",
                    a.grid_view.visible_rows.len()
                );
            }
            // Mode is one of the legal variants (this would panic
            // at the `match` if it weren't, but assertion as a
            // documenting checkpoint).
            let _: Mode = a.mode;
        }
        // If we reached here without a panic across the sequence,
        // the App is robust to arbitrary key mashing.
    }
}

// ----- Grid sort stability ---------------------------------------------------

proptest! {
    /// Sorting a grid twice ASC yields the same order. (`sort_by` is
    /// stable, so this also serves as a guard against the App's
    /// snapshot-then-sort path ever introducing instability.)
    #[test]
    fn grid_sort_is_idempotent(
        rows in proptest::collection::vec(
            (0i32..1000, "[a-z]{0,8}"),
            0..40,
        ),
    ) {
        let mut g = Grid {
            columns: vec!["id".into(), "name".into()],
            rows: rows
                .iter()
                .map(|(i, s)| vec![i.to_string(), s.clone()])
                .collect(),
                    truncated: false,
        };
        g.rows.sort_by(|a, b| cmp_cells(&a[0], &b[0]));
        let after_first = g.rows.clone();
        g.rows.sort_by(|a, b| cmp_cells(&a[0], &b[0]));
        prop_assert_eq!(after_first, g.rows);
    }
}
