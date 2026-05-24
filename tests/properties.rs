//! Property-based tests with `proptest`. Each property describes an
//! invariant that should hold across millions of random inputs.
//! When a property fails proptest shrinks the input to the minimum
//! that still breaks the invariant — far more pointed than the
//! example-based unit tests we'd otherwise write.

use proptest::prelude::*;

use pgman::app::{compute_visible_rows, history_search_next, next_sort_state};
use pgman::conn::Dsn;
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
        };
        g.rows.sort_by(|a, b| cmp_cells(&a[0], &b[0]));
        let after_first = g.rows.clone();
        g.rows.sort_by(|a, b| cmp_cells(&a[0], &b[0]));
        prop_assert_eq!(after_first, g.rows);
    }
}
