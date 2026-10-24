use super::*;

#[test]
fn tab_bar_hint_drops_whole_pieces_from_the_right_to_fit() {
    let full = "[ ] prev/next tab · ctrl-t new · ctrl-w close";
    assert_eq!(tab_bar_hint(80).as_deref(), Some(full));
    assert_eq!(
        tab_bar_hint(display_width(full) + 1).as_deref(),
        Some(full),
        "exactly the hint plus its gap fits"
    );
    assert_eq!(
        tab_bar_hint(display_width(full)).as_deref(),
        Some("[ ] prev/next tab · ctrl-t new"),
        "one column short: the close key goes, whole"
    );
    assert_eq!(tab_bar_hint(18).as_deref(), Some("[ ] prev/next tab"));
    assert_eq!(tab_bar_hint(17), None, "not even the switch keys fit");
}

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
fn build_field_layout_pads_label_and_marks_empty_and_null_apart() {
    let cols = vec![
        "id".to_string(),
        "long_column_name".to_string(),
        "n".to_string(),
        "missing".to_string(),
    ];
    let row = vec![Some("42".to_string()), Some("".to_string()), None];
    let got = build_field_layout(&cols, &row, 16, 40);
    assert_eq!(got.len(), 4);
    // Labels padded to label_width.
    assert_eq!(got[0].label.chars().count(), 16);
    assert!(got[0].label.starts_with("id"));
    assert_eq!(got[0].kind, FieldKind::Value);
    assert_eq!(got[0].values, vec!["42"]);
    // An empty string and a NULL: two different things, said two ways.
    assert_eq!(got[1].kind, FieldKind::Empty);
    assert_eq!(got[1].values, vec!["(empty string)"]);
    assert_eq!(got[2].kind, FieldKind::Null);
    assert_eq!(got[2].values, vec!["NULL"]);
    // A cell the row is too short to have is no value: NULL.
    assert_eq!(got[3].kind, FieldKind::Null);
}

#[test]
fn diff_cell_says_null_and_empty_string_apart() {
    assert_eq!(diff_cell(&None), "NULL");
    assert_eq!(diff_cell(&Some(String::new())), "''");
    assert_eq!(diff_cell(&Some("x".into())), "x");
}

#[test]
fn build_field_layout_truncates_oversized_labels() {
    let cols = vec!["aaaaaaaaaaaaaaaaaaaaaaaaa".to_string()]; // 25 chars
    let row = vec![Some("v".to_string())];
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
    let row = vec![Some("abcdefghij".to_string())];
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
    assert_eq!(fit_hints(hints, hints.chars().count(), "F1"), hints);
    assert_eq!(fit_hints(hints, hints.chars().count() + 10, "F1"), hints);
}

#[test]
fn fit_hints_never_cuts_mid_item() {
    let hints = "q quit · ? help · e editor · S schema · W wizard";
    for width in 0..=hints.chars().count() {
        let fitted = fit_hints(hints, width, "F1");
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
    let fitted = fit_hints(hints, 10, "F1");
    assert_eq!(fitted, "F1 +3 more");
}

#[test]
fn fit_hints_mid_list_cut_drops_whole_items_and_appends_marker() {
    let hints = "q quit · ? help · e editor · S schema · W wizard";
    // Fits "q quit · ? help · e editor" (26 chars) + " · F1 +2 more"
    // (13) = 39.
    let fitted = fit_hints(hints, 39, "F1");
    assert_eq!(fitted, "q quit · ? help · e editor · F1 +2 more");
    assert!(fitted.chars().count() <= 39);
}

#[test]
fn fit_hints_marker_width_is_always_accounted_for() {
    let hints = "aaaaaaaaaa · bbbbbbbbbb · cccccccccc";
    // Any width narrower than the full string must produce output no
    // wider than that width — including the marker itself.
    for width in [0, 1, 5, 8, 9, 10, 15, 20] {
        let fitted = fit_hints(hints, width, "F1");
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
    assert_eq!(fit_hints(hints, 3, "F1"), "");
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

const PROMPT_REST: &str = "  · 63 row(s) · enter accept · esc clear";

/// A pattern that fits leaves the line to `fit_status`, which already
/// keeps the prompt: the same text, and the cursor just after it.
#[test]
fn fit_prompt_matches_fit_status_while_that_keeps_the_prompt() {
    for width in [30, 40, 60, 80] {
        let (text, cursor) = fit_prompt("filter: /", "us", PROMPT_REST, width);
        assert_eq!(
            text,
            fit_status(&format!("filter: /us{PROMPT_REST}"), width),
            "{width}"
        );
        assert_eq!(cursor, Some(11), "{width}");
    }
}

/// The bug: `fit_status` shrank and then dropped the `filter: /`
/// segment, so a long pattern showed as `…` and key hints, with the
/// cursor past the end of the line.
#[test]
fn fit_prompt_keeps_the_head_and_the_patterns_tail() {
    let pattern = "customer_invoice_line_items_archive_2026";
    let (text, cursor) = fit_prompt("filter: /", pattern, PROMPT_REST, 30);
    assert_eq!(text, "filter: /…_items_archive_2026");
    assert_eq!(display_width(&text), 29, "one cell left for the cursor");
    assert_eq!(cursor, Some(29));
    // Wide enough for the pattern, not for the hints: the keys that
    // leave the prompt stay while they fit whole, and go otherwise.
    let (text, cursor) = fit_prompt("filter: /", pattern, PROMPT_REST, 64);
    assert_eq!(
        text,
        "filter: /customer_invoice_line_items_archive_2026  · esc clear"
    );
    assert_eq!(cursor, Some(49));
    let (text, _) = fit_prompt("filter: /", pattern, PROMPT_REST, 55);
    assert_eq!(text, "filter: /customer_invoice_line_items_archive_2026");
}

#[test]
fn fit_prompt_measures_a_wide_tail_by_display_width() {
    let (text, cursor) = fit_prompt("find: ", "東京都の顧客一覧", "  · esc cancel", 14);
    // 14 − head 6 − `…` − the cursor's cell = 6 columns: three glyphs.
    assert_eq!(text, "find: …客一覧");
    assert_eq!(cursor, Some(13));
    assert!(display_width(&text) < 14);
}

/// Too narrow for the head and two cells: the whole line is fitted
/// as any status is, and no cursor is placed in it.
#[test]
fn fit_prompt_falls_back_to_fit_status_when_the_head_cannot_fit() {
    let (text, cursor) = fit_prompt("filter: /", "users", PROMPT_REST, 10);
    assert_eq!(
        text,
        fit_status(&format!("filter: /users{PROMPT_REST}"), 10)
    );
    assert_eq!(cursor, None);
}

#[test]
fn fit_prompt_never_exceeds_width_and_its_cursor_stays_on_the_line() {
    let heads = ["filter: /", "find: ", "(failed reverse-i-search) '"];
    let patterns = ["", "a", "customer_invoice_line_items", "東京都の顧客一覧"];
    for head in heads {
        for pattern in patterns {
            for width in 0..90 {
                let (text, cursor) = fit_prompt(head, pattern, PROMPT_REST, width);
                assert!(
                    display_width(&text) <= width,
                    "{head}{pattern} @{width}: {text}"
                );
                if let Some(c) = cursor {
                    assert!(c < width, "{head}{pattern} @{width}: cursor {c}");
                    assert!(text.starts_with(head), "{head}{pattern} @{width}: {text}");
                } else {
                    assert!(display_width(head) + 2 > width, "{head}{pattern} @{width}");
                }
            }
        }
    }
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
fn compact_count_shortens_large_counts_without_overstating_them() {
    let cases: [(u64, &str); 10] = [
        (0, "0"),
        (9_999, "9999"),
        (10_000, "10.0k"),
        (12_345, "12.3k"),
        (999_999, "999.9k"),
        (1_000_000, "1.0M"),
        (4_567_890_123, "4.5G"),
        (999_999_999_999_999, "999.9T"),
        (1_000_000_000_000_000_000, "1.0e18"),
        (u64::MAX, "1.8e19"),
    ];
    for (n, want) in cases {
        assert_eq!(compact_count(n), want, "{n}");
    }
}

/// A producer's drop counter of 10¹⁸, shown raw, took nineteen columns
/// of the tap monitor's title and pushed its key hints off.
#[test]
fn a_huge_drop_count_leaves_the_tap_monitor_key_hints_visible() {
    let mut a = settled_app(false, false);
    a.tap_health.dropped_events_total = 1_000_000_000_000_000_000;
    let title = |a: &mut App, w: u16| {
        tap_monitor_rows(a, w, 30)
            .into_iter()
            .find(|r| r.contains("heartbeat"))
            .expect("title row")
    };
    let wide = title(&mut a, 150);
    assert!(wide.contains("q close"), "{wide}");
    assert!(wide.contains("1.0e18 dropped"), "{wide}");
    // Narrower, the drop count is what gives way — not the hints.
    let narrow = title(&mut a, 135);
    assert!(
        narrow.contains("q close") && !narrow.contains("dropped"),
        "{narrow}"
    );
}

fn hint_dump(listeners: &[crate::tap::TapListener]) -> String {
    tap_setup_hint_lines(&crate::theme::Theme::default(), listeners)
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn listener(transport: crate::tap::TapTransport, addr: &str) -> crate::tap::TapListener {
    crate::tap::TapListener {
        transport,
        addr: addr.parse().unwrap(),
    }
}

/// With the TCP listener bound, the hint said only "start pgman with
/// --tap-otlp", as if nothing were listening.
#[test]
fn tap_setup_hint_says_which_listeners_are_bound() {
    use crate::tap::TapTransport;
    let dump = hint_dump(&[listener(TapTransport::Tcp, "127.0.0.1:7432")]);
    assert!(
        dump.contains("pgman is listening on TCP 127.0.0.1:7432"),
        "got:\n{dump}"
    );
    // No OTLP listener yet, so restarting with one is still the advice.
    assert!(dump.contains("--tap-otlp :4318"), "got:\n{dump}");
    assert!(
        dump.contains("http://localhost:4318/v1/traces"),
        "got:\n{dump}"
    );
}

/// The endpoint the hint gives the JVM is one the OTLP listener
/// answers: OTEL_EXPORTER_OTLP_TRACES_ENDPOINT is used as-is, and a
/// bare `http://localhost:4318` got the agent's every export a 404.
#[test]
fn tap_setup_hint_endpoint_is_a_path_the_otlp_listener_serves() {
    let dump = hint_dump(&[]);
    let url = dump
        .split_whitespace()
        .find_map(|w| w.strip_prefix("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT="))
        .unwrap_or_else(|| panic!("no endpoint in:\n{dump}"));
    let path = url
        .strip_prefix("http://")
        .and_then(|rest| rest.find('/').map(|i| &rest[i..]))
        .unwrap_or("/");
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let resp = crate::tap::process_otlp_request(
        crate::tap::HttpRequest {
            method: "POST".into(),
            path: path.into(),
            headers: [("content-type".into(), "application/json".into())]
                .into_iter()
                .collect(),
            body: br#"{"resourceSpans":[]}"#.to_vec(),
        },
        &tx,
    );
    assert_eq!(resp.status, 200, "{url} → {} {}", resp.status, resp.body);
}

#[test]
fn tap_setup_hint_points_the_jvm_at_the_bound_otlp_listener() {
    use crate::tap::TapTransport;
    let dump = hint_dump(&[
        listener(TapTransport::Tcp, "127.0.0.1:7432"),
        listener(TapTransport::Otlp, "0.0.0.0:4319"),
    ]);
    assert!(
        dump.contains("listening on TCP 127.0.0.1:7432 · OTLP/HTTP 0.0.0.0:4319"),
        "got:\n{dump}"
    );
    assert!(
        !dump.contains("--tap-otlp"),
        "already listening; got:\n{dump}"
    );
    // A wildcard bind is reached as localhost, on the port it bound.
    assert!(
        dump.contains("http://localhost:4319/v1/traces"),
        "got:\n{dump}"
    );
    let dump = hint_dump(&[listener(TapTransport::Otlp, "127.0.0.1:4320")]);
    assert!(
        dump.contains("http://127.0.0.1:4320/v1/traces"),
        "got:\n{dump}"
    );
}

#[test]
fn tap_setup_hint_includes_otel_and_pgman_tap_routes() {
    let theme = crate::theme::Theme::default();
    let lines = tap_setup_hint_lines(&theme, &[]);
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

/// The TAP badge means "the tap has heard from the app": a commit or
/// rollback is hearing from it, even with no query or heartbeat yet.
#[test]
fn footer_badges_show_tap_for_a_stream_of_only_txn_boundaries() {
    let mut a = settled_app(false, false);
    a.on_tap_event(crate::tap::TapEvent {
        v: 1,
        kind: crate::tap::TapKind::TxnBoundary,
        ts_unix_micros: 1,
        received_at_unix_micros: 1,
        app: None,
        pool: None,
        conn: Some("c-1".into()),
        txn: Some("c-1#1".into()),
        sql: None,
        params: None,
        params_redacted: false,
        duration_micros: None,
        rows: None,
        error: None,
        caller: None,
        dropped_events_total: None,
        txn_outcome: Some(crate::tap::TxnOutcome::Commit),
    });
    let spans = footer_badges_with(&a, &a.theme, 0);
    assert!(
        spans.iter().any(|s| s.content.trim() == "TAP"),
        "expected TAP badge: {spans:?}"
    );
}

/// A query event bound to `params`, for the tap List view's detail.
fn tap_query_with_params(sql: &str, params: &[&str], redacted: bool) -> crate::tap::TapEvent {
    crate::tap::TapEvent {
        v: 1,
        kind: crate::tap::TapKind::Query,
        ts_unix_micros: 1,
        received_at_unix_micros: 1,
        app: Some("svc".into()),
        pool: None,
        conn: Some("c-1".into()),
        txn: None,
        sql: Some(sql.into()),
        params: Some(params.iter().map(|p| p.to_string()).collect()),
        params_redacted: redacted,
        duration_micros: Some(1),
        rows: None,
        error: None,
        caller: None,
        dropped_events_total: None,
        txn_outcome: None,
    }
}

/// The tap monitor drawn at `w`×`h`, one string per row.
fn tap_monitor_rows(a: &mut App, w: u16, h: u16) -> Vec<String> {
    a.splash_visible = false;
    a.mode = crate::app::Mode::TapMonitor;
    a.tap_nav.view = crate::app::TapView::List;
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    term.draw(|f| draw(f, a)).unwrap();
    let buf = term.backend().buffer().clone();
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

/// A result behind an open cursor says it is partial and names `M`;
/// once the cursor has closed, it says so and that `M` re-runs — at
/// 80 columns, whole.
#[test]
fn a_paged_result_title_names_m() {
    let mut a = settled_app(false, false);
    a.splash_visible = false;
    a.splash_until = None;
    a.conn_state = crate::app::ConnState::Connected {
        server_version: "16.0".into(),
    };
    a.mode = crate::app::Mode::Normal;
    a.grid = crate::grid::Grid {
        columns: vec!["id".into()],
        rows: (0..1000).map(|i| vec![Some(i.to_string())]).collect(),
        truncated: true,
    };
    a.grid_view.visible_rows = (0..1000).collect();
    a.result_paging = Some(crate::app::ResultPaging {
        id: 3,
        sql: "SELECT id FROM t".into(),
        ceiling: false,
        in_transaction: false,
        standby: false,
    });
    let now = std::time::Instant::now();
    a.cursor = Some(crate::app::CursorMirror {
        result_id: 3,
        opened: now,
        last_fetch: now,
        limits: crate::query::paging::Limits {
            idle: std::time::Duration::from_secs(60),
            lifetime: std::time::Duration::from_secs(600),
        },
    });
    let screen = |a: &mut App| {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(f, a)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let shown = screen(&mut a);
    assert!(
        shown.contains("result · 1000 row(s) · partial · M fetches more"),
        "{shown}"
    );
    a.cursor = None;
    let shown = screen(&mut a);
    assert!(
        shown.contains("result · 1000 row(s) · partial · cursor closed · M re-runs"),
        "{shown}"
    );
}

/// The tap monitor's title is fitted into its border whole segments at
/// a time; the border must never cut it mid-word. Its budget left out
/// the `[ ◆ … ◆ ]` decoration, so it did, by that much.
#[test]
fn the_tap_monitor_title_is_never_cut_by_its_border() {
    for w in (60..=180).step_by(7) {
        let mut a = settled_app(false, false);
        let rows = tap_monitor_rows(&mut a, w, 30);
        let row = rows
            .iter()
            .find(|r| r.contains("heartbeat"))
            .unwrap_or_else(|| panic!("no title at {w}:\n{}", rows.join("\n")));
        let inside = row.split('╮').next().unwrap();
        let text = inside.trim_end_matches('─').trim_end();
        assert!(
            text.ends_with("q close") || text.ends_with('…'),
            "title cut by the border at width {w}: {row}"
        );
    }
}

#[test]
fn tap_app_cell_is_exactly_the_column_wide_in_display_columns() {
    use crate::ui::tap::tap_app_cell;
    for app in [
        "",
        "svc",
        "billing-service-primary-eu",
        "注文",
        "注文サービス注文サービス",
    ] {
        let cell = tap_app_cell(app);
        assert_eq!(display_width(&cell), 20, "{app:?} → {cell:?}");
    }
    assert_eq!(tap_app_cell("svc").trim_end(), "svc");
    assert!(tap_app_cell("注文サービス注文サービス").contains('…'));
}

/// A CJK app name paints two columns a char; cut by char count it
/// pushed its row's SQL right of every other row's.
#[test]
fn tap_list_keeps_the_sql_column_aligned_past_a_wide_app_name() {
    let mut a = settled_app(false, false);
    let mut narrow = tap_query_with_params("select narrow", &[], false);
    narrow.app = Some("svc".into());
    let mut wide = tap_query_with_params("select wide", &[], false);
    wide.app = Some("注文サービス注文サービス".into());
    a.on_tap_event(narrow);
    a.on_tap_event(wide);
    a.splash_visible = false;
    a.mode = crate::app::Mode::TapMonitor;
    a.tap_nav.view = crate::app::TapView::List;
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
    term.draw(|f| draw(f, &mut a)).unwrap();
    let buf = term.backend().buffer().clone();
    // The buffer column where `needle` starts, on whichever row has it.
    let column_of = |needle: &str| {
        let chars: Vec<String> = needle.chars().map(|c| c.to_string()).collect();
        (0..buf.area.height)
            .find_map(|y| {
                (0..buf.area.width - chars.len() as u16).find(|&x| {
                    chars
                        .iter()
                        .enumerate()
                        .all(|(i, c)| buf[(x + i as u16, y)].symbol() == c)
                })
            })
            .unwrap_or_else(|| panic!("{needle:?} not drawn"))
    };
    assert_eq!(column_of("select narrow"), column_of("select wide"));
}

/// The List view shows the focused query's bound parameters — before,
/// they were only stored and written to `--tap-record`.
#[test]
fn tap_list_shows_the_focused_events_bound_parameters() {
    let mut a = settled_app(false, false);
    a.on_tap_event(tap_query_with_params(
        "SELECT * FROM accounts WHERE id = ?",
        &["2107"],
        false,
    ));
    let rows = tap_monitor_rows(&mut a, 120, 40);
    let row = rows
        .iter()
        .find(|r| r.contains("params"))
        .unwrap_or_else(|| panic!("no params line:\n{}", rows.join("\n")));
    assert!(row.contains("params  1: 2107"), "{row}");
    assert!(!row.contains("redacted"), "{row}");
}

/// A masked value carries a visible marker, in the warning colour, so the
/// operator reads `[redacted]` as masked and not as what was bound.
#[test]
fn tap_list_marks_parameters_the_producer_redacted() {
    let mut a = settled_app(false, false);
    a.on_tap_event(tap_query_with_params(
        "INSERT INTO invoice (account_id, amount, note) VALUES (?, ?, ?)",
        &["2", "11", "[redacted]"],
        true,
    ));
    a.splash_visible = false;
    a.mode = crate::app::Mode::TapMonitor;
    a.tap_nav.view = crate::app::TapView::List;
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    term.draw(|f| draw(f, &mut a)).unwrap();
    let buf = term.backend().buffer().clone();
    let (y, row) = (0..buf.area.height)
        .map(|y| {
            let row: String = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            (y, row)
        })
        .find(|(_, r)| r.contains("params"))
        .expect("a params line");
    assert!(
        row.contains("params (some redacted)  1: 2 · 2: 11 · 3: [redacted]"),
        "{row}"
    );
    let x = row.find("params (some").unwrap();
    let x = row[..x].chars().count() as u16;
    assert_eq!(buf[(x, y)].fg, a.theme.health_yellow, "{row}");
}

/// The detail follows the cursor; and a panel too short for it keeps its
/// list rows instead.
#[test]
fn tap_list_params_detail_follows_the_cursor_and_yields_to_a_short_panel() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut a = settled_app(false, false);
    a.on_tap_event(tap_query_with_params(
        "SELECT 1 WHERE a = ?",
        &["first"],
        false,
    ));
    a.on_tap_event(tap_query_with_params(
        "SELECT 2 WHERE b = ?",
        &["second"],
        false,
    ));
    a.mode = crate::app::Mode::TapMonitor;
    a.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    assert_eq!(a.tap_nav.events_cursor, 1);
    let dump = tap_monitor_rows(&mut a, 120, 40).join("\n");
    assert!(
        dump.contains("1: second") && !dump.contains("1: first"),
        "{dump}"
    );
    let dump = tap_monitor_rows(&mut a, 120, 12).join("\n");
    assert!(!dump.contains("params"), "{dump}");
    assert!(dump.contains("SELECT 2 WHERE b"), "{dump}");
}

/// A parameter list longer than the row carries on to a second line,
/// which shows the cut with `…` rather than stopping mid-value.
#[test]
fn tap_list_carries_a_long_parameter_list_onto_a_marked_second_line() {
    let mut a = settled_app(false, false);
    let values: Vec<String> = (1..=40).map(|i| format!("value-{i}")).collect();
    let refs: Vec<&str> = values.iter().map(String::as_str).collect();
    a.on_tap_event(tap_query_with_params(
        "SELECT * FROM t WHERE a IN (?)",
        &refs,
        false,
    ));
    let rows = tap_monitor_rows(&mut a, 120, 40);
    let at = rows
        .iter()
        .position(|r| r.contains("params  1: value-1 ·"))
        .unwrap_or_else(|| panic!("{}", rows.join("\n")));
    let next = &rows[at + 1];
    assert!(next.contains("value-") && next.contains('…'), "{next}");
}

/// Crossing a COMMIT row keeps the detail's rows (the list doesn't
/// change height under the cursor) and says a boundary has no params.
#[test]
fn tap_list_params_detail_keeps_its_rows_on_a_commit() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut a = settled_app(false, false);
    a.on_tap_event(tap_query_with_params(
        "SELECT 1 WHERE a = ?",
        &["first"],
        false,
    ));
    let mut commit = tap_query_with_params("", &[], false);
    commit.kind = crate::tap::TapKind::TxnBoundary;
    commit.sql = None;
    commit.params = None;
    commit.txn = Some("c-1#1".into());
    commit.txn_outcome = Some(crate::tap::TxnOutcome::Commit);
    a.on_tap_event(commit);
    let on_query = tap_monitor_rows(&mut a, 120, 40);
    a.mode = crate::app::Mode::TapMonitor;
    a.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    let on_commit = tap_monitor_rows(&mut a, 120, 40);
    // The detail's rule starts at the panel's left border; the popup's
    // own top and bottom borders start at a corner.
    let rule = "│── enter: the whole event ──".to_string();
    let rule_at = |rows: &[String]| rows.iter().position(|r| r.contains(&rule));
    assert!(rule_at(&on_query).is_some(), "{}", on_query.join("\n"));
    assert_eq!(rule_at(&on_query), rule_at(&on_commit));
    let dump = on_commit.join("\n");
    assert!(
        dump.contains("no parameters on a commit / rollback"),
        "{dump}"
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
    let got = fit_hints(&hints, 20, "F1");
    assert!(
        display_width(&got) <= 20,
        "fitted hints are {} columns wide: {got:?}",
        display_width(&got)
    );
}

#[test]
fn error_footer_rows_wrap_and_keep_the_pointer_last() {
    const P: &str = " · ! detail";
    // Fits: one row, as before.
    assert_eq!(
        error_footer_rows("relation \"x\" does not exist", P, 60, 3),
        vec!["relation \"x\" does not exist · ! detail"]
    );
    // Too long for one row: wrapped on words, the pointer on the last.
    let msg = "duplicate key value violates unique constraint \"users_email_key\"";
    let rows = error_footer_rows(msg, P, 40, 3);
    assert_eq!(
        rows,
        vec![
            "duplicate key value violates unique",
            "constraint \"users_email_key\" · ! detail"
        ]
    );
    // Nothing lost when it fits in the rows it has.
    assert_eq!(
        rows.concat().replace(P, ""),
        msg.replace("unique constraint", "uniqueconstraint")
    );
    // More than `max_rows` worth: cut on the last row, the pointer kept.
    let long = "word ".repeat(60);
    let rows = error_footer_rows(long.trim(), P, 30, 3);
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert!(rows[2].ends_with(P) && rows[2].contains('…'), "{rows:?}");
    assert!(rows.iter().all(|r| display_width(r) <= 30), "{rows:?}");
    // A last row too full for the pointer, with a row to spare: the
    // pointer takes that row rather than cutting the message.
    let rows = error_footer_rows("aaaa bbbb cccc dddd", P, 10, 3);
    assert_eq!(rows, vec!["aaaa bbbb", "cccc dddd", "! detail"]);
    // One row allowed: the old single-line fit.
    let rows = error_footer_rows(msg, P, 40, 1);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].ends_with(P) && display_width(&rows[0]) <= 40);
    // No detail to point at: no pointer anywhere.
    let rows = error_footer_rows(msg, "", 40, 3);
    assert_eq!(
        rows.concat(),
        msg.replace("unique constraint", "uniqueconstraint")
    );
}

#[test]
fn fit_error_row_cuts_the_end_of_the_message_and_keeps_the_pointer() {
    const P: &str = " · F2 detail";
    let msg =
        "function nonexistent_function_with_long_name(integer, integer, integer) does not exist";
    // Fits: unchanged.
    assert_eq!(fit_error_row("boom", P, 40), "boom · F2 detail");
    // Too wide: the start stays, the end goes, the pointer stays.
    let got = fit_error_row(msg, P, 40);
    assert_eq!(got, "function nonexistent_functi… · F2 detail");
    assert_eq!(display_width(&got), 40);
    // Too narrow for pointer and message both: the message wins.
    assert_eq!(fit_error_row(msg, P, 12), "function no…");
    // No pointer: a plain end cut.
    assert_eq!(fit_error_row(msg, "", 10), "function …");
    assert_eq!(fit_error_row(msg, P, 0), "");
    // Never wider than asked, at every width.
    for w in 0..120 {
        assert!(display_width(&fit_error_row(msg, P, w)) <= w, "{w}");
    }
}

#[test]
fn wrap_path_breaks_after_slashes_and_loses_nothing() {
    let p = "/Users/someone/.cache/pgman/pgman.log.2026-10-21";
    assert_eq!(wrap_path(p, 80), vec![p]);
    let rows = wrap_path(p, 20);
    assert_eq!(
        rows,
        vec!["/Users/someone/", ".cache/pgman/", "pgman.log.2026-10-21"]
    );
    assert_eq!(rows.concat(), p);
    // A name wider than a row is split inside it, still within width.
    let rows = wrap_path(p, 8);
    assert_eq!(rows.concat(), p);
    assert!(rows.iter().all(|r| display_width(r) <= 8), "{rows:?}");
    // Double-width names never overrun a row either.
    let rows = wrap_path("/データ/東京都大阪府/x", 5);
    assert_eq!(rows.concat(), "/データ/東京都大阪府/x");
    assert!(rows.iter().all(|r| display_width(r) <= 5), "{rows:?}");
    // Width 0 hands the path back whole rather than looping.
    assert_eq!(wrap_path(p, 0), vec![p]);
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
fn fields_out_of_view_counts_fields_not_wrapped_lines() {
    // Five fields; the third wraps over 10 lines.
    let counts = [1, 1, 10, 1, 1];
    // Lines 0..4 seen: fields 0..=2 start in view, two below.
    assert_eq!(fields_out_of_view(&counts, 0..4), (0, 2));
    // Scrolled into the long field: 0, 1 and 2 start above, 3 and 4 below.
    assert_eq!(fields_out_of_view(&counts, 4..8), (3, 2));
    // At the bottom: everything but the last two started above.
    assert_eq!(fields_out_of_view(&counts, 10..14), (3, 0));
    assert_eq!(fields_out_of_view(&[], 0..4), (0, 0));
    // A field starting on the first line past what is seen is out of view.
    assert_eq!(fields_out_of_view(&[2, 2, 2], 0..2), (0, 2));
}

#[test]
fn more_fields_label_pluralises_and_handles_a_continuing_field() {
    assert_eq!(more_fields_label("↓", 1, "below"), "↓ 1 more field below");
    assert_eq!(more_fields_label("↑", 3, "above"), "↑ 3 more fields above");
    assert_eq!(more_fields_label("↓", 0, "below"), "↓ more below");
}

#[test]
fn is_key_hint_tells_keys_from_prose() {
    for hint in [
        "y run",
        "? full keys",
        "n / esc cancel",
        "+ / − expand-all/collapse-all",
        "/ filter",
        "[ ] jump schema",
        "J/K next/prev row",
        "ctrl-t new",
        "enter loads selected",
        "F2 detail",
        "esc / q close",
    ] {
        assert!(is_key_hint(hint), "{hint}");
    }
    for prose in [
        "tip",
        "find: pro",
        "loaded query",
        "3 matches",
        "1/3 match",
        "run (DELETE without WHERE)",
        "connecting…",
        "in ~/Library/Caches/pgman",
        "no rows",
        "",
    ] {
        assert!(!is_key_hint(prose), "{prose}");
    }
}

/// `:fixture`'s status on a partial result: the path's `in <dir>` is
/// prose to shrink, not a key hint to keep whole, so the file name and
/// the row count survive alongside the note at the end.
#[test]
fn fit_status_shrinks_a_long_directory_before_the_file_name() {
    let s = "wrote 1000 row(s) to public_invoices-fixture-20261024-101112.xml · in \
             /var/folders/xy/abcdefghijklmnopqrstuvwxyz0000gn/T/pgman/a/directory/deep/enough \
             · fetched rows only, more not fetched";
    for width in [110usize, 130, 150] {
        let got = fit_status(s, width);
        assert!(display_width(&got) <= width, "{width}: {got}");
        assert!(
            got.starts_with("wrote 1000 row(s) to public_invoices-fixture-20261024-101112.xml · "),
            "{width}: {got}"
        );
        assert!(
            got.ends_with(" · fetched rows only, more not fetched"),
            "{width}: {got}"
        );
    }
}

#[test]
fn fit_status_drops_whole_key_hints_from_the_end_never_a_key_alone() {
    let tip = "tip · / filter · [ ] jump schema · + / − expand-all/collapse-all · ? full keys";
    let fitted = fit_status(tip, display_width(tip) - 1);
    assert_eq!(fitted, "tip · / filter · [ ] jump schema · ? full keys");
    // Tighter: the next one from the end goes, whole.
    let fitted = fit_status(tip, 40);
    assert_eq!(fitted, "tip · / filter · ? full keys");
    for w in 10..display_width(tip) {
        let fitted = fit_status(tip, w);
        assert!(display_width(&fitted) <= w, "{w}: {fitted}");
        assert!(!fitted.contains("… expand"), "{w}: {fitted}");
        assert!(!fitted.contains("…ollapse"), "{w}: {fitted}");
    }
}

#[test]
fn fit_status_drops_a_middle_segment_it_would_shrink_to_a_stub() {
    let tip =
        "tip · from environments.toml · replicas under their primary · r refresh · ? full keys";
    let fitted = fit_status(tip, 60);
    assert_eq!(
        fitted,
        "tip · from environments.toml · r refresh · ? full keys"
    );
    for w in 10..display_width(tip) {
        let fitted = fit_status(tip, w);
        assert!(display_width(&fitted) <= w, "{w}: {fitted}");
        assert!(!fitted.contains(" r… "), "{w}: {fitted}");
    }
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
        " · Postgres will refuse it — this session is read-only"
    );
    assert_eq!(
        confirm_wrap_note(true, false),
        " · Postgres will refuse it — this session is read-only"
    );
    assert_eq!(
        confirm_wrap_note(false, true),
        " · will wrap in transaction"
    );
    assert_eq!(confirm_wrap_note(false, false), "");
    use super::panels::confirm_action_line;
    assert_eq!(
        confirm_action_line(true),
        "y = send it anyway · n / esc = cancel"
    );
    assert_eq!(confirm_action_line(false), "y = run · n / esc = cancel");
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
            let got = bounded(move || fit_hints(&t, width, "F1"));
            let got = got.expect("fit_hints did not return within 2s");
            prop_assert!(
                display_width(&got) <= width,
                "{text:?} @ {width}: {got:?} is {} wide",
                display_width(&got)
            );
        }
    }
}

// -- editor_rows: the editor's share of the body --

#[test]
fn editor_rows_opens_five_lines_tall_on_a_body_of_twenty_or_more() {
    // 80×24 leaves a 22-row body: five content lines plus two borders.
    assert_eq!(editor_rows(1, 22), 7);
    assert_eq!(editor_rows(1, 20), 7, "the boundary is inclusive");
    assert_eq!(
        editor_rows(5, 20),
        7,
        "five lines fit the opening size exactly"
    );
    assert_eq!(
        editor_rows(1, 48),
        7,
        "a tall body still opens at five lines"
    );
}

#[test]
fn editor_rows_grows_with_content_up_to_forty_percent_of_the_body() {
    assert_eq!(
        editor_rows(6, 20),
        8,
        "six lines: one past the opening size"
    );
    assert_eq!(editor_rows(7, 20), 8, "40 % of 20 is 8 rows — the cap");
    assert_eq!(
        editor_rows(100, 22),
        8,
        "80×24: 40 % of 22 rounds down to 8"
    );
    assert_eq!(editor_rows(100, 48), 19, "200×50: 40 % of 48 is 19");
}

#[test]
fn editor_rows_opens_at_one_line_below_twenty_but_still_grows() {
    assert_eq!(
        editor_rows(1, 19),
        3,
        "under 20 the opening size drops to one line"
    );
    assert_eq!(editor_rows(4, 19), 6);
    assert_eq!(editor_rows(100, 19), 7, "40 % of 19 rounds down to 7");
    // 60×16 leaves a 14-row body.
    assert_eq!(editor_rows(1, 14), 3);
    assert_eq!(
        editor_rows(4, 14),
        5,
        "40 % of 14 is 5: three content lines"
    );
    assert_eq!(
        editor_rows(1, 12),
        3,
        "12 is the first body that grows at all"
    );
    assert_eq!(editor_rows(10, 12), 4, "40 % of 12 is 4");
}

#[test]
fn editor_rows_is_one_line_on_a_cramped_body_whatever_the_buffer_holds() {
    assert_eq!(editor_rows(1, 11), 3);
    assert_eq!(editor_rows(50, 11), 3, "no growth under 12 rows");
    assert_eq!(editor_rows(1, 3), 3);
}

#[test]
fn editor_rows_never_exceeds_the_body() {
    assert_eq!(
        editor_rows(1, 2),
        2,
        "a two-row body cannot hold three rows"
    );
    assert_eq!(editor_rows(1, 0), 0);
    assert_eq!(editor_rows(usize::MAX, 22), 8, "a huge buffer still clamps");
    assert_eq!(
        editor_rows(1, u16::MAX),
        7,
        "the 40 % arithmetic must not overflow"
    );
    assert_eq!(editor_rows(usize::MAX, u16::MAX), 26214);
}

// -- pane_split: zoom and manual size over the automatic split --

#[test]
fn pane_split_without_zoom_or_size_is_the_automatic_split() {
    assert_eq!(pane_split(1, None, None, 22), (7, 15));
    assert_eq!(pane_split(100, None, None, 22), (8, 14));
    assert_eq!(pane_split(1, None, None, 14), (3, 11));
}

#[test]
fn pane_split_zoom_gives_the_whole_body_to_one_pane() {
    assert_eq!(pane_split(1, None, Some(ZoomPane::Editor), 22), (22, 0));
    assert_eq!(pane_split(1, None, Some(ZoomPane::Results), 22), (0, 22));
    assert_eq!(
        pane_split(1, Some(9), Some(ZoomPane::Results), 22),
        (0, 22),
        "zoom wins over a manual size"
    );
    assert_eq!(pane_split(1, None, Some(ZoomPane::Editor), 0), (0, 0));
}

#[test]
fn pane_split_manual_size_adds_borders_and_clamps_to_the_body() {
    assert_eq!(pane_split(1, Some(9), None, 22), (11, 11));
    assert_eq!(pane_split(1, Some(1), None, 22), (3, 19));
    assert_eq!(
        pane_split(1, Some(40), None, 22),
        (22, 0),
        "a size from a taller terminal is cut to this one"
    );
    assert_eq!(
        pane_split(1, Some(1), None, 2),
        (2, 0),
        "a two-row body cannot hold three rows"
    );
    assert_eq!(pane_split(1, Some(u16::MAX), None, 22), (22, 0));
}

#[test]
fn pane_split_always_sums_to_the_body() {
    for body in [0u16, 1, 2, 3, 11, 12, 19, 20, 22, 48] {
        for manual in [None, Some(1), Some(5), Some(60)] {
            for zoom in [None, Some(ZoomPane::Editor), Some(ZoomPane::Results)] {
                let (e, r) = pane_split(3, manual, zoom, body);
                assert_eq!(e + r, body, "body {body} manual {manual:?} zoom {zoom:?}");
            }
        }
    }
}

// -- tab_label_text: what the tab bar calls a tab --

#[test]
fn tab_label_text_is_the_first_non_blank_line_trimmed() {
    assert_eq!(tab_label_text("SELECT 1"), "SELECT 1");
    assert_eq!(
        tab_label_text("\n   \n  select id\n  from users"),
        "select id",
        "leading blank lines and indentation are skipped"
    );
}

#[test]
fn tab_label_text_says_empty_for_a_blank_buffer() {
    assert_eq!(tab_label_text(""), "empty");
    assert_eq!(tab_label_text("  \n\t\n"), "empty");
}

#[test]
fn tab_bar_cells_cut_labels_at_eighteen_columns_when_there_is_room() {
    let labels = vec![
        "SELECT id, email, plan FROM users WHERE plan = 'pro'".to_string(),
        "SELECT * FROM user".to_string(),
        "empty".to_string(),
    ];
    let cells = tab_bar_cells(&labels, 80);
    assert_eq!(cells[0], " 1 SELECT id, email,… ");
    assert_eq!(display_width(&cells[0]), 4 + 18);
    assert_eq!(
        cells[1], " 2 SELECT * FROM user ",
        "exactly eighteen columns is not cut"
    );
    assert_eq!(cells[2], " 3 empty ");
    // Counts display columns, not chars: nine CJK glyphs are eighteen
    // columns and must not be cut to nine glyphs plus an ellipsis.
    let cjk = vec![
        "顧客マスタ統合基盤".to_string(),
        "顧客マスタ統合基盤の一覧".to_string(),
    ];
    let cells = tab_bar_cells(&cjk, 80);
    assert_eq!(cells[0], " 1 顧客マスタ統合基盤 ");
    assert!(display_width(&cells[1]) <= 4 + 18, "{:?}", cells[1]);
    assert!(cells[1].ends_with("… "), "{:?}", cells[1]);
}

#[test]
fn tab_bar_cells_shrink_labels_until_every_tab_fits() {
    let labels: Vec<String> = (0..5)
        .map(|i| format!("select {i} from a_long_table_name"))
        .collect();
    // 80 columns: five full cells (23 each) need 115 — the 10-column
    // budget (15 each, 75) fits.
    let cells = tab_bar_cells(&labels, 80);
    assert_eq!(cells[0], " 1 select 0 … ");
    assert_eq!(cells.len(), 5);
    // 60 columns: 75 does not fit; the 6-column budget (11 each, 55) does.
    let cells = tab_bar_cells(&labels, 60);
    assert_eq!(cells[0], " 1 selec… ");
    // 40 columns: even 55 is too much — numbers only (4 each, 20).
    let cells = tab_bar_cells(&labels, 40);
    assert_eq!(cells, vec![" 1 ", " 2 ", " 3 ", " 4 ", " 5 "]);
    // Narrower than the numbers themselves: they are returned anyway
    // (the row clips) rather than nothing.
    assert_eq!(tab_bar_cells(&labels, 10).len(), 5);
    assert!(tab_bar_cells(&[], 10).is_empty());
}

#[test]
fn the_header_names_the_build_and_gives_way_before_the_connection_does() {
    let version = format!(" {}", env!("CARGO_PKG_VERSION"));
    let full = if crate::RELEASE_DATE.is_empty() {
        version.clone()
    } else {
        format!("{version} · released {}", crate::RELEASE_DATE)
    };
    // " pgman" + build + " " + rest.
    let need = |s: &str| display_width(" pgman") + display_width(s) + 1;
    assert_eq!(header_build(200, 40), full);
    assert_eq!(
        header_build(need(&full) + 40, 40),
        full,
        "exactly enough room"
    );
    assert_eq!(
        header_build(need(&version) + 40, 40),
        version,
        "date goes first"
    );
    assert_eq!(
        header_build(need(&version) + 39, 40),
        "",
        "then the version"
    );
    assert_eq!(header_build(10, 40), "", "never at the rest's expense");
}

#[test]
fn hint_keys_are_the_first_word_and_their_alternatives() {
    assert_eq!(hint_key("j/k navigate"), Some(("j/k", " navigate")));
    assert_eq!(hint_key("esc / q close"), Some(("esc / q", " close")));
    assert_eq!(hint_key("q / esc close"), Some(("q / esc", " close")));
    assert_eq!(hint_key("ctrl-t new"), Some(("ctrl-t", " new")));
    assert_eq!(hint_key("F1 +3 more"), Some(("F1", " +3 more")));
    assert_eq!(hint_key("connecting…"), None);
    assert_eq!(hint_key("esc / ?"), None, "alternatives with no action");
}

#[test]
fn the_key_strip_colours_keys_and_keeps_its_text() {
    let theme = Theme::default();
    let fitted = "j/k navigate · esc / q close · connecting…";
    let spans = hint_spans(fitted, &theme);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, fitted, "same text, so the fitted width holds");
    let key = |s: &str| {
        spans
            .iter()
            .find(|sp| sp.content == s)
            .map(|sp| (sp.style.fg, sp.style.add_modifier.contains(Modifier::BOLD)))
    };
    assert_eq!(key("j/k"), Some((Some(theme.accent), true)));
    assert_eq!(key("esc / q"), Some((Some(theme.accent), true)));
    assert_eq!(key(" navigate"), Some((Some(theme.muted), false)));
    assert_eq!(key("connecting…"), Some((Some(theme.muted), false)));
}

/// The universal help pointer names `?` where `?` opens help, and F1
/// only where `?` is a character being typed.
#[test]
fn the_help_pointer_names_the_key_that_works_in_this_mode() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = App::new(
        Theme::default(),
        None,
        Vec::new(),
        crate::safety::SafetyConfig::default(),
    );
    app.environments_loader = Some(crate::app::EnvironmentsLoader::Immediate(
        crate::demo::environments(),
    ));
    app.on_key(KeyEvent::new(KeyCode::Char('E'), KeyModifiers::NONE));
    let footer = |app: &mut App| {
        let backend = ratatui::backend::TestBackend::new(140, 24);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.width)
            .map(|x| buf[(x, buf.area.height - 1)].symbol().to_string())
            .collect::<String>()
    };
    let overview = footer(&mut app);
    assert!(
        // The overview opens on its tip, which points at help too.
        overview.contains("? full keys") && !overview.contains("F1"),
        "{overview}"
    );
    assert!(!app.mode.is_text_input());
}

/// With environments listed, the grid's hints say Esc goes back to them.
#[test]
fn the_grid_hints_say_esc_goes_back_to_the_environments() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = App::new(
        Theme::default(),
        None,
        Vec::new(),
        crate::safety::SafetyConfig::default(),
    );
    let footer = |app: &mut App| {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(220, 24)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.width)
            .map(|x| buf[(x, buf.area.height - 1)].symbol().to_string())
            .collect::<String>()
    };
    app.last_status = None;
    assert!(
        !footer(&mut app).contains("esc / E envs"),
        "no environments, no claim"
    );
    app.environments_loader = Some(crate::app::EnvironmentsLoader::Immediate(
        crate::demo::environments(),
    ));
    app.on_key(KeyEvent::new(KeyCode::Char('E'), KeyModifiers::NONE));
    app.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
    app.last_status = None;
    assert_eq!(app.mode, crate::app::Mode::Normal);
    let f = footer(&mut app);
    assert!(f.contains("esc / E envs"), "{f}");
}

/// The error pointer names the key that works where the operator is:
/// `!` outside a text input, F2 in the editor where `!` is typed.
#[test]
fn the_error_pointer_names_the_key_that_works_here() {
    let mut app = App::new(
        Theme::default(),
        None,
        Vec::new(),
        crate::safety::SafetyConfig::default(),
    );
    let footer = |app: &mut App| {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 24)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.width)
            .map(|x| buf[(x, buf.area.height - 1)].symbol().to_string())
            .collect::<String>()
    };
    app.splash_visible = false;
    app.last_error = Some("duplicate key".into());
    app.last_error_detail = Some(crate::conn::QueryErrDetail::default());
    app.mode = crate::app::Mode::Normal;
    let f = footer(&mut app);
    assert!(f.contains("· ! detail"), "{f}");
    app.mode = crate::app::Mode::Editor;
    let f = footer(&mut app);
    assert!(
        f.contains("· F2 / :error detail") && !f.contains("· ! detail"),
        "{f}"
    );
}

#[test]
fn a_running_watch_wears_a_badge() {
    let mut a = App::new(
        Theme::default(),
        None,
        Vec::new(),
        crate::safety::SafetyConfig::default(),
    );
    let text = |a: &App| {
        footer_badges_with(a, &a.theme, 0)
            .iter()
            .map(|s| s.content.to_string())
            .collect::<String>()
    };
    assert!(!text(&a).contains("WATCH"));
    a.watch = Some(crate::app::WatchState {
        sql: "select 1".into(),
        interval: std::time::Duration::from_secs(2),
        last_fire: std::time::Instant::now(),
    });
    assert!(text(&a).contains(" WATCH 2s "), "{}", text(&a));
}

/// The confirm modal's footer is the keys that answer it, whatever
/// status the editor left behind.
#[test]
fn the_confirm_modal_footer_keeps_its_y_n_keys() {
    let mut app = App::new(
        Theme::default(),
        None,
        Vec::new(),
        crate::safety::SafetyConfig::default(),
    );
    app.splash_visible = false;
    app.pending_run = Some(crate::app::PendingRun {
        sql: "delete from t".into(),
        kind: crate::app::RunKind::Run,
        decision: crate::safety::Decision {
            kind: crate::safety::StatementKind::Delete { has_where: false },
            guard: crate::safety::Guard::Confirm,
            wrap_in_tx: true,
            blocked_by_read_only: false,
            read_only_escape: false,
            server_control: false,
        },
        is_batch: false,
        summary: Some("delete without a where".into()),
        cell_edit: None,
    });
    app.mode = crate::app::Mode::Confirm;
    app.last_status = Some("completed `users`".into());
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 24)).unwrap();
    term.draw(|f| draw(f, &mut app)).unwrap();
    let buf = term.backend().buffer().clone();
    let footer: String = (0..buf.area.width)
        .map(|x| buf[(x, buf.area.height - 1)].symbol().to_string())
        .collect();
    assert!(footer.contains("y run · n / esc cancel"), "{footer}");
}

/// A typo'd statement's confirm card says it's unrecognised — not that a
/// read-only session will refuse a write — and still asks before sending.
#[test]
fn the_confirm_card_calls_a_typo_unrecognised_not_a_refused_write() {
    let mut app = App::new(
        Theme::default(),
        None,
        Vec::new(),
        crate::safety::SafetyConfig::default(),
    );
    app.splash_visible = false;
    app.read_only = true;
    let decision =
        crate::safety::evaluate(&crate::safety::SafetyConfig::default(), "db", "selec 1;");
    assert_eq!(decision.guard, crate::safety::Guard::Confirm);
    app.pending_run = Some(crate::app::PendingRun {
        sql: "selec 1;".into(),
        kind: crate::app::RunKind::Run,
        decision,
        is_batch: false,
        summary: None,
        cell_edit: None,
    });
    app.mode = crate::app::Mode::Confirm;
    for (w, h) in [(80, 24), (120, 40)] {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        let cell = |x: u16, y: u16| buf[(x, y)].symbol().to_string();
        let row = |y: u16| (0..buf.area.width).map(|x| cell(x, y)).collect::<Vec<_>>();
        // The confirm box: its top border carries the title.
        let y0 = (0..buf.area.height)
            .find(|&y| row(y).concat().contains("[ ◆ confirm"))
            .expect("confirm box drawn");
        let top = row(y0);
        let title_at = (0..top.len())
            .find(|&x| top[x..].concat().starts_with("[ ◆ confirm"))
            .unwrap();
        let x0 = (0..title_at).rev().find(|&x| top[x] == "╭").unwrap();
        let x1 = (title_at..top.len()).find(|&x| top[x] == "╮").unwrap();
        let mut inner = String::new();
        for y in y0 + 1..buf.area.height {
            if row(y)[x0] == "╰" {
                break;
            }
            inner.push_str(&row(y)[x0 + 1..x1].concat());
            inner.push(' ');
        }
        let squashed = inner.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            squashed.contains("`selec` isn't an SQL statement pgman recognises"),
            "{w}x{h}: {squashed}"
        );
        assert!(
            squashed.contains("likely be a syntax error"),
            "{w}x{h}: {squashed}"
        );
        assert!(
            !squashed.contains("Postgres will refuse it"),
            "{w}x{h}: {squashed}"
        );
        assert!(
            squashed.contains("y = send it anyway"),
            "{w}x{h}: {squashed}"
        );
    }
}

/// A write refused by a read-only connection at 80x24: the footer
/// names `safety.toml` in at most two rows and never its path — a full
/// path made it three and was cut anyway — and the F2 overlay carries
/// the path whole.
#[test]
fn a_read_only_refusal_names_the_file_in_the_footer_and_its_path_in_f2() {
    let path = std::path::Path::new("/home/op/.config/pgman/safety.toml");
    let (msg, detail) = crate::conn::explain_read_only_refusal(
        "cannot execute DELETE in a read-only transaction".into(),
        Some(crate::conn::QueryErrDetail {
            code: Some("25006".into()),
            severity: Some("ERROR".into()),
            ..Default::default()
        }),
        &crate::conn::ReadOnlyCause::Profile {
            safety_toml: Some(path.to_path_buf()),
        },
    );
    let mut app = App::new(
        Theme::default(),
        None,
        Vec::new(),
        crate::safety::SafetyConfig::default(),
    );
    app.splash_visible = false;
    app.last_error = Some(msg);
    app.last_error_detail = detail;
    app.mode = crate::app::Mode::Editor;
    let screen = |app: &mut App| {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
    };
    let rows = screen(&mut app);
    let footer: Vec<&String> = rows.iter().skip_while(|r| !r.contains('⚠')).collect();
    // The rendered footer's height also depends on the tap's
    // process-wide drop counter (a `DROP ×n` badge other tests can
    // raise), so the row budget is checked with the badges pinned —
    // the same `error_footer_rows` `draw` sizes the footer from.
    let badges: usize = footer_badges_with(&app, &app.theme, 0)
        .iter()
        .map(|s| display_width(&s.content))
        .sum();
    let budgeted = error_footer_rows(
        &footer_text(app.last_error.as_deref().unwrap()),
        " · F2 / :error detail",
        80 - badges - display_width(ERROR_ICON),
        ERROR_FOOTER_MAX_ROWS as usize,
    );
    assert!(
        budgeted.len() <= 2,
        "footer is {} rows: {budgeted:?}",
        budgeted.len()
    );
    assert!(!footer.is_empty(), "no error footer:\n{}", rows.join("\n"));
    let footer = footer
        .iter()
        .map(|r| r.trim_end())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(footer.contains("safety.toml"), "{footer}");
    assert!(footer.contains("F2 / :error detail"), "{footer}");
    assert!(!footer.contains("/home/op"), "path in the footer: {footer}");
    assert!(!footer.contains('…'), "the footer was cut: {footer}");

    app.mode = crate::app::Mode::ErrorDetail;
    let rows = screen(&mut app);
    // The overlay wraps a long value mid-word: read it with the wraps
    // (and the panes' borders around it) taken out.
    let joined: String = rows
        .concat()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '│')
        .collect();
    assert!(
        joined.contains("read_only=truein/home/op/.config/pgman/safety.toml"),
        "F2 lost the path:\n{}",
        rows.join("\n")
    );
}

/// The log picker's failed-pick line fits its border and is cut at the
/// end, never in the middle with a stray word after the ellipsis.
#[test]
fn failed_pick_title_fits_and_cuts_at_the_end() {
    use super::panels::failed_pick_title;
    let err = "relation \"invoice_lnes\" does not exist at character 15";
    assert_eq!(
        failed_pick_title(err, 200),
        format!(" failed in the log: {err} ")
    );
    let cut = failed_pick_title(err, 60);
    assert!(display_width(&cut) <= 60, "{cut:?}");
    assert!(cut.starts_with(" failed in the log: relation"), "{cut:?}");
    assert!(cut.ends_with("… "), "{cut:?}");
    assert!(
        !cut.contains("15"),
        "a middle cut kept the last word: {cut:?}"
    );
    // A multi-line message is one line on a border.
    assert_eq!(failed_pick_title("a\nb", 200), " failed in the log: a · b ");
}

/// An error with no server detail says so in words, not type names.
#[test]
fn the_error_overlay_without_detail_speaks_plainly() {
    let mut app = App::new(
        Theme::default(),
        None,
        Vec::new(),
        crate::safety::SafetyConfig::default(),
    );
    app.splash_visible = false;
    app.last_error = Some("not connected".into());
    app.mode = crate::app::Mode::ErrorDetail;
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
    term.draw(|f| draw(f, &mut app)).unwrap();
    let buf = term.backend().buffer().clone();
    let text: String = (0..buf.area.height)
        .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
        .map(|(x, y)| buf[(x, y)].symbol().to_string())
        .collect();
    assert!(text.contains("nothing more to show"), "{text}");
    assert!(!text.contains("DbError"));
}

/// A picker narrower than its panel sits in the middle of it, one row
/// below the top, and never on the panel's own frame.
#[test]
fn a_floated_popup_is_centred_across_the_panel_and_off_its_frame() {
    use ratatui::layout::Rect;
    let panel = Rect {
        x: 0,
        y: 10,
        width: 120,
        height: 20,
    };
    let r = crate::ui::floated_in_panel(panel, 100, 8);
    assert_eq!((r.x, r.y, r.width, r.height), (10, 11, 100, 8));
    // As wide as the panel: inside its borders, not on them.
    let r = crate::ui::floated_in_panel(panel, 200, 40);
    assert_eq!((r.x, r.y, r.width, r.height), (1, 11, 118, 18));
}

/// A broken environments.toml means pgman starts here, not on the
/// overview: the card says so, and where the details are.
#[test]
fn the_no_connection_card_points_at_a_broken_environments_toml() {
    let screen = |broken: bool, picks: bool| {
        let mut app = App::new(
            Theme::default(),
            None,
            Vec::new(),
            crate::safety::SafetyConfig::default(),
        );
        app.splash_visible = false;
        app.environments_config_broken = broken;
        if picks {
            app.conn_pick.picks = vec![crate::app::DataSourcePick {
                name: "dataSource (application)".into(),
                origin: "Spring",
                dsn: None,
                unresolved: Vec::new(),
                unresolved_host: Vec::new(),
                creds: Default::default(),
            }];
        }
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].symbol().to_string())
            .collect::<String>()
    };
    let hint = "environments.toml didn't load — press E for the details";
    for picks in [false, true] {
        let text = screen(true, picks);
        assert!(text.contains(hint), "{text}");
        assert!(text.contains("no connection"), "{text}");
        assert!(!screen(false, picks).contains("environments.toml"));
    }
}

/// An advisor finding's detail strip puts the setting and the
/// suggestion right under its title, so an 80×24 terminal shows them;
/// a lint finding's keeps its object line.
#[test]
fn the_advisor_detail_strip_leads_with_what_to_do() {
    let theme = crate::theme::Theme::default();
    let text = |lines: Vec<Line<'static>>| -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    };
    let adv = crate::demo::advisor()
        .findings
        .into_iter()
        .find(|f| f.code == "ADV004")
        .unwrap();
    let rows = text(super::schema::lint_detail_lines(&adv, 70, &theme));
    assert!(rows[0].starts_with("  ADV004 · "), "{rows:?}");
    assert!(
        rows[1].starts_with("  setting:  idle_in_transaction_session_timeout 1d (this session)"),
        "{rows:?}"
    );
    // The setting wraps with a hanging indent; the suggestion follows it.
    assert_eq!(rows[2], "            (no restart)", "{rows:?}");
    assert!(
        rows[3].starts_with("  suggest:  ALTER ROLE finance_ro"),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|r| r.starts_with("  evidence: a snapshot, now")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.starts_with("  RDS:      ")),
        "{rows:?}"
    );
    assert!(rows.iter().all(|r| display_width(r) <= 72), "{rows:?}");
    let lint = crate::query::lint::fk_without_index_finding("public", "t", "fk", "c");
    let rows = text(super::schema::lint_detail_lines(&lint, 70, &theme));
    assert_eq!(rows[1], "  object: public.t.fk");
}

/// A lint finding's detail strip breaks between words at every width
/// (it wrapped by characters: `referen` / `ces`), and its heading and
/// object rows wrap rather than run past the strip.
#[test]
fn the_lint_detail_strip_never_breaks_inside_a_word() {
    let theme = crate::theme::Theme::default();
    let lint = crate::query::lint::fk_without_index_finding(
        "public",
        "accounts",
        "accounts_customer_id_fkey",
        "customer_id",
    );
    let words: std::collections::HashSet<String> = [
        &lint.title,
        &lint.object,
        &lint.detail,
        lint.suggestion.as_ref().unwrap(),
    ]
    .iter()
    .flat_map(|s| s.split_whitespace().map(str::to_string))
    .chain(["LINT101", "·", "object:", "suggest:"].map(String::from))
    .collect();
    for width in [50usize, 58, 66, 70, 120] {
        let rows: Vec<String> = super::schema::lint_detail_lines(&lint, width, &theme)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        for row in &rows {
            assert!(display_width(row) <= width + 2, "{width}: {row:?}");
            for word in row.split_whitespace() {
                assert!(words.contains(word), "{width}: {word:?} cut from {row:?}");
            }
        }
    }
}

/// The wizard list gives `title` room to say something: at 80 columns
/// it was 3 wide (header `tit`), with `object` holding 48.
#[test]
fn lint_columns_shrinks_object_before_title() {
    use super::schema::{lint_columns, LINT_TITLE_MIN};
    // Wide: object gets its widest value, capped at 48.
    assert_eq!(lint_columns(200, 30), (30, 200 - 20 - 30));
    assert_eq!(lint_columns(200, 70), (48, 200 - 20 - 48));
    // The 80x24 wizard's row (71 columns): title keeps its minimum.
    let (object, title) = lint_columns(71, 49);
    assert_eq!(title, LINT_TITLE_MIN);
    assert_eq!(object + title + 20, 71);
    // Short objects leave the rest to title.
    assert_eq!(lint_columns(71, 17), (17, 71 - 20 - 17));
    // Never narrower than the `object` header itself.
    assert_eq!(lint_columns(30, 40).0, "object".len());
}

/// The whole screen at `w`×`h` as one string.
fn screen_text(a: &mut App, w: u16, h: u16) -> String {
    a.splash_visible = false;
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    term.draw(|f| draw(f, a)).unwrap();
    let buf = term.backend().buffer().clone();
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The list panels draw a header row above their rows; the scroll has
/// to keep the cursor's row in the rows left under it. Counting the
/// header as a row left the last row, under the cursor, one past the
/// bottom.
#[test]
fn list_panels_keep_the_cursor_row_on_screen_under_their_header() {
    use crate::app::Mode;
    let n = 40;
    let mut a = settled_app(false, false);
    a.schema_lint.findings = (0..n)
        .map(|i| crate::query::lint::Finding {
            severity: crate::query::lint::Severity::Low,
            code: "LINT105",
            title: "t".into(),
            object: format!("public.tbl_{i:02}"),
            detail: "d".into(),
            suggestion: None,
            advice: None,
        })
        .collect();
    a.schema_lint.cursor = n - 1;
    a.mode = Mode::SchemaLint;
    let s = screen_text(&mut a, 100, 30);
    // The list row, not the detail strip's `object:` line.
    assert!(s.contains("LINT105  public.tbl_39"), "wizard:\n{s}");

    let mut a = settled_app(false, false);
    a.sessions.rows = (0..n as i32)
        .map(|i| crate::query::sessions::SessionRow {
            pid: 7000 + i,
            user: "u".into(),
            application: "a".into(),
            state: "active".into(),
            wait_event: None,
            blocked_by: String::new(),
            query: format!("SELECT {i}"),
            age_secs: 1.0,
        })
        .collect();
    a.sessions.cursor = n - 1;
    a.mode = Mode::Sessions;
    let s = screen_text(&mut a, 100, 30);
    assert!(s.contains("7039"), "sessions:\n{s}");

    let mut a = settled_app(false, false);
    a.slow_queries.rows = (0..n)
        .map(|i| crate::query::slow_queries::SlowQueryRow {
            query: format!("SELECT slow_{i:02}"),
            calls: 1,
            total_ms: 1.0,
            mean_ms: 1.0,
            rows: 1,
        })
        .collect();
    a.slow_queries.cursor = n - 1;
    a.mode = Mode::SlowQueries;
    let s = screen_text(&mut a, 100, 30);
    // The detail strip shows the focused query too: count the list's.
    assert!(
        s.matches("SELECT slow_39").count() >= 2,
        "slow queries:\n{s}"
    );

    let mut a = settled_app(false, false);
    a.notifications.items = (0..n as i32)
        .map(|i| crate::conn::NotificationMsg {
            channel: format!("chan_{i:02}"),
            pid: 1,
            payload: "p".into(),
        })
        .collect();
    a.notifications.cursor = n - 1;
    a.mode = Mode::Notifications;
    let s = screen_text(&mut a, 100, 30);
    assert!(s.contains("chan_39"), "notifications:\n{s}");
}

/// Twenty statements, each with its own table, caller frame and
/// transaction, run five times apiece: one hotspot, caller group,
/// transaction and N+1 finding each, and a baseline diff entry each
/// against the empty ring captured first. The SQL ends `kNN`.
fn tap_app_with_twenty_tokens() -> App {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut a = settled_app(false, false);
    a.mode = crate::app::Mode::TapMonitor;
    a.on_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT));
    assert!(a.tap_baseline.is_some());
    for i in 0..20 {
        for _ in 0..crate::tap::NPLUS1_MIN_REPEATS {
            let mut e = tap_query_with_params(&format!("SELECT a FROM k{i:02}"), &[], false);
            e.caller = Some(vec![format!("com.example.Repo.k{i:02}(Repo.java:1)")]);
            e.txn = Some(format!("c-1#{i}"));
            a.on_tap_event(e);
        }
    }
    assert_eq!(a.current_nplus1().len(), 20);
    assert_eq!(a.current_txns().len(), 20);
    assert_eq!(a.current_baseline_diff().len(), 20);
    a
}

/// The text of each screen row drawn on the selected-row background.
fn selected_rows(a: &mut App, w: u16, h: u16) -> Vec<String> {
    a.splash_visible = false;
    let bg = a.theme.row_selected_bg;
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    term.draw(|f| draw(f, a)).unwrap();
    let buf = term.backend().buffer().clone();
    (0..buf.area.height)
        .filter(|&y| (0..buf.area.width).any(|x| buf[(x, y)].bg == bg))
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

/// The tap views draw a header row above their rows (Baseline, its
/// summary lines too). Scrolling as if the header were a row left the
/// focused row one past the panel's bottom edge: at 80x24, `j` ×7 in
/// the List showed row 7's parameters with row 7 not drawn, and `G`
/// hid the last row.
#[test]
fn tap_views_keep_the_focused_row_on_screen_under_their_header() {
    use crate::app::TapView;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for view in [
        TapView::List,
        TapView::Hotspots,
        TapView::Callers,
        TapView::Transactions,
        TapView::NplusOne,
        TapView::Baseline,
    ] {
        let mut a = tap_app_with_twenty_tokens();
        a.mode = crate::app::Mode::TapMonitor;
        a.tap_nav.view = view;
        let keys = std::iter::repeat_n('j', 7).chain(std::iter::once('G'));
        for (n, key) in keys.enumerate() {
            a.on_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
            let rows = selected_rows(&mut a, 80, 24);
            assert_eq!(rows.len(), 1, "{view:?} after key {n} ({key}): {rows:?}");
            // At 80 columns the aggregate views cut their rows before
            // the token; the List row shows it, so check it is the
            // focused event's row and not some other.
            if view == TapView::List {
                let sql = a.tap_events[a.tap_nav.events_cursor].sql.clone().unwrap();
                let token = &sql[sql.len() - 3..];
                assert!(rows[0].contains(token), "List: {token} not in {rows:?}");
            }
        }
    }
}

// ---------------------------------------------------------------
// The result diff's title: the key before the query text.
// ---------------------------------------------------------------

#[test]
fn result_diff_title_is_whole_when_it_fits() {
    let got = results::result_diff_title("key: id (primary key)", ("q1", 5), ("q2", 6), 200);
    assert_eq!(
        got,
        "Result diff — key: id (primary key) · A: q1 (5 rows) vs B: q2 (6 rows) · r re-pin · c clear · q close"
    );
}

#[test]
fn result_diff_title_drops_the_hints_then_cuts_the_labels_never_the_key() {
    let a = "SELECT name, id FROM ledger.accounts ORDER BY id";
    let b = "SELECT name, id FROM ledger.accounts WHERE active";
    for budget in [40usize, 64, 80, 100, 120] {
        let got = results::result_diff_title("key: id (primary key)", (a, 5), (b, 6), budget);
        assert!(display_width(&got) <= budget, "{budget}: {got}");
        assert!(
            got.starts_with("Result diff — key: id (primary key) · "),
            "{budget}: {got}"
        );
        assert!(got.ends_with(" · …"), "{budget}: {got}");
    }
    // Room for both labels, cut: each ends on `…`, the counts stay.
    let got = results::result_diff_title("key: id (primary key)", (a, 5), (b, 6), 100);
    assert!(got.contains("A: SELECT"), "{got}");
    assert!(got.contains("… (5 rows) vs B: SELECT"), "{got}");
    assert!(got.contains("… (6 rows)"), "{got}");
    // A short label keeps its whole text and leaves its room to the other.
    let got = results::result_diff_title("key: id", ("q1", 5), (b, 6), 70);
    assert!(got.contains("A: q1 (5 rows) vs B: SELECT name"), "{got}");
    // Too narrow for labels: the key survives on its own.
    let got = results::result_diff_title("key: id (primary key)", (a, 5), (b, 6), 50);
    assert_eq!(got, "Result diff — key: id (primary key) · …");
}

/// Every tap view, at every swept size, ends each row inside the
/// panel on a space or on `…`: a row the panel's border cuts with no
/// marker (hotspots' `· InvoiceS`, callers' SQL at 120 columns) fails.
/// The demo's rows never fill a line exactly, so a letter in the last
/// column can only be a cut.
#[test]
fn no_tap_view_row_is_cut_by_the_border_without_an_ellipsis() {
    use crate::app::TapView;
    let views = [
        TapView::List,
        TapView::Hotspots,
        TapView::Callers,
        TapView::Transactions,
        TapView::Pools,
        TapView::NplusOne,
        TapView::Baseline,
    ];
    for view in views {
        for (w, h) in [(60u16, 16u16), (80, 24), (100, 30), (120, 40), (200, 50)] {
            let mut a = crate::demo::app(Theme::default());
            a.splash_visible = false;
            a.splash_until = None;
            a.mode = crate::app::Mode::TapMonitor;
            a.tap_nav.view = view;
            if view == TapView::Baseline {
                a.on_key(crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char('B'),
                    crossterm::event::KeyModifiers::SHIFT,
                ));
            }
            let mut term =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            term.draw(|f| draw(f, &mut a)).unwrap();
            let buf = term.backend().buffer().clone();
            let row = |y: u16| -> Vec<String> {
                (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect()
            };
            let top = (0..h)
                .find(|&y| row(y).concat().contains("JDBC tap ◆ ]"))
                .unwrap_or_else(|| panic!("{view:?} {w}x{h}: no tap panel"));
            let cells = row(top);
            let title_x = cells.iter().position(|c| c == "[").unwrap();
            let left = (0..title_x).rev().find(|&x| cells[x] == "╭").unwrap();
            let right = (title_x..w as usize).find(|&x| cells[x] == "╮").unwrap();
            for y in top + 1..h {
                let cells = row(y);
                if cells[left] == "╰" {
                    break;
                }
                let last = &cells[right - 1];
                assert!(
                    last == " " || last == "…" || last == "─",
                    "{view:?} at {w}x{h}: row {y} cut by the border: {:?}",
                    cells[left..=right].concat()
                );
            }
        }
    }
}

/// The help section list's teasers: whole at 80 columns, and at 60
/// columns each keeps its lead (the key, as in `:tap / F4 · …`) whole.
/// At 80x24 the tap's teaser ended `(:…`, cutting the `:tap / F4` it
/// exists to show.
#[test]
fn help_teasers_fit_at_80_columns_and_keep_their_key_at_60() {
    let screen = |w: u16, h: u16, section: usize| -> String {
        let mut a = crate::demo::app(Theme::default());
        a.splash_visible = false;
        a.splash_until = None;
        a.mode = crate::app::Mode::Help;
        a.help.section = section;
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, &mut a)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    for (i, (title, teaser)) in HELP_SECTIONS.iter().enumerate() {
        let shown = screen(80, 24, i);
        assert!(
            shown.contains(teaser),
            "{title}: teaser {teaser:?} cut at 80x24:\n{shown}"
        );
        if let Some((lead, _)) = teaser.split_once(" · ") {
            let shown = screen(60, 16, i);
            assert!(
                shown.contains(&format!("{lead} · ")),
                "{title}: lead {lead:?} cut at 60x16:\n{shown}"
            );
        }
    }
}

#[test]
fn grid_header_text_cuts_the_name_and_keeps_the_sort_marker() {
    use super::results::grid_header_text;
    assert_eq!(grid_header_text("billing_email", "", 13), "billing_email");
    assert_eq!(grid_header_text("billing_email", "", 8), "billing…");
    assert_eq!(
        grid_header_text("billing_email", " ▲", 15),
        "billing_email ▲"
    );
    assert_eq!(grid_header_text("billing_email", " ▲", 10), "billing… ▲");
    // No room for the marker and a name: the whole is cut.
    assert_eq!(grid_header_text("id", " ▼", 2), "i…");
    assert_eq!(grid_header_text("id", "", 0), "");
    for w in 0..20 {
        assert!(
            display_width(&grid_header_text("billing_email", " ▲", w)) <= w,
            "{w}"
        );
    }
}

#[test]
fn columns_differ_note_names_the_shape_or_nothing() {
    use super::results::columns_differ_note;
    let cols = |c: &[&str]| c.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        columns_differ_note(&cols(&["id", "name"]), &cols(&["id", "name"])),
        None
    );
    let note = columns_differ_note(&cols(&["id", "name", "email"]), &cols(&["id"])).unwrap();
    assert!(
        note.starts_with("columns differ: A has 3 columns, B has 1"),
        "{note}"
    );
    let note = columns_differ_note(&cols(&["id", "name"]), &cols(&["id", "email"])).unwrap();
    assert!(note.contains("name their 2 columns differently"), "{note}");
    // The same names reordered are not named differently.
    let note = columns_differ_note(&cols(&["id", "name"]), &cols(&["name", "id"])).unwrap();
    assert!(note.contains("same 2 columns in another order"), "{note}");
}

/// The connection-lost card with a hot-standby conflict's reason and
/// hint, at 80x24 as the panel saw it: the hint is there whole, wrapped
/// on words, and the log path and the `r retry` line still fit under it.
#[test]
fn the_lost_card_keeps_the_servers_hint_and_its_keys_at_80_columns() {
    let mut a = settled_app(false, false);
    a.dsn = crate::conn::Dsn::parse("postgres://app@replica-db:5432/shop").ok();
    a.conn_state = crate::app::ConnState::Failed(
        "connection lost · terminating connection due to conflict with recovery · \
         In a moment you should be able to reconnect to the database and repeat your command."
            .into(),
    );
    let screen = screen_text(&mut a, 80, 24);
    let words: String = screen
        .lines()
        .map(|l| l.trim_matches(|c: char| c == '│' || c.is_whitespace()))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        words.contains("reconnect to the database and repeat your command."),
        "{screen}"
    );
    assert!(screen.contains("r retry"), "{screen}");
    assert!(screen.contains("logs"), "{screen}");
}
