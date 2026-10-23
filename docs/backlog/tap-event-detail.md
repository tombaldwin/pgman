# Tap List: the rest of the focused event

Design note for the BACKLOG item "Tap List detail: the rest of the
focused event". Class: behaviour, but it touches the tap module's UI,
the app's key handling and nav state, the footer and the docs, so it
gets a note.

## What is missing

The List view shows one row per event and, under the list, the focused
query's bound parameters over two lines. Stored on every event and
shown nowhere for one event:

- the `error` cause chain, root cause last (a failed row is only
  coloured red; the one-line join reaches the reports, not the panel);
- the whole `caller` stack (only `caller[0]` reaches the Callers view);
- `pool`, `conn` and `txn`;
- and the SQL beyond what the row's width allows.

## Options

1. **Grow the strip under the list.** It has two lines, and the panel at
   80x24 has about ten rows for the list. The ingest caps allow 64
   causes of 1 KiB each and 64 frames; a realistic Spring failure is a
   three-to-five-deep chain of 100-300 character messages, and a caller
   stack is five to fifteen frames. Two lines hold one of those at best,
   and taking more rows starves the list it describes.
2. **A focused-event overlay**, opened with a plain key from the List
   view. Everything above fits, scrolled when it doesn't, and the list
   keeps its height.

The overlay wins: the strip can't carry the content at any budget that
leaves a list. The strip stays as it is: parameters at a glance while
moving the cursor is still worth having.

## Shape

- **Key:** `Enter` on a List row opens it; `Esc`, `q` or `Enter` close
  it, back to the list with the cursor where it was. `j/k`, `↑↓`,
  `PgUp/PgDn` and `g/G` scroll. Plain keys only, no F-key or Alt chord
  (Mac / iTerm guidance). `Enter` does nothing in the List view today.
- **Snapshot, not a live view:** opening it clones the focused event. A
  full ring evicts one event per arrival and the cursor follows the
  eviction, so an overlay reading `tap_events[cursor]` would change
  under the reader at line rate.
- **State:** `TapNavUi::event_detail: Option<TapEventDetail>` (the
  event and a scroll offset), not a new `Mode`: it is a layer of the
  List view, and the monitor's other keys (`v`, `c`, `s`) are held off
  while it is open. Reset when the monitor opens and when the view
  cycles.
- **Content**, one labelled field per row, values wrapped under a
  hanging indent: kind; started (`ts_unix_micros`, UTC); duration;
  rows; app; pool; conn; txn; sql (whole, its line breaks kept);
  params (one per line, the redaction label as in the strip); error,
  then `caused by` for each further cause, the last marked `root
  cause`; caller frames, innermost first. Fields the producer did not
  send read `—`, so "not sent" is visible rather than a missing line.
  Built by a pure function, so the wording is pinned by unit tests and
  the layout by render tests.
- **Discoverability:** the footer says `enter detail` in the List view,
  and the overlay's footer gives its own keys.
