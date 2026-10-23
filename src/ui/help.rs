//! The help overlay: a section list plus one section, a `/` filter
//! across every section, and `:help all`'s flat page. The content is
//! `ui::help_body`, unchanged; its anchor map (heading → row) is what
//! splits it into sections. See `docs/backlog/help-overlay.md`.

use super::*;
use crate::app::HelpView;
use std::collections::HashMap;
use std::ops::Range;

use super::panels::wrap_help_lines;

/// Body width from which the section list sits beside the section. Below
/// it the list and the section are two levels (Enter / Esc between them).
pub(crate) const HELP_SPLIT_MIN_WIDTH: u16 = 110;

/// Is the overlay drawn split (list beside section) at body width
/// `width`? The renderer, the footer and — through `HelpUi::split` —
/// the key handler all go by this one answer. Pure.
pub(crate) fn help_is_split(width: u16) -> bool {
    width >= HELP_SPLIT_MIN_WIDTH
}

/// The rows of each section in `help_body`'s lines: from its heading
/// (`starts[i]`) up to the next heading, trailing blank rows dropped.
/// `starts` is in document order, one per `HELP_SECTIONS` entry. Pure.
pub(crate) fn help_section_ranges(texts: &[String], starts: &[usize]) -> Vec<Range<usize>> {
    starts
        .iter()
        .enumerate()
        .map(|(i, &start)| {
            let start = start.min(texts.len());
            let next = starts.get(i + 1).copied().unwrap_or(texts.len());
            let mut end = next.clamp(start, texts.len());
            while end > start + 1 && texts[end - 1].trim().is_empty() {
                end -= 1;
            }
            start..end.max((start + 1).min(texts.len()))
        })
        .collect()
}

/// The filter's matches: for each section with any, its index and the
/// rows (heading excluded) that match `query` case-insensitively. A
/// section whose *name* matches keeps every row — `/confirm` should
/// find the confirm section, whose rows never say "confirm". An empty
/// query matches nothing. Pure.
pub(crate) fn help_filter_matches(
    texts: &[String],
    ranges: &[Range<usize>],
    query: &str,
) -> Vec<(usize, Vec<usize>)> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    ranges
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let body = r.start + 1..r.end;
            let title_hit = texts
                .get(r.start)
                .is_some_and(|t| t.to_lowercase().contains(&q));
            let rows: Vec<usize> = body
                .filter(|&row| {
                    let t = &texts[row];
                    !t.trim().is_empty() && (title_hit || t.to_lowercase().contains(&q))
                })
                .collect();
            (!rows.is_empty()).then_some((i, rows))
        })
        .collect()
}

/// The scroll that shows the heading at `anchor_row` as the first row
/// *not* covered by the `↑ N more above` indicator. That indicator is
/// drawn over the first visible row whenever the scroll is past 0, so
/// the scroll stops one row short of the heading (the blank row
/// between sections goes under the indicator). Clamped to
/// `max_scroll`. Pure.
pub(crate) fn help_anchor_scroll(anchor_row: u16, max_scroll: u16) -> u16 {
    anchor_row.saturating_sub(1).min(max_scroll)
}

/// Tab / `]` / `[` on a flat page (`:help all`, the filter's matches):
/// the scroll that brings the next (`forward`) or previous heading in
/// `heading_rows` to the top, by [`help_anchor_scroll`]'s rule. With
/// no heading further on, forward stays put; with none above, back
/// goes to the top. Pure.
pub(crate) fn help_heading_jump(
    heading_rows: &[u16],
    scroll: u16,
    max_scroll: u16,
    forward: bool,
) -> u16 {
    let targets = heading_rows
        .iter()
        .map(|&r| help_anchor_scroll(r, max_scroll));
    if forward {
        targets.filter(|&t| t > scroll).min().unwrap_or(scroll)
    } else {
        targets.filter(|&t| t < scroll).max().unwrap_or(0)
    }
}

/// First list row to draw so `selected` stays in a `height`-row list.
/// Pure.
pub(crate) fn help_list_offset(selected: usize, height: usize) -> usize {
    if height == 0 || selected < height {
        0
    } else {
        selected + 1 - height
    }
}

fn plain(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

pub(super) fn draw_help(f: &mut Frame, area: Rect, app: &mut App) {
    let (raw, anchors) = help_body(&app.theme);
    if app.help.view == HelpView::All && app.help.filter.is_none() {
        draw_help_all(f, area, app, raw, anchors);
        return;
    }
    let theme = &app.theme;
    let texts: Vec<String> = raw.iter().map(plain).collect();
    let starts: Vec<usize> = HELP_SECTIONS
        .iter()
        .map(|(title, _)| anchors.get(title).map_or(0, |&r| r as usize))
        .collect();
    let ranges = help_section_ranges(&texts, &starts);
    let count = HELP_SECTIONS.len();
    app.help.section = app.help.section.min(count.saturating_sub(1));
    let section = app.help.section;

    let split = help_is_split(area.width);
    app.help.split = split;
    // A big terminal gets a centred card; anything smaller, the whole
    // body (a 70% card left a few rows of a long section).
    let popup = if area.width < 160 || area.height < 30 {
        area
    } else {
        centered_pct(area, 70, 80)
    };
    let in_list = !split && app.help.view == HelpView::List && app.help.filter.is_none();
    let title = if app.help.filter.is_some() {
        " help · filter ".to_string()
    } else if in_list {
        format!(" help · {count} sections ")
    } else {
        format!(" help · {} ", HELP_SECTIONS[section].0)
    };
    let block = crate::ui::chrome::panel_block(theme)
        .border_style(Style::default().fg(theme.border_active))
        .style(Style::default().fg(theme.text))
        .title(crate::ui::chrome::panel_title(theme, &title));
    let inner = block.inner(popup);
    f.render_widget(Clear, popup);
    f.render_widget(block, popup);
    // One column of padding each side.
    let inner = Rect {
        x: inner.x.saturating_add(1),
        width: inner.width.saturating_sub(2),
        ..inner
    };
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    // Row 0: the preamble — the global keys line `help_body` opens with.
    let preamble = raw.first().cloned().unwrap_or_default();
    let preamble_style = preamble.spans.first().map(|s| s.style).unwrap_or_default();
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            crate::grid::truncate_cell(&plain(&preamble), inner.width as usize),
            preamble_style,
        ))),
        Rect { height: 1, ..inner },
    );
    let body = Rect {
        y: inner.y + 1,
        height: inner.height - 1,
        ..inner
    };

    let content_rect = if split {
        let list_w = HELP_SECTIONS
            .iter()
            .map(|(t, _)| display_width(t))
            .max()
            .unwrap_or(0) as u16
            + 2;
        let list_w = list_w.min(body.width / 3);
        let list = Rect {
            width: list_w,
            ..body
        };
        // The filter's page spans every section, so no one entry in
        // the list is the one shown: none is marked.
        let marked = app.help.filter.is_none().then_some(section);
        draw_section_list(f, list, theme, marked, false);
        // A rule between the list and the section.
        let rule_x = body.x + list_w + 1;
        let rule: Vec<Line> = (0..body.height)
            .map(|_| Line::from(Span::styled("│", Style::default().fg(theme.border_idle))))
            .collect();
        f.render_widget(
            Paragraph::new(rule),
            Rect {
                x: rule_x,
                width: 1,
                ..body
            },
        );
        let gap = list_w + 3;
        Rect {
            x: body.x + gap,
            width: body.width.saturating_sub(gap),
            ..body
        }
    } else if in_list {
        draw_section_list(f, body, theme, Some(section), true);
        app.help.max_scroll = 0;
        app.help.heading_rows.clear();
        return;
    } else {
        body
    };

    let width = content_rect.width as usize;
    let (lines, heading_rows) = match app.help.filter.as_deref() {
        Some(query) => {
            let (lines, heads) = filter_lines(&raw, &texts, &ranges, query, theme);
            let (wrapped, mapped) = wrap_help_lines(lines, heads, width);
            let mut rows: Vec<u16> = mapped.into_values().collect();
            rows.sort_unstable();
            (wrapped, rows)
        }
        None => {
            let lines = raw[ranges[section].clone()].to_vec();
            let (wrapped, _) = wrap_help_lines(lines, HashMap::new(), width);
            (wrapped, Vec::new())
        }
    };
    app.help.heading_rows = heading_rows;
    draw_scrolled(f, content_rect, lines, app);
}

/// The filter's page: a summary row, then each matching section's
/// heading with its matching rows under it, plus the heading → row map
/// Tab / `]` / `[` jump by.
fn filter_lines(
    raw: &[Line<'static>],
    texts: &[String],
    ranges: &[Range<usize>],
    query: &str,
    theme: &Theme,
) -> (Vec<Line<'static>>, HashMap<&'static str, u16>) {
    let groups = help_filter_matches(texts, ranges, query);
    let rows: usize = groups.iter().map(|(_, r)| r.len()).sum();
    let q = query.trim();
    let summary = if q.is_empty() {
        "  type to filter every section".to_string()
    } else if groups.is_empty() {
        format!("  no help row matches '{q}'")
    } else {
        let s = |n: usize| if n == 1 { "" } else { "s" };
        format!(
            "  {rows} row{} in {} section{} match '{q}'",
            s(rows),
            groups.len(),
            s(groups.len())
        )
    };
    let mut lines = vec![
        Line::from(Span::styled(summary, Style::default().fg(theme.muted))),
        Line::from(""),
    ];
    let mut heads = HashMap::new();
    for (i, (section, rows)) in groups.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        heads.insert(HELP_SECTIONS[*section].0, lines.len() as u16);
        lines.push(raw[ranges[*section].start].clone());
        lines.extend(rows.iter().map(|&r| raw[r].clone()));
    }
    (lines, heads)
}

/// The section list: one row per section, `selected` (if any) marked. With
/// `teasers` (the narrow terminal's level 1) each name is followed by
/// its one-line teaser, cut with an ellipsis where it doesn't fit.
fn draw_section_list(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    selected: Option<usize>,
    teasers: bool,
) {
    let count = HELP_SECTIONS.len();
    // A list taller than the area gives up its last row to say how
    // many entries are out of sight, rather than ending mid-list with
    // nothing to show there is more.
    let overflow = count > area.height as usize && area.height >= 2;
    let height = area.height as usize - usize::from(overflow);
    let offset = help_list_offset(selected.unwrap_or(0), height);
    let name_w = HELP_SECTIONS
        .iter()
        .map(|(t, _)| display_width(t))
        .max()
        .unwrap_or(0);
    let width = area.width as usize;
    let lines: Vec<Line> = HELP_SECTIONS
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(i, (title, teaser))| {
            let chosen = Some(i) == selected;
            let marker = if chosen { "▸ " } else { "  " };
            let name = crate::grid::truncate_cell(
                &format!("{marker}{title}"),
                width.min(name_w + marker.chars().count()),
            );
            let base = if chosen {
                Style::default()
                    .bg(theme.row_selected_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let mut spans = vec![Span::styled(
                name.clone(),
                base.fg(if chosen { theme.text } else { theme.accent }),
            )];
            if teasers {
                let used = display_width(&name);
                let pad = (name_w + 2 + 2).saturating_sub(used);
                let room = width.saturating_sub(used + pad);
                if room > 0 {
                    spans.push(Span::styled(" ".repeat(pad), base));
                    spans.push(Span::styled(
                        crate::grid::truncate_cell(teaser, room),
                        base.fg(theme.muted),
                    ));
                }
            }
            Line::from(spans)
        })
        .collect();
    let mut lines = lines;
    if overflow {
        let below = count.saturating_sub(offset + height);
        let mark = match (offset, below) {
            (0, b) => format!("  ↓ {b} more"),
            (a, 0) => format!("  ↑ {a} more"),
            (a, b) => format!("  ↑ {a} more · ↓ {b} more"),
        };
        lines.push(Line::from(Span::styled(
            crate::grid::truncate_cell(&mark, area.width as usize),
            Style::default().fg(theme.accent),
        )));
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// Draw `lines` scrolled by `app.help.scroll` into `area`, write the
/// frame's `max_scroll` back (clamping the stored scroll to it, so a
/// terminal that grew can't leave a scroll the keys walk through
/// invisibly), and mark more above / below over the first / last row.
fn draw_scrolled(f: &mut Frame, area: Rect, lines: Vec<Line<'static>>, app: &mut App) {
    let theme = &app.theme;
    let total = lines.len() as u16;
    let max_scroll = total.saturating_sub(area.height);
    app.help.max_scroll = max_scroll;
    app.help.scroll = app.help.scroll.min(max_scroll);
    let scroll = app.help.scroll;
    // No `.wrap(…)`: `wrap_help_lines` already re-flowed every row to
    // the pane's width, and that is the wrap the heading rows were
    // counted over.
    f.render_widget(
        Paragraph::new(lines)
            .scroll((scroll, 0))
            .style(Style::default().fg(theme.text)),
        area,
    );
    draw_scroll_marks(f, area, scroll, max_scroll, theme);
}

/// "↑ N more above" over the first row of `area` and "↓ N more below"
/// over its last, when there is more that way.
fn draw_scroll_marks(f: &mut Frame, area: Rect, scroll: u16, max_scroll: u16, theme: &Theme) {
    let hint_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let mut mark = |y: u16, text: String| {
        let row = Rect {
            y,
            height: 1,
            ..area
        };
        f.render_widget(Clear, row);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(text, hint_style))),
            row,
        );
    };
    if area.height == 0 {
        return;
    }
    if scroll > 0 {
        mark(area.y, format!("↑ {scroll} more above"));
    }
    if scroll < max_scroll {
        mark(
            area.y + area.height - 1,
            format!("↓ {} more below", max_scroll - scroll),
        );
    }
}

/// `:help all`: every section on one scrolling page — the overlay as it
/// was before it had sections.
fn draw_help_all(
    f: &mut Frame,
    area: Rect,
    app: &mut App,
    raw: Vec<Line<'static>>,
    anchors: HashMap<&'static str, u16>,
) {
    app.help.split = help_is_split(area.width);
    // On a smaller terminal 70 % left a few lines of a several-hundred
    // line sheet: there it takes the whole body.
    let popup = if area.width < 120 || area.height < 30 {
        area
    } else {
        centered_pct(area, 70, 70)
    };
    // Body content width: popup minus borders (2) and the uniform(1)
    // padding (2). Long descriptions get wrapped to this width by
    // `wrap_help_lines`, with a hanging indent to the description
    // column, rather than by `Paragraph`'s `Wrap` — which has no notion
    // of where the description starts and so wraps continuations to
    // column 0.
    let inner_width = popup.width.saturating_sub(4) as usize;
    let (lines, anchors) = wrap_help_lines(raw, anchors, inner_width);
    let mut rows: Vec<u16> = anchors.into_values().collect();
    rows.sort_unstable();
    app.help.heading_rows = rows;
    let theme = &app.theme;
    f.render_widget(Clear, popup);
    let block = crate::ui::chrome::panel_block(theme)
        .border_style(Style::default().fg(theme.border_active))
        .padding(Padding::uniform(1));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    draw_scrolled(f, inner, lines, app);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_split_is_decided_at_110_columns() {
        assert!(!help_is_split(80));
        assert!(!help_is_split(109));
        assert!(help_is_split(110));
        assert!(help_is_split(200));
    }

    #[test]
    fn a_section_runs_to_the_next_heading_without_its_trailing_blanks() {
        let t = texts(&[
            "title", "", "  a", "    x", "", "  b", "    y", "    z", "", "", "  c",
        ]);
        let r = help_section_ranges(&t, &[2, 5, 10]);
        assert_eq!(r, vec![2..4, 5..8, 10..11]);
        // A heading with nothing under it is still one row, never empty.
        let r = help_section_ranges(&texts(&["  a", "", "  b"]), &[0, 2]);
        assert_eq!(r, vec![0..1, 2..3]);
        // Out-of-order or out-of-range starts don't panic or invert.
        let r = help_section_ranges(&texts(&["  a", "x"]), &[1, 0, 9]);
        assert_eq!(r, vec![1..2, 0..2, 2..2], "clamped into the body");
    }

    #[test]
    fn the_real_help_body_splits_into_every_section_in_order() {
        let theme = Theme::default();
        let (raw, anchors) = help_body(&theme);
        let t: Vec<String> = raw.iter().map(plain).collect();
        let starts: Vec<usize> = HELP_SECTIONS
            .iter()
            .map(|(title, _)| anchors[title] as usize)
            .collect();
        let ranges = help_section_ranges(&t, &starts);
        assert_eq!(ranges.len(), HELP_SECTIONS.len());
        for (r, (title, _)) in ranges.iter().zip(HELP_SECTIONS) {
            assert_eq!(t[r.start].trim(), *title, "a section starts at its heading");
            assert!(r.end - r.start > 1, "{title}: a section has rows");
            assert!(
                !t[r.end - 1].trim().is_empty(),
                "{title}: trailing blank kept"
            );
        }
        // Every row of the body is in exactly one section, or is the
        // preamble or a blank between sections: nothing is lost.
        let covered: usize = ranges.iter().map(|r| r.len()).sum();
        let blanks_between = (starts[0]..t.len())
            .filter(|&i| !ranges.iter().any(|r| r.contains(&i)))
            .inspect(|&i| assert!(t[i].trim().is_empty(), "row {i} dropped: {:?}", t[i]))
            .count();
        assert_eq!(covered + blanks_between, t.len() - starts[0]);
    }

    #[test]
    fn help_body_headings_are_help_sections_in_order() {
        let theme = Theme::default();
        let (raw, anchors) = help_body(&theme);
        let mut by_row: Vec<(&u16, &&str)> = anchors.iter().map(|(k, v)| (v, k)).collect();
        by_row.sort();
        let headings: Vec<&str> = by_row.iter().map(|(_, k)| **k).collect();
        let listed: Vec<&str> = HELP_SECTIONS.iter().map(|(t, _)| *t).collect();
        assert_eq!(headings, listed);
        assert!(HELP_SECTIONS.iter().all(|(_, teaser)| !teaser.is_empty()));
        assert!(raw.len() > HELP_SECTIONS.len());
    }

    #[test]
    fn the_filter_keeps_matching_rows_under_their_sections() {
        let t = texts(&[
            "  grid",
            "    q   quit",
            "    y   yank a row",
            "",
            "  editor",
            "    ctrl-y redo",
            "    esc back",
            "",
            "  confirm",
            "    y run it",
            "    n cancel",
        ]);
        let ranges = vec![0..3, 4..7, 8..11];
        assert_eq!(
            help_filter_matches(&t, &ranges, "YANK"),
            vec![(0, vec![2])],
            "case-insensitive, and the other sections drop out"
        );
        assert_eq!(
            help_filter_matches(&t, &ranges, "y"),
            vec![(0, vec![2]), (1, vec![5]), (2, vec![9])]
        );
        // A section's own name matches every row in it.
        assert_eq!(
            help_filter_matches(&t, &ranges, "confirm"),
            vec![(2, vec![9, 10])]
        );
        // Case-insensitive both ways: the rows' capitals match too.
        let t2 = texts(&["  grid", "    I   yank as INSERT", "  tabs", "    x"]);
        assert_eq!(
            help_filter_matches(&t2, &[0..2, 2..4], "insert"),
            vec![(0, vec![1])]
        );
        assert!(help_filter_matches(&t, &ranges, "").is_empty());
        assert!(help_filter_matches(&t, &ranges, "  ").is_empty());
        assert!(help_filter_matches(&t, &ranges, "zzz").is_empty());
    }

    #[test]
    fn heading_jumps_land_one_row_above_the_heading_and_stop_at_the_ends() {
        let rows = [2, 10, 30];
        assert_eq!(help_heading_jump(&rows, 0, 100, true), 1);
        assert_eq!(help_heading_jump(&rows, 1, 100, true), 9);
        assert_eq!(help_heading_jump(&rows, 9, 100, true), 29);
        assert_eq!(help_heading_jump(&rows, 29, 100, true), 29, "none further");
        assert_eq!(help_heading_jump(&rows, 29, 100, false), 9);
        assert_eq!(help_heading_jump(&rows, 5, 100, false), 1);
        assert_eq!(help_heading_jump(&rows, 1, 100, false), 0, "back to top");
        // Clamped: a heading past the end of the range scrolls to it.
        assert_eq!(help_heading_jump(&rows, 9, 20, true), 20);
        assert_eq!(help_heading_jump(&[], 7, 20, true), 7);
    }

    #[test]
    fn the_list_scrolls_only_once_the_selection_would_fall_off() {
        assert_eq!(help_list_offset(0, 10), 0);
        assert_eq!(help_list_offset(9, 10), 0);
        assert_eq!(help_list_offset(10, 10), 1);
        assert_eq!(help_list_offset(21, 10), 12);
        assert_eq!(help_list_offset(5, 0), 0);
    }

    #[test]
    fn the_footer_names_the_keys_of_the_page_on_screen() {
        let mut h = crate::app::HelpUi::default();
        // Narrow list: Enter opens, j/k select.
        let list = help_footer_hints(&h, 80);
        assert!(
            list.contains("enter open") && list.contains("j/k select"),
            "{list}"
        );
        // The same state at 110 columns is the split overlay: j/k scroll.
        let wide = help_footer_hints(&h, 110);
        assert!(
            wide.contains("j/k scroll") && !wide.contains("enter open"),
            "{wide}"
        );
        // A narrow section: Esc goes back to the list.
        h.view = HelpView::Section;
        assert!(help_footer_hints(&h, 80).starts_with("esc list"));
        assert!(help_footer_hints(&h, 110).starts_with("esc / ? close help"));
        // A kept filter: Esc clears it.
        h.filter = Some("x".into());
        assert!(help_footer_hints(&h, 80).contains("esc clear filter"));
        // Every page leads with the way out, so a narrow footer keeps it.
        for view in [HelpView::List, HelpView::Section, HelpView::All] {
            for filter in [None, Some("x".to_string())] {
                for width in [60, 80, 110, 200] {
                    let h = crate::app::HelpUi {
                        view,
                        filter: filter.clone(),
                        ..Default::default()
                    };
                    let hints = help_footer_hints(&h, width);
                    let first_two: Vec<&str> = hints.split(" · ").take(2).collect();
                    assert!(
                        first_two.iter().any(|s| s.ends_with("close help")),
                        "{view:?} {filter:?} {width}: {hints}"
                    );
                }
            }
        }
    }

    #[test]
    fn anchor_scroll_stops_a_row_short_and_clamps() {
        assert_eq!(help_anchor_scroll(0, 10), 0);
        assert_eq!(help_anchor_scroll(5, 10), 4);
        assert_eq!(help_anchor_scroll(50, 10), 10);
    }
}
