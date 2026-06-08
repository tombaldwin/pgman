use super::*;

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
    // Spring Boot starter snippet.
    assert!(
        dump.contains("pgman-tap-spring-boot-starter"),
        "got:\n{dump}"
    );
    assert!(dump.contains("pgman.tap.enabled"), "got:\n{dump}");
    // Honest about the JAR still being in development.
    assert!(
        dump.contains("Route 1 works today"),
        "expected an honest note that Route 2 isn't shipped yet; got:\n{dump}"
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
