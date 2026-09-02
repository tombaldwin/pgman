use super::*;

/// Walk the highlighter spans that overlap `[line_start, line_end)`,
/// emitting one styled ratatui `Span` per highlight segment. When
/// `cursor_byte_in_line` is `Some`, that single char inside the line
/// renders with the `REVERSED` modifier so the cursor stays visible
/// regardless of which syntax colour the underlying byte sits in.
fn push_highlighted_line<'a>(
    out: &mut Vec<Span<'a>>,
    buf: &'a str,
    spans: &[crate::query::highlight::Span],
    line_start: usize,
    line_end: usize,
    cursor_byte_in_line: Option<usize>,
    theme: &'a Theme,
) {
    use crate::query::highlight::TokenClass;
    let line_text = &buf[line_start..line_end];

    // No highlight spans (unclassified path, e.g. empty cache fallback
    // before we connect). Render plain.
    if spans.is_empty() {
        push_with_cursor(out, line_text, theme.text, cursor_byte_in_line);
        return;
    }

    let color_for = |c: TokenClass| -> ratatui::style::Color {
        match c {
            TokenClass::Keyword => theme.title,
            TokenClass::Function => theme.accent,
            TokenClass::String => theme.syn_string,
            TokenClass::Comment | TokenClass::Number => theme.muted,
            TokenClass::UnknownIdent => theme.syn_unknown,
            TokenClass::KnownIdent
            | TokenClass::Identifier
            | TokenClass::Operator
            | TokenClass::Whitespace => theme.text,
        }
    };

    let mut byte_in_line = 0usize;
    for s in spans {
        // Clip to the line range. Skip spans entirely outside.
        if s.end <= line_start {
            continue;
        }
        if s.start >= line_end {
            break;
        }
        let clip_start = s.start.max(line_start);
        let clip_end = s.end.min(line_end);
        if clip_start >= clip_end {
            continue;
        }
        let in_line_start = clip_start - line_start;
        let in_line_end = clip_end - line_start;
        if in_line_start > byte_in_line {
            // Defensive: a gap shouldn't happen (tokenize covers every
            // byte), but if one does, render it plain.
            push_with_cursor(
                out,
                &line_text[byte_in_line..in_line_start],
                theme.text,
                cursor_byte_in_line.map(|c| c.checked_sub(byte_in_line).unwrap_or(c)),
            );
        }
        let segment = &line_text[in_line_start..in_line_end];
        let color = color_for(s.class);
        // Offset the cursor position into this segment's coords if it
        // falls inside; otherwise pass None.
        let cursor_here = match cursor_byte_in_line {
            Some(c) if c >= in_line_start && c < in_line_end => Some(c - in_line_start),
            _ => None,
        };
        push_with_cursor(out, segment, color, cursor_here);
        byte_in_line = in_line_end;
    }
    // Trailing range with no spans (cursor at EOL on an empty line).
    if byte_in_line < line_text.len() {
        push_with_cursor(
            out,
            &line_text[byte_in_line..],
            theme.text,
            cursor_byte_in_line.and_then(|c| c.checked_sub(byte_in_line)),
        );
    }
    // Empty line with the cursor parked on it — show a single-space
    // REVERSED block so the cursor doesn't vanish.
    if line_text.is_empty() {
        if let Some(0) = cursor_byte_in_line {
            out.push(Span::styled(
                " ".to_string(),
                Style::default().add_modifier(Modifier::REVERSED),
            ));
        }
    }
}

/// Render `segment` in `color`. When `cursor_byte` is `Some(n)` and
/// `n` is inside the segment, split it into [before, cursor-char,
/// after] so the cursor char gets the REVERSED modifier while still
/// inheriting the segment's syntax colour for the before / after
/// portions.
fn push_with_cursor<'a>(
    out: &mut Vec<Span<'a>>,
    segment: &str,
    color: ratatui::style::Color,
    cursor_byte: Option<usize>,
) {
    if segment.is_empty() {
        return;
    }
    let style = Style::default().fg(color);
    match cursor_byte {
        Some(c) if c < segment.len() => {
            // Walk to the next char boundary so we slice cleanly past
            // a multi-byte codepoint.
            let mut next = c + 1;
            while next < segment.len() && !segment.is_char_boundary(next) {
                next += 1;
            }
            out.push(Span::styled(segment[..c].to_string(), style));
            out.push(Span::styled(
                segment[c..next].to_string(),
                style.add_modifier(Modifier::REVERSED),
            ));
            out.push(Span::styled(segment[next..].to_string(), style));
        }
        Some(_at_or_past_end) => {
            // Cursor sits at the very end of this segment — render
            // the segment as normal; the next push (or the empty-line
            // tail) handles the cursor.
            out.push(Span::styled(segment.to_string(), style));
        }
        None => {
            out.push(Span::styled(segment.to_string(), style));
        }
    }
}

/// SQL editor pane — always visible, focused in `Mode::Editor`. Multi-line
/// buffer; the cursor renders as a reverse-video character on its line.
pub(super) fn draw_editor(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = &app.theme;
    let focused = app.mode == Mode::Editor;
    let border_color = if focused {
        theme.border_active
    } else {
        theme.border_idle
    };
    let total_lines = app.editor.buffer.matches('\n').count() + 1;
    let (cur_line_check, _) = crate::app::cursor_position(&app.editor.buffer, app.editor.cursor);
    let title_text = if focused {
        let base = match app.history_pos {
            None => "editor".to_string(),
            Some(i) => format!("editor · history {}/{}", i + 1, app.history.len()),
        };
        // Show line N/M when the buffer is long enough to actually scroll;
        // the visible window is bounded by the pane height (3-12).
        if total_lines > 10 {
            format!("{base} · line {}/{}", cur_line_check + 1, total_lines)
        } else {
            base
        }
    } else {
        "editor (e to focus)".to_string()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            format!(" {title_text} "),
            Style::default().fg(theme.title),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Refresh the cached highlight spans only when the buffer changed since
    // last frame (a schema change clears the cache on Booted / SchemaRefreshed).
    // The lex + O(identifiers × schema) classify otherwise re-ran every frame —
    // including ≈9fps during any animation — for an unchanged buffer. Done
    // before the `&app.editor.buffer` borrow below so the cache write is clean.
    if focused {
        let stale = match &app.editor_highlight_cache {
            Some((b, _)) => b != &app.editor.buffer,
            None => true,
        };
        if stale {
            let spans = if app.schema_cache.is_empty() {
                // No cache to resolve against — lex only; identifiers fall
                // back to the default text colour rather than turning red.
                crate::query::highlight::tokenize(&app.editor.buffer)
            } else {
                let from_before = crate::query::from_parse::parse_from_tables_resolved(
                    &app.editor.buffer,
                    &app.schema_cache,
                );
                let ctes = crate::query::clause::extract_ctes_resolved(
                    &app.editor.buffer,
                    &app.schema_cache,
                );
                let raw = crate::query::highlight::tokenize(&app.editor.buffer);
                crate::query::highlight::classify(
                    raw,
                    &app.editor.buffer,
                    &app.schema_cache,
                    &from_before,
                    &ctes,
                )
            };
            app.editor_highlight_cache = Some((app.editor.buffer.clone(), spans));
        }
    }

    let buf = &app.editor.buffer;
    let (cur_line, cur_col) = crate::app::cursor_position(buf, app.editor.cursor);
    let text_color = if focused { theme.text } else { theme.muted };

    // Unfocused, empty buffer — show a hint instead of an empty pane.
    if !focused && buf.is_empty() {
        let lines = vec![Line::from(vec![
            Span::styled("> ", Style::default().fg(theme.muted)),
            Span::styled(
                "(empty — press e to focus)",
                Style::default().fg(theme.muted),
            ),
        ])];
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
        return;
    }

    // Read the memoised highlight spans computed above (cheap clone — `Span`
    // is `Copy`, just byte offsets + a class). Unfocused panes get the muted
    // text colour for everything — syntax highlighting is for the active edit
    // surface — so they don't populate or read the cache.
    let highlight_spans: Vec<crate::query::highlight::Span> = if focused {
        app.editor_highlight_cache
            .as_ref()
            .map(|(_, spans)| spans.clone())
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut lines: Vec<Line> = Vec::new();
    let mut line_start_byte: usize = 0;
    for (i, line_text) in buf.split('\n').enumerate() {
        let prompt = if i == 0 { "> " } else { "  " };
        let mut spans: Vec<Span> = vec![Span::styled(
            prompt.to_string(),
            Style::default().fg(theme.muted),
        )];

        let line_end_byte = line_start_byte + line_text.len();
        if focused {
            // Cursor split — we still need to overlay the REVERSED
            // glyph on top of whatever syntax colour the byte sits in.
            let byte_at_col = if i == cur_line {
                line_text
                    .char_indices()
                    .nth(cur_col)
                    .map(|(b, _)| b)
                    .unwrap_or(line_text.len())
            } else {
                usize::MAX
            };
            push_highlighted_line(
                &mut spans,
                buf,
                &highlight_spans,
                line_start_byte,
                line_end_byte,
                if i == cur_line {
                    Some(byte_at_col)
                } else {
                    None
                },
                theme,
            );
        } else {
            spans.push(Span::styled(
                line_text.to_string(),
                Style::default().fg(text_color),
            ));
        }
        lines.push(Line::from(spans));
        // +1 for the newline we split on (except after the last line).
        line_start_byte = line_end_byte + 1;
    }

    // Vertical scroll: keep the cursor's line visible inside the pane's
    // limited height (3-12 rows incl. borders). For short buffers
    // editor.scroll stays 0; long buffers scroll to follow the cursor.
    let total_rendered = lines.len() as u16;
    let scroll = clamp_editor_scroll(
        app.editor.scroll,
        cur_line as u16,
        total_rendered,
        inner.height,
    );
    app.editor.scroll = scroll;
    f.render_widget(Paragraph::new(Text::from(lines)).scroll((scroll, 0)), inner);

    // Real terminal cursor — the blinking one most operators expect.
    // The REVERSED block underneath stays put so the column stays
    // obvious even when the OS cursor blinks off (some terminals
    // hide it during a long pause). Only shown when the editor is
    // focused.
    if focused {
        let visible_y = (cur_line as u16).saturating_sub(scroll);
        if visible_y < inner.height {
            // 2-char prompt prefix on every line ("> " or "  ").
            let x = inner.x.saturating_add(2).saturating_add(cur_col as u16);
            let y = inner.y.saturating_add(visible_y);
            if x < inner.x.saturating_add(inner.width) {
                f.set_cursor_position((x, y));
            }
        }
    }
}

/// One completion-popup row's pieces, sized to fit within `inner_w`
/// display columns (the popup's content width, excluding its
/// border). Nothing in pgman may truncate silently — every cut gets
/// an explicit `…` marker — so a row is shortened in priority order:
///
///   1. Show the label whole, plus the full `(kind · context)`
///      annotation.
///   2. Drop the annotation's context, keeping `(kind)`.
///   3. Drop the annotation entirely.
///   4. If the bare label (marker + head + tail) still doesn't fit,
///      ellipsise it with `grid::truncate_cell_parts` — the same
///      truncation convention, and the same marker styling, the
///      results grid uses.
///
/// `head` is the operator's typed/expanded prefix (rendered bold, to
/// match what's already in the buffer); `tail` is the remainder of
/// the candidate's display string. Both are treated as one label for
/// width purposes — ellipsising may shorten either or both.
struct FittedCompletionRow {
    head: String,
    tail: String,
    /// `"…"` when the label itself was cut, else `""`.
    ellipsis: &'static str,
    /// The `(kind · context)` annotation, already shortened to fit —
    /// or `""` if it was dropped entirely.
    kind: String,
    /// Trailing spaces to pad the row out to `inner_w` columns.
    pad: usize,
}

fn fit_completion_row(
    marker_w: usize,
    head: &str,
    tail: &str,
    kind_full: &str,
    kind_short: &str,
    inner_w: usize,
) -> FittedCompletionRow {
    let head_w = head.chars().count();
    let tail_w = tail.chars().count();
    let label_w = head_w + tail_w;

    for kind in [kind_full, kind_short, ""] {
        let kind_w = kind.chars().count();
        let total = marker_w + label_w + kind_w;
        if total <= inner_w {
            return FittedCompletionRow {
                head: head.to_string(),
                tail: tail.to_string(),
                ellipsis: "",
                kind: kind.to_string(),
                pad: inner_w - total,
            };
        }
    }

    // Even the bare label doesn't fit — ellipsise it. Reassemble
    // head+tail into one string first since the cut may land inside
    // either part (a long typed prefix is unusual but possible).
    let label: String = head.chars().chain(tail.chars()).collect();
    let budget = inner_w.saturating_sub(marker_w);
    let (kept, ellipsis) = grid::truncate_cell_parts(&label, budget);
    let kept_chars: Vec<char> = kept.chars().collect();
    let head_kept_n = head_w.min(kept_chars.len());
    let head_kept: String = kept_chars[..head_kept_n].iter().collect();
    let tail_kept: String = kept_chars[head_kept_n..].iter().collect();
    let shown = marker_w + kept_chars.len() + ellipsis.chars().count();
    FittedCompletionRow {
        head: head_kept,
        tail: tail_kept,
        ellipsis,
        kind: String::new(),
        pad: inner_w.saturating_sub(shown),
    }
}

/// Completion candidates popup. Anchored just under the editor pane,
/// flush-left over the body area. Shows up to ~10 candidates with the
/// active one highlighted; "↑ N more" / "↓ N more" markers when the
/// list is longer than the popup. Only the active cycle is rendered;
/// any non-Tab editor key dismisses (see `App::editor_key`).
pub(super) fn draw_completion_popup(f: &mut Frame, editor_area: Rect, body_area: Rect, app: &App) {
    let Some(cycle) = app.completion.as_ref() else {
        return;
    };
    if cycle.candidates.is_empty() {
        return;
    }
    let theme = &app.theme;
    // The right-side tail is `(kind · context)`. Long context strings
    // (e.g. a long schema or table name) would otherwise overflow the
    // popup width on narrow terminals and get raggedly truncated by
    // ratatui — defeating the disambiguation the context is there for.
    // Cap the rendered context length and ellipsise; preserve the
    // "..." suffix at the end so the kind label and the boundary
    // marker are always visible.
    const CONTEXT_MAX_CHARS: usize = 24;
    let truncated_context = |ctx: &str| -> String {
        let cc = ctx.chars().count();
        if cc <= CONTEXT_MAX_CHARS {
            ctx.to_string()
        } else {
            let keep = CONTEXT_MAX_CHARS.saturating_sub(1);
            let head: String = ctx.chars().take(keep).collect();
            format!("{head}…")
        }
    };
    let tail_of = |c: &Candidate| -> String {
        match &c.context {
            Some(ctx) => format!(" ({} · {})", c.kind.label(), truncated_context(ctx)),
            None => format!(" ({})", c.kind.label()),
        }
    };
    let tail_width = cycle
        .candidates
        .iter()
        .map(|c| tail_of(c).chars().count())
        .max()
        .unwrap_or(0);
    let label_width = cycle
        .candidates
        .iter()
        .map(|c| c.display.chars().count())
        .max()
        .unwrap_or(0);
    let content_width = (label_width + tail_width + 4) as u16;

    let total = cycle.candidates.len();
    // Computed here (rather than down where it's used to build the
    // block) because its length feeds into the popup width — a popup
    // sized only for the candidate rows truncated the title (e.g.
    // "Tab to┐" instead of the full "Tab to cycle ┐").
    let title = match cycle.selected {
        Some(i) => format!(" {}/{} ", i + 1, total),
        None => format!(" {total} matches · Tab to cycle "),
    };
    let title_width = title.chars().count() as u16;
    // +2 so the title text fits between the block's corner glyphs
    // without ratatui truncating it against the border fill.
    let desired_width = content_width.max(title_width + 2);

    // Show at most VISIBLE rows; auto-scroll keeps the active row in view.
    const VISIBLE: usize = 8;
    let visible = total.min(VISIBLE);
    let desired_height = visible as u16 + 2; // +2 = borders

    // Float the popup inside the result panel instead of anchoring it
    // flush under the editor: that old anchor sat exactly on the
    // panel's own top border row, fusing the two frames into a single
    // run of glyphs. One row down and one column in from the panel's
    // top-left keeps the popup's frame fully inside the panel's, with
    // the panel's own border still visible around it.
    let base_x = body_area.x;
    let base_y = editor_area.y + editor_area.height;
    let floated_x = base_x + 1;
    let floated_y = base_y + 1;
    // Leave the panel's own right border column visible too, mirroring
    // the one-column margin `floated_x` already keeps on the left —
    // without the extra `-1` the popup's right edge lands exactly on
    // that border column and overwrites it, the same fusion bug this
    // fix exists to avoid.
    let avail_width_floated = (body_area.x + body_area.width)
        .saturating_sub(1)
        .saturating_sub(floated_x);
    let avail_height_floated = (body_area.y + body_area.height).saturating_sub(floated_y);

    // Only float when at least 3 rows remain inside the panel (a top
    // border, one candidate row, and a bottom border). When the panel
    // is too short — e.g. a very small terminal — fall back to the old
    // anchor rather than not drawing the popup at all.
    let (anchor_x, anchor_y, avail_w, avail_h) = if avail_height_floated >= 3 {
        (
            floated_x,
            floated_y,
            avail_width_floated,
            avail_height_floated,
        )
    } else {
        (base_x, base_y, body_area.width, body_area.height)
    };

    // Clamp to the room actually available, shrinking below the usual
    // 20-column floor if the panel genuinely doesn't have that much.
    let width = desired_width.max(20).min(avail_w);
    let height = desired_height.min(avail_h);
    if height < 3 || width < 3 {
        return;
    }

    // Scroll to keep the selected row centred. When nothing's selected
    // (we expanded a common prefix or just showed the list), show the
    // top of the candidate list.
    let focus_idx = cycle.selected.unwrap_or(0);
    let scroll = if total <= VISIBLE {
        0
    } else if focus_idx >= total - VISIBLE / 2 {
        total - VISIBLE
    } else {
        focus_idx.saturating_sub(VISIBLE / 2)
    };

    let popup = Rect {
        x: anchor_x,
        y: anchor_y,
        width,
        height,
    };

    let label_style = Style::default().fg(theme.text);
    let kind_style = Style::default().fg(theme.muted);
    let focus_style = Style::default()
        .fg(theme.text)
        .bg(theme.row_selected_bg)
        .add_modifier(Modifier::BOLD);
    // Style for the prefix portion — bolded so the operator sees at a
    // glance what they've already typed vs. what auto-completion would
    // add. Inherits the row's bg when the row is focused.
    let prefix_style = Style::default()
        .fg(theme.title)
        .add_modifier(Modifier::BOLD);
    let prefix_focus_style = focus_style.fg(theme.title);

    // The bolded "head" portion must visually match what's already in
    // the buffer — i.e. the operator's typed/expanded case — so slice
    // directly from `editor.buffer[cycle.start..cycle.end)` rather than
    // taking the first N chars of the candidate's `display` (which is
    // always in the cache's case). Avoids the surprise where the
    // operator typed `T_` and Tab-expanded to `T_USER_`, but the popup
    // bolds `t_user_` (lowercase) on each row.
    //
    // The cycle's [start..end) is always the live prefix (LCP-expanded,
    // single-match-inserted, or narrowed-via-typing — refresh_completion
    // / editor_complete maintain this invariant). When the slice isn't
    // on a char boundary (shouldn't happen in practice, but defensive
    // here), .get returns None and we fall back to no highlighting.
    let typed_head: String = app
        .editor
        .buffer
        .get(cycle.start..cycle.end)
        .unwrap_or("")
        .to_string();
    let typed_head_char_count = typed_head.chars().count();

    let inner_w = popup.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::with_capacity(visible);
    for (i, cand) in cycle
        .candidates
        .iter()
        .enumerate()
        .skip(scroll)
        .take(VISIBLE)
    {
        // Only highlight a row when the operator has actually picked
        // one (after the second Tab). Before that, render all rows
        // neutrally — the popup is informational.
        let is_focus = cycle.selected == Some(i);
        let marker = if is_focus { "▶ " } else { "  " };
        let display = &cand.display;
        let display_chars: Vec<char> = display.chars().collect();
        // Skip the first `typed_head_char_count` chars of display when
        // composing the tail — those are the prefix we'll render from
        // the buffer slice instead. Clamp to the candidate length to
        // avoid skipping past the end (rare: short candidate, long
        // prefix — possible in test fixtures).
        let skip_n = typed_head_char_count.min(display_chars.len());
        let tail: String = display_chars[skip_n..].iter().collect();
        let kind_full = tail_of(cand);
        let kind_short = format!(" ({})", cand.kind.label());
        let fitted = fit_completion_row(
            marker.chars().count(),
            &typed_head,
            &tail,
            &kind_full,
            &kind_short,
            inner_w,
        );
        let (l_style, k_style, p_style) = if is_focus {
            (focus_style, focus_style, prefix_focus_style)
        } else {
            (label_style, kind_style, prefix_style)
        };
        // Style the `…` truncation marker like the results grid does
        // (`ui/results.rs`): accent colour, bold — while preserving
        // the focused row's selection background so the marker
        // doesn't read as a gap in the highlight.
        let mut ellipsis_style = Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD);
        if is_focus {
            ellipsis_style = ellipsis_style.bg(theme.row_selected_bg);
        }
        lines.push(Line::from(vec![
            Span::styled(marker.to_string(), l_style),
            Span::styled(fitted.head, p_style),
            Span::styled(fitted.tail, l_style),
            Span::styled(fitted.ellipsis, ellipsis_style),
            Span::styled(" ".repeat(fitted.pad), l_style),
            Span::styled(fitted.kind, k_style),
        ]));
    }

    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border_active))
                .title(Span::styled(title, Style::default().fg(theme.title))),
        ),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_completion_row_fits_unchanged_when_there_is_room() {
        let marker_w = 2;
        let head = "au";
        let tail = "th";
        let kind_full = " (table)";
        let kind_short = " (table)";
        let label_w = head.chars().count() + tail.chars().count();
        let kind_w = kind_full.chars().count();
        let slack = 6;
        let inner_w = marker_w + label_w + kind_w + slack;

        let fitted = fit_completion_row(marker_w, head, tail, kind_full, kind_short, inner_w);

        assert_eq!(fitted.head, head);
        assert_eq!(fitted.tail, tail);
        assert_eq!(fitted.ellipsis, "");
        assert_eq!(fitted.kind, kind_full);
        assert_eq!(fitted.pad, slack);
    }

    #[test]
    fn fit_completion_row_drops_context_before_shortening_the_label() {
        let marker_w = 2;
        let head = "us";
        let tail = "ers";
        // Deliberately much longer than `kind_short` so the two
        // don't both fit — the row must fall back to `kind_short`
        // (dropping just the context) rather than touching the label.
        let kind_full = " (table · a_very_long_schema_name_that_forces_a_drop)";
        let kind_short = " (table)";
        assert!(kind_full.chars().count() > kind_short.chars().count());

        let label_w = head.chars().count() + tail.chars().count();
        // Exactly enough room for the label + the *short* annotation,
        // but not the full one.
        let inner_w = marker_w + label_w + kind_short.chars().count();

        let fitted = fit_completion_row(marker_w, head, tail, kind_full, kind_short, inner_w);

        assert_eq!(fitted.head, head, "label must stay whole");
        assert_eq!(fitted.tail, tail, "label must stay whole");
        assert_eq!(fitted.ellipsis, "", "label wasn't touched, so no ellipsis");
        assert_eq!(fitted.kind, kind_short, "context dropped, kind kept");
        assert_eq!(fitted.pad, 0);
    }

    #[test]
    fn fit_completion_row_drops_kind_entirely_when_even_that_does_not_fit() {
        let marker_w = 2;
        let head = "us";
        let tail = "ers";
        let kind_full = " (table · public)";
        let kind_short = " (table)";
        let label_w = head.chars().count() + tail.chars().count();
        // Room for the label alone, but not for any annotation at all.
        let inner_w = marker_w + label_w;

        let fitted = fit_completion_row(marker_w, head, tail, kind_full, kind_short, inner_w);

        assert_eq!(fitted.head, head);
        assert_eq!(fitted.tail, tail);
        assert_eq!(fitted.ellipsis, "");
        assert_eq!(fitted.kind, "", "no room for any annotation");
        assert_eq!(fitted.pad, 0);
    }

    #[test]
    fn fit_completion_row_ellipsises_the_label_at_the_exact_inner_width() {
        let marker_w = 2;
        let head = "au";
        let tail = "thentication_audit_log_entries";
        let kind_full = " (table)";
        let kind_short = " (table)";
        // Too small even for the bare label — must fall through to
        // ellipsising it via `grid::truncate_cell_parts`.
        let inner_w = 10;

        let fitted = fit_completion_row(marker_w, head, tail, kind_full, kind_short, inner_w);

        assert_eq!(fitted.ellipsis, "…");
        assert_eq!(fitted.kind, "", "no room left for an annotation");
        // The row must land on exactly `inner_w` columns: marker +
        // kept label chars + the ellipsis marker, no leftover pad.
        let shown = marker_w + fitted.head.chars().count() + fitted.tail.chars().count() + 1;
        assert_eq!(shown, inner_w);
        assert_eq!(fitted.pad, 0);
    }

    #[test]
    fn fit_completion_row_below_four_columns_still_yields_something_non_empty() {
        // `draw_completion_popup` never calls this with an `inner_w`
        // narrower than 1 (it bails out before rendering once the
        // popup itself would drop under 3 columns), but 3 — one below
        // the round number 4 — is the realistic floor: room for the
        // 2-column marker plus exactly one more column. That one
        // column must still carry something (the truncation `…`),
        // not render as blank.
        let marker_w = 2;
        let head = "au";
        let tail = "thentication_audit_log_entries";
        let kind_full = " (table)";
        let kind_short = " (table)";
        let inner_w = 3;

        let fitted = fit_completion_row(marker_w, head, tail, kind_full, kind_short, inner_w);
        let rendered: String = format!("{}{}{}", fitted.head, fitted.tail, fitted.ellipsis);

        assert!(
            !rendered.is_empty(),
            "inner_w=3 produced no visible content"
        );
        assert_eq!(fitted.ellipsis, "…");
    }
}
