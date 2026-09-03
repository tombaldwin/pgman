use super::*;

#[test]
fn centered_pct_does_not_overflow_on_a_wide_terminal() {
    use ratatui::layout::Rect;
    // Regression: `area.width * w` was a u16 multiply that overflowed once a
    // terminal hit ~713+ columns (1000 × 92 = 92000 > 65535) — panic in
    // debug/test, garbage rect in release. The u32 path stays correct.
    let area = Rect {
        x: 0,
        y: 0,
        width: 1000,
        height: 800,
    };
    let r = centered_pct(area, 92, 90);
    assert_eq!(r.width, 920);
    assert_eq!(r.height, 720);
    // Stays inside the parent area (no underflow in the centring math either).
    assert!(r.x + r.width <= area.width);
    assert!(r.y + r.height <= area.height);
}

#[test]
fn format_duration_picks_unit_by_magnitude() {
    assert_eq!(format_duration(0), "0µs");
    assert_eq!(format_duration(999), "999µs");
    assert_eq!(format_duration(1_000), "1.0ms");
    assert_eq!(format_duration(1_500), "1.5ms");
    assert_eq!(format_duration(999_999), "1000.0ms");
    assert_eq!(format_duration(1_000_000), "1.00s");
    assert_eq!(format_duration(3_500_000), "3.50s");
}

#[test]
fn wrap_value_splits_on_existing_newlines() {
    let got = wrap_value("a\nb\nc", 80);
    assert_eq!(got, vec!["a", "b", "c"]);
}

#[test]
fn wrap_value_chunks_long_lines() {
    let got = wrap_value("abcdefghij", 4);
    assert_eq!(got, vec!["abcd", "efgh", "ij"]);
}

#[test]
fn wrap_value_handles_empty_string() {
    // Empty value should still emit one line so the row stays visible.
    assert_eq!(wrap_value("", 80), vec![""]);
}

#[test]
fn wrap_value_preserves_blank_lines() {
    let got = wrap_value("a\n\nb", 80);
    assert_eq!(got, vec!["a", "", "b"]);
}

#[test]
fn wrap_value_width_zero_returns_input_unchanged() {
    // Defensive: a popup so narrow there's no room for the value column.
    // Falling back to one line keeps render functions from panicking on
    // `chunks(0)`.
    assert_eq!(wrap_value("hello", 0), vec!["hello"]);
}

#[test]
fn clamp_editor_scroll_holds_when_cursor_inside_viewport() {
    // 20-line buffer, 5-row viewport, cursor on line 7, current scroll 3.
    // Viewport shows lines 3..8 — cursor visible — no change.
    assert_eq!(clamp_editor_scroll(3, 7, 20, 5), 3);
}

#[test]
fn clamp_editor_scroll_pulls_down_when_cursor_above() {
    // Cursor moved up to line 1; viewport was at line 5.
    // Scroll up to make cursor the new top row.
    assert_eq!(clamp_editor_scroll(5, 1, 20, 5), 1);
}

#[test]
fn clamp_editor_scroll_pushes_up_when_cursor_below_viewport() {
    // Viewport 0..5, cursor moved to line 12 — bring it to last
    // visible row (so cursor sits at row 4 of the pane).
    assert_eq!(clamp_editor_scroll(0, 12, 20, 5), 12 - 5 + 1);
}

#[test]
fn clamp_editor_scroll_caps_at_total_minus_visible() {
    // Don't reveal blank rows past the buffer's end.
    assert_eq!(clamp_editor_scroll(50, 19, 20, 5), 15);
}

#[test]
fn clamp_editor_scroll_returns_zero_when_buffer_fits() {
    // Buffer fits in one pane — no scrolling possible.
    assert_eq!(clamp_editor_scroll(3, 4, 5, 10), 0);
}

#[test]
fn clamp_editor_scroll_handles_zero_visible() {
    // Defensive: degenerate pane height.
    assert_eq!(clamp_editor_scroll(7, 5, 20, 0), 0);
}

#[test]
fn wrap_value_strips_carriage_returns_in_crlf_input() {
    // CRLF must not leak raw `\r` into the output — crossterm would
    // jump the cursor to column 0 and corrupt the row.
    let got = wrap_value("a\r\nb\r\nc", 80);
    assert_eq!(got, vec!["a", "b", "c"]);
}

#[test]
fn wrap_value_handles_multibyte_chars_by_chars_not_bytes() {
    // "café" is 4 chars / 5 bytes — chunk by chars so a slice doesn't
    // land mid-codepoint.
    let got = wrap_value("café", 2);
    assert_eq!(got, vec!["ca", "fé"]);
}

#[test]
fn build_field_layout_pads_label_and_marks_empty() {
    let cols = vec!["id".to_string(), "long_column_name".to_string()];
    let row = vec!["42".to_string(), "".to_string()];
    let got = build_field_layout(&cols, &row, 16, 40);
    assert_eq!(got.len(), 2);
    // Labels padded to label_width.
    assert_eq!(got[0].label.chars().count(), 16);
    assert!(got[0].label.starts_with("id"));
    assert!(!got[0].is_empty);
    assert_eq!(got[0].values, vec!["42"]);
    // Empty cell rendered with "(empty)" sentinel.
    assert!(got[1].is_empty);
    assert_eq!(got[1].values, vec!["(empty)"]);
}

#[test]
fn build_field_layout_truncates_oversized_labels() {
    let cols = vec!["aaaaaaaaaaaaaaaaaaaaaaaaa".to_string()]; // 25 chars
    let row = vec!["v".to_string()];
    let got = build_field_layout(&cols, &row, 8, 10);
    assert_eq!(got[0].label.chars().count(), 8);
    assert!(got[0].label.starts_with("aaaaaaaa"));
}

#[test]
fn scroll_offset_keeps_cursor_in_view() {
    // Cursor before the last visible row — no scroll yet.
    assert_eq!(scroll_offset(0, 5), 0);
    assert_eq!(scroll_offset(4, 5), 0);
    // Cursor on the first row past the window — scroll by one.
    assert_eq!(scroll_offset(5, 5), 1);
    // Cursor well past the window — keep it on the last visible row.
    assert_eq!(scroll_offset(12, 5), 8);
    // Degenerate zero-height viewport: cursor (>=0) is always "past",
    // so the offset is cursor + 1.
    assert_eq!(scroll_offset(0, 0), 1);
    assert_eq!(scroll_offset(3, 0), 4);
}

#[test]
fn build_field_layout_wraps_long_values() {
    let cols = vec!["bio".to_string()];
    let row = vec!["abcdefghij".to_string()];
    let got = build_field_layout(&cols, &row, 4, 4);
    assert_eq!(got[0].values, vec!["abcd", "efgh", "ij"]);
}

#[test]
fn auto_scroll_keeps_focused_field_in_view_when_moving_down() {
    // Three fields, 2 lines each = 6 total lines. Viewport = 3 rows.
    // max_scroll = 3. Focus field 2 (lines 4..6) — need scroll = 3.
    let counts = vec![2u16, 2, 2];
    let scroll = auto_scroll_to_field(&counts, 2, 0, 3, 3);
    assert_eq!(scroll, 3);
}

#[test]
fn auto_scroll_scrolls_up_when_focus_moves_above_viewport() {
    // Same shape; user had scrolled down to 3 (last field visible),
    // then moves focus back to field 0. Scroll should snap to 0.
    let counts = vec![2u16, 2, 2];
    let scroll = auto_scroll_to_field(&counts, 0, 3, 3, 3);
    assert_eq!(scroll, 0);
}

#[test]
fn auto_scroll_clamps_to_max_scroll() {
    let counts = vec![2u16, 2];
    let scroll = auto_scroll_to_field(&counts, 1, 999, 4, 0);
    // max_scroll = 0 — everything fits — auto-scroll must clamp.
    assert_eq!(scroll, 0);
}

#[test]
fn auto_scroll_handles_empty_field_list() {
    let scroll = auto_scroll_to_field(&[], 0, 5, 4, 10);
    assert_eq!(scroll, 5);
}

#[test]
fn auto_scroll_handles_zero_height_viewport() {
    let counts = vec![1u16];
    let scroll = auto_scroll_to_field(&counts, 0, 2, 0, 5);
    // body_height=0 → no-op except for the max_scroll clamp.
    assert_eq!(scroll, 2);
}

fn settled_app(read_only: bool, tx_open: bool) -> App {
    use crate::safety::SafetyConfig;
    use crate::theme::Theme;
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.read_only = read_only;
    a.tx_open = tx_open;
    a
}

#[test]
fn footer_badges_empty_when_nothing_to_signal() {
    // Pin dropped=0 via the `_with` variant so a leaked
    // count from another test running in the same process
    // can't flip this assertion.
    let a = settled_app(false, false);
    assert!(footer_badges_with(&a, &a.theme, 0).is_empty());
}

#[test]
fn footer_badges_render_ro_then_tx_in_stable_order() {
    let a = settled_app(true, true);
    let spans = footer_badges_with(&a, &a.theme, 0);
    // Pairs of (badge, space). Length 4: " RO ", " ", " TX ", " ".
    assert_eq!(spans.len(), 4);
    assert_eq!(spans[0].content, " RO ");
    assert_eq!(spans[1].content, " ");
    assert_eq!(spans[2].content, " TX ");
}

#[test]
fn footer_badges_show_only_active_ones() {
    let a = settled_app(true, false);
    let spans = footer_badges_with(&a, &a.theme, 0);
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].content, " RO ");
    let a = settled_app(false, true);
    let spans = footer_badges_with(&a, &a.theme, 0);
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].content, " TX ");
}

#[test]
fn footer_badges_drop_counter_surfaces_amber_badge() {
    let a = settled_app(false, false);
    let spans = footer_badges_with(&a, &a.theme, 42);
    let labels: Vec<String> = spans
        .iter()
        .map(|s| s.content.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    assert!(
        labels.iter().any(|l| l.contains("DROP ×42")),
        "expected DROP badge with count 42: {labels:?}"
    );
}

#[test]
fn fit_hints_returns_unchanged_when_everything_fits() {
    let hints = "q quit · ? help · e editor";
    assert_eq!(fit_hints(hints, hints.chars().count()), hints);
    assert_eq!(fit_hints(hints, hints.chars().count() + 10), hints);
}

#[test]
fn fit_hints_never_cuts_mid_item() {
    let hints = "q quit · ? help · e editor · S schema · W wizard";
    for width in 0..=hints.chars().count() {
        let fitted = fit_hints(hints, width);
        assert!(
            fitted.chars().count() <= width,
            "fitted {fitted:?} exceeds width {width}"
        );
        if fitted.is_empty() {
            continue;
        }
        // Every remaining piece (split on " · ") must be either a whole
        // item from the source string, or the trailing "F1 +N more" marker —
        // never a partial hint.
        for piece in fitted.split(" · ") {
            let is_marker = piece
                .strip_prefix("F1 +")
                .and_then(|rest| rest.strip_suffix(" more"))
                .is_some_and(|n| n.parse::<usize>().is_ok());
            assert!(
                is_marker || hints.split(" · ").any(|item| item == piece),
                "piece {piece:?} at width {width} is neither a whole hint nor the marker (fitted: {fitted:?})"
            );
        }
    }
}

#[test]
fn fit_hints_width_too_small_for_first_hint_yields_just_the_marker() {
    let hints = "q quit · ? help · e editor";
    // Too narrow even for "q quit" plus a marker, but the bare
    // "F1 +3 more" marker (10 chars) fits.
    let fitted = fit_hints(hints, 10);
    assert_eq!(fitted, "F1 +3 more");
}

#[test]
fn fit_hints_mid_list_cut_drops_whole_items_and_appends_marker() {
    let hints = "q quit · ? help · e editor · S schema · W wizard";
    // Fits "q quit · ? help · e editor" (26 chars) + " · F1 +2 more"
    // (13) = 39.
    let fitted = fit_hints(hints, 39);
    assert_eq!(fitted, "q quit · ? help · e editor · F1 +2 more");
    assert!(fitted.chars().count() <= 39);
}

#[test]
fn fit_hints_marker_width_is_always_accounted_for() {
    let hints = "aaaaaaaaaa · bbbbbbbbbb · cccccccccc";
    // Any width narrower than the full string must produce output no
    // wider than that width — including the marker itself.
    for width in [0, 1, 5, 8, 9, 10, 15, 20] {
        let fitted = fit_hints(hints, width);
        assert!(
            fitted.chars().count() <= width,
            "fitted {fitted:?} ({}  chars) exceeds width {width}",
            fitted.chars().count()
        );
    }
}

#[test]
fn fit_hints_empty_when_nothing_at_all_fits() {
    // Not even a single-digit "F1 +N more" marker (10 chars) fits in 3.
    let hints = "q quit · ? help";
    assert_eq!(fit_hints(hints, 3), "");
}

#[test]
fn fit_status_returns_unchanged_when_everything_fits() {
    let text = "terminate pid 1234? \"UPDATE accounts SET balance = 0\" · y confirm · n cancel";
    assert_eq!(fit_status(text, text.chars().count()), text);
    assert_eq!(fit_status(text, text.chars().count() + 10), text);
}

#[test]
fn fit_status_shrinks_the_longest_non_last_segment_first() {
    let text = "terminate pid 1234? \"UPDATE accounts SET balance = 0\" · y confirm · n cancel";
    let fitted = fit_status(text, 60);
    assert_eq!(
        fitted,
        "terminate pid 1234…s SET balance = 0\" · y confirm · n cancel"
    );
    assert!(fitted.chars().count() <= 60);
    // The action-key segments (the whole point of the fix) survive
    // untouched — only the quoted SQL got the middle-ellipsis treatment.
    assert!(fitted.ends_with("y confirm · n cancel"));
}

#[test]
fn fit_status_drops_leading_segments_once_others_are_fully_ellipsised() {
    let text = "terminate pid 1234? \"UPDATE accounts SET balance = 0\" · y confirm · n cancel";
    // Both non-last segments get collapsed to a bare "…" first; that
    // still doesn't fit 15, so the leading "…" segment is dropped
    // outright rather than the protected last segment being touched.
    let fitted = fit_status(text, 15);
    assert_eq!(fitted, "… · n cancel");
    assert!(fitted.chars().count() <= 15);
    assert!(fitted.ends_with("n cancel"));
}

#[test]
fn fit_status_end_ellipsises_the_last_segment_as_a_last_resort() {
    let text = "terminate pid 1234? \"UPDATE accounts SET balance = 0\" · y confirm · n cancel";
    assert_eq!(fit_status(text, 8), "n cancel"); // exact fit — no ellipsis needed
    assert_eq!(fit_status(text, 5), "n ca…");
    assert_eq!(fit_status(text, 3), "n …");
    assert_eq!(fit_status(text, 1), "…");
    assert_eq!(fit_status(text, 0), "");
}

#[test]
fn fit_status_drops_short_middle_segments_whole_instead_of_mangling_a_key_hint() {
    // The real 80-column grid-find footer, two characters over budget.
    // The only non-last segments are short key hints; middle-ellipsis
    // would produce `enter…cept`. Drop `1/3 match` whole instead.
    let text = "find: pro  · 1/3 match · n/N jump · enter accept · esc cancel";
    let fitted = fit_status(text, text.chars().count() - 2);
    assert_eq!(fitted, "find: pro  · n/N jump · enter accept · esc cancel");
    assert!(
        !fitted.contains('…'),
        "no key hint may be ellipsised: {fitted:?}"
    );
}

#[test]
fn fit_status_never_exceeds_width_or_ends_in_a_partial_word() {
    // Real footer status strings (confirm prompt, tip) plus a couple of
    // edge shapes (a segment that already contains a real "…" glyph, an
    // empty string) swept across every width from 0 up to the full
    // length. Property: the result is never wider than asked, and its
    // trailing word is always either a real word from the source text
    // (an untouched, or wholesale-dropped, segment) or ends with the
    // ellipsis marker (an intentional truncation) — never a raw
    // mid-word cut.
    let samples = [
        "terminate pid 1234? \"UPDATE accounts SET balance = 0\" · y confirm · n cancel",
        "tip · JSON cells render as a tree · y yanks the value (or jq path)",
        "connecting… · q quit",
        "no separators here at all",
        "",
    ];
    for text in samples {
        let words: std::collections::HashSet<&str> = text.split_whitespace().collect();
        for width in 0..=text.chars().count() {
            let fitted = fit_status(text, width);
            assert!(
                fitted.chars().count() <= width,
                "text={text:?} width={width} fitted={fitted:?} exceeds width"
            );
            if let Some(last_word) = fitted.split_whitespace().last() {
                let ok = last_word.ends_with('…') || words.contains(last_word);
                assert!(
                    ok,
                    "text={text:?} width={width} fitted={fitted:?} \
                     last_word={last_word:?} looks like a mid-word cut"
                );
            }
        }
    }
}

#[test]
fn tap_setup_hint_includes_otel_and_pgman_tap_routes() {
    let theme = crate::theme::Theme::default();
    let lines = tap_setup_hint_lines(&theme);
    let dump: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
        .collect::<Vec<_>>()
        .join("\n");
    // Both routes named.
    assert!(dump.contains("Route 1: OpenTelemetry"), "got:\n{dump}");
    assert!(dump.contains("Route 2: pgman-tap"), "got:\n{dump}");
    // The flag + env vars the operator needs.
    assert!(dump.contains("--tap-otlp :4318"), "got:\n{dump}");
    assert!(
        dump.contains("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"),
        "got:\n{dump}"
    );
    assert!(dump.contains("OTEL_EXPORTER_OTLP_PROTOCOL"), "got:\n{dump}");
    // Route 2 is not shipped, so it prints no install instructions at
    // all: the coordinate that used to be here resolved to nothing,
    // and an application.yml snippet for a JAR that cannot be fetched
    // is six lines of work that ends in a build failure.
    assert!(
        dump.contains("Route 2: pgman-tap — not yet released"),
        "got:\n{dump}"
    );
    assert!(
        !dump.contains("pgman-tap-spring-boot-starter"),
        "a coordinate that resolves to nothing was printed as if it \
         were installable; got:\n{dump}"
    );
    assert!(!dump.contains("pgman.tap.enabled"), "got:\n{dump}");
    assert!(!dump.contains("pgman.tap.endpoint"), "got:\n{dump}");
    // Says what it will add, and points at the route that works.
    assert!(
        dump.contains("caller, pool and transaction context"),
        "got:\n{dump}"
    );
    assert!(
        dump.contains("use Route 1 today"),
        "expected a pointer at the route that works today; got:\n{dump}"
    );
}

#[test]
fn footer_badges_surface_tap_and_nplus1_when_findings_exist() {
    let mut a = settled_app(false, false);
    // Seed 6 same-shape events in one txn within window:
    // detect_nplus1 fires one finding.
    for i in 0..6u64 {
        a.on_tap_event(crate::tap::TapEvent {
            v: 1,
            kind: crate::tap::TapKind::Query,
            ts_unix_micros: i * 20_000,
            received_at_unix_micros: i * 20_000,
            app: Some("svc".into()),
            pool: None,
            conn: Some("c-1".into()),
            txn: Some("c-1#1".into()),
            sql: Some("SELECT * FROM t WHERE id = ?".into()),
            params: None,
            params_redacted: false,
            duration_micros: Some(1),
            rows: None,
            error: None,
            caller: None,
            dropped_events_total: None,
            txn_outcome: None,
        });
    }
    let spans = footer_badges_with(&a, &a.theme, 0);
    let labels: Vec<String> = spans
        .iter()
        .map(|s| s.content.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    assert!(
        labels.iter().any(|l| l == "TAP"),
        "expected TAP badge: {labels:?}"
    );
    assert!(
        labels.iter().any(|l| l.contains("N+1 ×1")),
        "expected N+1 ×1 badge: {labels:?}"
    );
}

#[test]
fn short_server_version_drops_the_packager_build_detail() {
    // The string Postgres actually reports on a Debian package.
    assert_eq!(
        short_server_version("16.15 (Debian 16.15-1.pgdg13+2)"),
        "16.15"
    );
    assert_eq!(
        short_server_version("17.2 (Ubuntu 17.2-1.pgdg24.04+1)"),
        "17.2"
    );
}

#[test]
fn short_server_version_passes_a_bare_version_through() {
    assert_eq!(short_server_version("16.2"), "16.2");
    assert_eq!(short_server_version(""), "");
}

#[test]
fn short_server_version_cuts_at_the_first_paren_only() {
    // Defensive: only the first " (" separates version from detail —
    // a nested paren in the build id must not re-split the tail.
    assert_eq!(
        short_server_version("16.15 (Debian (x86_64) build)"),
        "16.15"
    );
    // A parenthesis with no leading space is part of the version and
    // stays: nothing is cut that wasn't clearly a build annotation.
    assert_eq!(short_server_version("16.2(demo)"), "16.2(demo)");
}

#[test]
fn count_label_pluralises_everything_but_one() {
    assert_eq!(count_label(0, "query", "queries"), "0 queries");
    assert_eq!(count_label(1, "query", "queries"), "1 query");
    assert_eq!(count_label(8, "query", "queries"), "8 queries");
    assert_eq!(count_label(1, "heartbeat", "heartbeats"), "1 heartbeat");
}

#[test]
fn fit_title_returns_a_title_that_already_fits() {
    assert_eq!(fit_title("JDBC tap · q close", 40), "JDBC tap · q close");
    // Exactly the budget is a fit, not a cut.
    assert_eq!(fit_title("abcde", 5), "abcde");
}

#[test]
fn fit_title_drops_whole_trailing_segments_and_marks_the_cut() {
    let title = "JDBC tap — 8 queries · view: list · v cycle · Shift-B baseline · q close";
    let got = fit_title(title, 40);
    assert!(got.chars().count() <= 40, "{got:?}");
    assert!(got.starts_with("JDBC tap — 8 queries"), "{got:?}");
    assert!(got.ends_with(" · …"), "{got:?}");
    // Never a word cut in half: every kept segment is intact.
    assert!(!got.contains("Shift-B base"), "{got:?}");
}

#[test]
fn fit_title_ellipsises_the_first_segment_when_even_it_does_not_fit() {
    // No whole-segment prefix fits — the panel's identity is kept and
    // ellipsised rather than the title vanishing.
    let got = fit_title("JDBC tap — 8 queries · q close", 12);
    assert_eq!(got, "JDBC tap — …");
    assert!(got.chars().count() <= 12);
}

#[test]
fn fit_title_handles_a_zero_budget() {
    assert_eq!(fit_title("JDBC tap · q close", 0), "");
}

// ---------------------------------------------------------------
// Display width, not char count. A Postgres server running with
// `lc_messages=ja_JP` reports errors in Japanese: every glyph is two
// terminal columns, so a message that "fits" by char count paints
// twice as wide and shoves the protected trailing segment off the row.
// ---------------------------------------------------------------

/// 「重複したキーの値が…」 — 22 chars, 44 display columns.
const JA: &str = "重複したキーの値が一意性制約に違反しています";

#[test]
fn display_width_counts_columns_not_chars() {
    assert_eq!(JA.chars().count(), 22);
    assert_eq!(display_width(JA), 44);
    assert_eq!(display_width("abc"), 3);
    // A combining mark adds no column of its own.
    assert_eq!(display_width("e\u{301}"), 1);
}

#[test]
fn fit_status_keeps_the_protected_tail_within_a_cjk_budget() {
    let text = format!("{JA} · F2 detail");
    let got = fit_status(&text, 40);
    assert!(
        display_width(&got) <= 40,
        "fitted line is {} columns wide: {got:?}",
        display_width(&got)
    );
    assert!(
        got.ends_with("· F2 detail"),
        "the protected pointer was clipped: {got:?}"
    );
}

#[test]
fn fit_hints_measures_cjk_hints_in_columns() {
    let hints = format!("{JA} · b · c");
    let got = fit_hints(&hints, 20);
    assert!(
        display_width(&got) <= 20,
        "fitted hints are {} columns wide: {got:?}",
        display_width(&got)
    );
}

#[test]
fn middle_ellipsis_never_overruns_a_cjk_budget() {
    for target in 0..=20 {
        let got = middle_ellipsis(JA, target);
        assert!(
            display_width(&got) <= target.max(1),
            "target {target}: {got:?} is {} columns",
            display_width(&got)
        );
    }
    // A double-width glyph that only half fits is dropped, not cut:
    // budget 4 = marker (1) + 2 front columns + 1 back column, and one
    // column can't hold a 2-column glyph.
    assert_eq!(middle_ellipsis("あいうえお", 4), "あ…");
}

#[test]
fn end_ellipsis_never_overruns_a_cjk_budget() {
    for width in 0..=20 {
        let got = end_ellipsis(JA, width);
        assert!(
            display_width(&got) <= width.max(1),
            "width {width}: {got:?} is {} columns",
            display_width(&got)
        );
    }
    assert_eq!(end_ellipsis("あいうえお", 5), "あい…");
}

// ----- Wizard detail wraps at word boundaries ---------------------------

#[test]
fn wrap_words_breaks_between_words_not_inside_them() {
    let s = "constraint `x` references column(s) (a) — no index leads with the first FK column";
    let rows = wrap_words(s, 30);
    for row in &rows {
        assert!(display_width(row) <= 30, "{row:?}");
    }
    // Every row boundary is a word boundary: joining with spaces gives
    // the original back.
    assert_eq!(rows.join(" "), s);
    assert_eq!(
        rows,
        vec![
            "constraint `x` references",
            "column(s) (a) — no index leads",
            "with the first FK column",
        ]
    );
}

#[test]
fn wrap_words_hard_splits_a_word_wider_than_the_line_and_keeps_newlines() {
    assert_eq!(wrap_words("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    assert_eq!(
        wrap_words("ab abcdefghij cd", 4),
        vec!["ab", "abcd", "efgh", "ij", "cd"]
    );
    assert_eq!(wrap_words("a\nb c", 80), vec!["a", "b c"]);
    assert_eq!(wrap_words("", 10), vec![""]);
    assert_eq!(wrap_words("hello", 0), vec!["hello"]);
    // Columns, not chars: two CJK glyphs fill a four-column row.
    assert_eq!(wrap_words("東京 都", 4), vec!["東京", "都"]);
}

// ----- The failure card names the dated log file -----------------------

#[test]
fn dated_log_file_name_is_the_daily_rollers_utc_date() {
    assert_eq!(dated_log_file_name(0), "pgman.log.1970-01-01");
    assert_eq!(dated_log_file_name(951_782_400), "pgman.log.2000-02-29"); // leap day
    assert_eq!(dated_log_file_name(1_700_000_000), "pgman.log.2023-11-14");
    assert_eq!(dated_log_file_name(1_704_067_199), "pgman.log.2023-12-31"); // 23:59:59
    assert_eq!(dated_log_file_name(1_704_067_200), "pgman.log.2024-01-01");
    assert_eq!(dated_log_file_name(4_102_444_800), "pgman.log.2100-01-01");
}

// ----- Prose segments are cut at word boundaries ------------------------

#[test]
fn looks_like_sql_tells_a_statement_from_prose_about_one() {
    assert!(looks_like_sql(
        "terminate pid 1234? \"UPDATE accounts SET balance = 0\""
    ));
    assert!(looks_like_sql(
        "SELECT * FROM orders WHERE status = 'shipped'"
    ));
    assert!(looks_like_sql("delete from item where id = 1"));
    assert!(looks_like_sql("INSERT INTO t VALUES (1)"));
    assert!(looks_like_sql("'select 1'"));
    // Prose that merely mentions the keywords.
    assert!(!looks_like_sql("run (DELETE without WHERE)"));
    assert!(!looks_like_sql(
        "blocked by safety: DELETE without WHERE on 'main'"
    ));
    assert!(!looks_like_sql(
        "hint: this connection is read-only by safety.toml (/x/safety.toml, read_only) — see docs/configuration.md"
    ));
    assert!(!looks_like_sql(
        "cannot execute DELETE in a read-only transaction"
    ));
}

#[test]
fn middle_ellipsis_cuts_prose_at_word_boundaries_keeping_the_last_word() {
    assert_eq!(
        middle_ellipsis("run (DELETE without WHERE)", 20),
        "run (DELETE… WHERE)"
    );
    let hint = "hint: this connection is read-only by safety.toml (/x/safety.toml, read_only) — see docs/configuration.md";
    let got = middle_ellipsis(hint, 50);
    assert_eq!(got, "hint: this connection is… docs/configuration.md");
    assert!(display_width(&got) <= 50);
    // Never a cut inside a word once `… docs/configuration.md` (the
    // ellipsis, a space, the last word) fits at all: the marker always
    // borders a space or a word edge. Narrower than that there is no
    // word boundary to honour and the character cut takes over.
    let floor = 2 + display_width("docs/configuration.md");
    for width in floor..hint.len() {
        let got = middle_ellipsis(hint, width);
        assert!(display_width(&got) <= width, "{width}: {got:?}");
        if let Some(i) = got.find('…') {
            let before = got[..i].chars().next_back();
            let after = got[i + '…'.len_utf8()..].chars().next();
            assert!(
                !(before.is_some_and(char::is_alphanumeric)
                    && after.is_some_and(char::is_alphanumeric)),
                "mid-word cut at width {width}: {got:?}"
            );
        }
    }
}

#[test]
fn middle_ellipsis_keeps_the_character_cut_for_a_statement() {
    // Both ends of the SQL survive, words or not.
    assert_eq!(
        middle_ellipsis("SELECT * FROM orders WHERE status = 'shipped'", 20),
        "SELECT * F…'shipped'"
    );
    // A single over-long word has no boundary to cut at: falls back.
    assert_eq!(middle_ellipsis("abcdefghijklmnop", 7), "abc…nop");
}

#[test]
fn fit_status_never_cuts_a_prose_segment_mid_word() {
    let text = "confirm: run (DELETE without WHERE) · y run · n / esc cancel";
    let fitted = fit_status(text, 45);
    assert!(display_width(&fitted) <= 45);
    assert!(fitted.ends_with("y run · n / esc cancel"));
    assert!(
        !fitted.contains("w…hout") && fitted.contains("… WHERE)"),
        "{fitted:?}"
    );
}

// ----- Confirm card: no transaction promised on a read-only session ----

#[test]
fn confirm_wrap_note_says_refused_under_read_only_instead_of_wrapping() {
    use super::panels::confirm_wrap_note;
    assert_eq!(
        confirm_wrap_note(true, true),
        " · will be refused — this session is read-only"
    );
    assert_eq!(
        confirm_wrap_note(true, false),
        " · will be refused — this session is read-only"
    );
    assert_eq!(
        confirm_wrap_note(false, true),
        " · will wrap in transaction"
    );
    assert_eq!(confirm_wrap_note(false, false), "");
}

// ----- Fitter termination (the read-only refusal hang) -------------------

/// The exact message a guarded `DELETE` produced under the default
/// (`read_only = true`) profile: Postgres's refusal, a `\n`, and the
/// `hint:` line `conn::read_only_refusal_hint` appends. The footer
/// then adds its ` · F2 detail` pointer.
fn read_only_refusal_footer() -> String {
    format!(
        "cannot execute DELETE in a read-only transaction\nhint: this connection is read-only by safety.toml ({}, read_only) — see docs/configuration.md · F2 detail",
        "/home/op/.config/pgman/safety.toml"
    )
}

/// Run `f` on its own thread and give up after two seconds — a fitter
/// that fails to converge used to spin at 100% CPU with the UI dead,
/// and the assertion has to *fail*, not hang the suite with it.
fn bounded<F: FnOnce() -> String + Send + 'static>(f: F) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(std::time::Duration::from_secs(2)).ok()
}

#[test]
fn fit_status_terminates_on_the_read_only_refusal_at_sixty_columns() {
    let text = read_only_refusal_footer();
    let got = bounded(move || fit_status(&text, 60)).expect("fit_status hung");
    assert!(
        display_width(&got) <= 60,
        "{got:?} is {} wide",
        display_width(&got)
    );
    assert!(got.ends_with("· F2 detail"), "pointer lost: {got:?}");
}

#[test]
fn fit_status_terminates_on_the_read_only_refusal_at_one_twenty_columns() {
    let text = read_only_refusal_footer();
    let got = bounded(move || fit_status(&text, 120)).expect("fit_status hung");
    assert!(
        display_width(&got) <= 120,
        "{got:?} is {} wide",
        display_width(&got)
    );
    assert!(got.ends_with("· F2 detail"), "pointer lost: {got:?}");
    // The raw message is wider than 120; the shrink had to happen in
    // the message, not the pointer.
    assert!(got.starts_with("cannot execute DELETE"), "{got:?}");
}

#[test]
fn footer_text_folds_newlines_into_segments_and_tabs_into_spaces() {
    assert_eq!(
        footer_text("cannot execute DELETE\nhint: read-only"),
        "cannot execute DELETE · hint: read-only"
    );
    assert_eq!(footer_text("a\r\nb\rc"), "a · b · c");
    assert_eq!(footer_text("a\tb\u{0}c"), "a b c");
    // Blank lines don't become empty segments; edges don't grow separators.
    assert_eq!(footer_text("\n\na\n\nb\n"), "a · b");
    assert_eq!(footer_text("plain"), "plain");
    assert_eq!(footer_text(""), "");
}

#[test]
fn the_footer_fits_the_folded_refusal_within_every_width() {
    let folded = footer_text(&read_only_refusal_footer());
    assert!(!folded.contains('\n'));
    for width in 0..=folded.chars().count() + 5 {
        let f = folded.clone();
        let got = bounded(move || fit_status(&f, width)).expect("fit_status hung");
        assert!(
            display_width(&got) <= width,
            "width {width}: {got:?} is {} wide",
            display_width(&got)
        );
    }
}

mod fitter_properties {
    use super::*;
    use proptest::prelude::*;

    /// Footer-shaped strings with the awkward cases over-represented:
    /// control characters (the hang), the ` · ` separator, CJK and
    /// emoji (double-width), a combining mark (zero-width).
    fn footer_strings() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                4 => any::<char>(),
                4 => prop::char::range('a', 'z'),
                2 => Just(' '),
                1 => Just('\n'),
                1 => Just('\r'),
                1 => Just('\t'),
                1 => Just('\u{0}'),
                1 => Just('\u{7f}'),
                1 => Just('·'),
                1 => Just('…'),
                1 => Just('漢'),
                1 => Just('😀'),
                1 => Just('\u{301}'),
            ],
            0..96,
        )
        .prop_map(|cs| cs.into_iter().collect())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn fit_status_terminates_and_never_exceeds_the_width(
            text in footer_strings(),
            width in 0usize..160,
        ) {
            let t = text.clone();
            let got = bounded(move || fit_status(&t, width));
            let got = got.expect("fit_status did not return within 2s");
            prop_assert!(
                display_width(&got) <= width,
                "{text:?} @ {width}: {got:?} is {} wide",
                display_width(&got)
            );
        }

        #[test]
        fn fit_hints_terminates_and_never_exceeds_the_width(
            text in footer_strings(),
            width in 0usize..160,
        ) {
            let t = text.clone();
            let got = bounded(move || fit_hints(&t, width));
            let got = got.expect("fit_hints did not return within 2s");
            prop_assert!(
                display_width(&got) <= width,
                "{text:?} @ {width}: {got:?} is {} wide",
                display_width(&got)
            );
        }
    }
}
