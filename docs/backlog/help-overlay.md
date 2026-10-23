# Design note: a sectioned help overlay

Status: decided 2026-10-23, for the "help overlay is one page" item
(BACKLOG.md, "From the Fable panel, tolerated for 0.1.0").

## The problem

`?` / `F1` opened one scrolling page: 22 sections, about 275 wrapped
rows at 80 columns, `↓ 260 more below` on open. Opening it from a mode
scrolled to that mode's section, but the anchor pass put the heading on
the row the `↑ N more above` marker covers, so the section opened
without its name (fixed separately, first). `:help <topic>` knew 12 of
the 22 sections, had no Tab completion, and its error listed only those
12. At 200x50 the footer showed whatever status was left over from
before help opened, not the overlay's keys.

## The shape

The overlay becomes a **section list plus one section**. The content is
not rewritten: `ui::help_body` still builds every row, and its anchor
map (heading → row) is what splits the rows into sections. A section is
the rows from its heading up to the next heading, trailing blank rows
dropped.

- **Wide (body ≥ 110 columns)**: the list is a left column, the selected
  section fills the right. j/k scroll the section; Tab / Shift-Tab and
  `]` / `[` move to the next / previous section; Esc closes.
- **Narrow (< 110 columns)**: two levels. Level 1 is the list, each
  section with a one-line teaser; j/k move the selection, Enter opens
  the section full width. In a section Esc goes back to the list, and
  Esc from the list closes. Tab / `]` / `[` work at both levels.
- `?`, `q` and `F1` close from anywhere in the overlay, as before.
- `g` / `G` are top / bottom of the section (first / last entry in the
  list); PageUp / PageDown move by 10.

Opening from a mode selects that mode's section and shows it (heading
on row 0). Opening from the grid, where there is no section to pick,
opens the list.

### Filter

`/` starts a live filter across **all** sections: matching rows are
shown under their section headings, as the schema browser's filter shows
matching tables under their schemas. It is its own mode,
`Mode::HelpFilter`, beside `Mode::SchemaBrowserFilter`, so every global
plain-key binding (`?`, `!`, `:`, `]`, digits) is already off while the
operator types: `is_text_input` covers them. Enter keeps the filter and
returns to scrolling it; Esc clears it. With a filter up, Tab / `]` /
`[` jump between its section groups.

### `:help`

- `:help <topic>` matches a section by name, case-insensitively and
  ignoring the `:` of `: commands`: an exact name, then the old
  one-word aliases (`schema`, `saved`, `tap`, …, so nothing that worked
  stops working), then the first section whose name starts with the
  topic, then the first with a word that does.
- Tab after `:help ` completes against every section name, through the
  same `:` completion that completes command names.
- An unknown topic lists every section.
- `:help all` keeps the old flat page, scrollable as before, with `]` /
  `[` jumping heading to heading.

### Footer

In `Mode::Help` the footer shows the overlay's own keys for the view
it is in, never a status left from before (the status branch skips
`Mode::Help`, as it already skips `Mode::Confirm`). An error still
wins: it is news.

## Where things live

- `ui::HELP_SECTIONS`: the ordered section titles with their teasers,
  next to `help_body`. A test pins that `help_body`'s headings are
  exactly this list, in this order, so the two cannot drift. The app
  side resolves topics and mode anchors against it without building
  the body.
- `ui/help.rs`: the overlay's renderer and its pure parts — the section
  split, the filter, the heading jump, the split decision.
- `app/keys.rs`: `on_help_key` / `on_help_filter_key`; state in
  `HelpUi` (selected section, view, filter, plus the per-frame values
  the renderer writes back: `max_scroll`, whether it drew split, the
  heading rows of a flat view).
- `app/cmd.rs`: topic resolution and `:help` completion.

`HelpUi::origin` (an anchor the renderer resolved on the first draw)
goes: the section is resolved when help opens, so there is no deferred
anchor pass left to get wrong.

## Rejected

- **Collapsible sections in the one page.** Still one long page to
  scroll through, and the anchor arithmetic stays.
- **Rewriting the help content into structured data.** Larger diff,
  every row touched, for no visible gain; the anchor map already
  carries the structure.
- **A bool for "filter typing" inside `Mode::Help`.** Every global
  plain-key handler would need to learn about it; a mode gets
  `is_text_input` for free.
