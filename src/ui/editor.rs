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
    let inner_width = (label_width + tail_width + 4) as u16;
    let width = inner_width.min(body_area.width).max(20);

    // Show at most VISIBLE rows; auto-scroll keeps the active row in view.
    const VISIBLE: usize = 8;
    let total = cycle.candidates.len();
    let visible = total.min(VISIBLE);
    let height = (visible as u16 + 2).min(body_area.height); // +2 = borders
    if height < 3 {
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

    // Anchor flush under the editor's bottom border, left-aligned.
    let popup = Rect {
        x: body_area.x,
        y: editor_area.y + editor_area.height,
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
        let head = typed_head.clone();
        let tail: String = display_chars[skip_n..].iter().collect();
        let row_display_chars = typed_head_char_count + (display_chars.len() - skip_n);
        let kind_text = tail_of(cand);
        let kind_w = kind_text.chars().count();
        let body_w = marker.chars().count() + row_display_chars;
        let pad_after = inner_w.saturating_sub(body_w).saturating_sub(kind_w);
        let pad = " ".repeat(pad_after);
        let (l_style, k_style, p_style) = if is_focus {
            (focus_style, focus_style, prefix_focus_style)
        } else {
            (label_style, kind_style, prefix_style)
        };
        lines.push(Line::from(vec![
            Span::styled(marker.to_string(), l_style),
            Span::styled(head, p_style),
            Span::styled(tail, l_style),
            Span::styled(pad, l_style),
            Span::styled(kind_text, k_style),
        ]));
    }

    let title = match cycle.selected {
        Some(i) => format!(" {}/{} ", i + 1, total),
        None => format!(" {} matches · Tab to pick ", total),
    };
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
