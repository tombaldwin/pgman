//! Replay — feed a captured event stream from a file into
//! the same pipeline as the live listeners. Lets pgman be
//! demoed and downstream layers (L3 advisor, L4 evidence,
//! L6 index advisor) be developed before the JVM-side JAR
//! exists or against deterministic fixture data.
//!
//! Split from `tap/mod.rs` for code-health; the public
//! functions are re-exported.

use super::{now_unix_micros, parse, TapEvent, WarnThrottle, TAP_MAX_FRAME_BYTES};

/// Cap on one JSONL line read from a `--tap-replay` file, in
/// bytes. Matches [`TAP_MAX_FRAME_BYTES`] — a captured event
/// can never be larger than that on the way in (every live
/// transport enforces it, and [`record_line`] round-trips the
/// same [`TapEvent`]), so a line past this size in a replay
/// file is either corrupt or hostile, not a legitimate capture.
/// `tokio::io::AsyncBufReadExt::lines()` has no such cap — its
/// internal `String` grows until it finds `\n` or EOF, so one
/// mangled or adversarial line could otherwise pull an
/// unbounded allocation.
pub const TAP_REPLAY_MAX_LINE_BYTES: usize = TAP_MAX_FRAME_BYTES;

static OVERLONG_LINE_WARN: WarnThrottle = WarnThrottle::new();
static MALFORMED_LINE_WARN: WarnThrottle = WarnThrottle::new();

/// Parse one line of a JSONL capture into a [`TapEvent`].
/// Blank lines are silently skipped at the caller; lines that
/// don't validate as a `TapEvent` return their parse error so
/// the caller can log a useful pointer to the bad line.
#[must_use = "parse_replay_line returns the parsed event — discarding it loses replay data"]
pub fn parse_replay_line(line: &str) -> Option<Result<TapEvent, String>> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(parse(trimmed.as_bytes()))
}

/// Serialize one event as JSON suitable for appending to a
/// `--tap-record` capture file. `received_at_unix_micros`
/// is skipped (it's `#[serde(skip)]` on the struct) so a
/// captured file is portable across hosts with skewed
/// clocks — the replay side re-stamps on receive.
#[must_use = "record_line returns the serialised JSON — caller must write it"]
pub fn record_line(event: &TapEvent) -> Result<String, String> {
    serde_json::to_string(event).map_err(|e| format!("serialize failed: {e}"))
}

/// Stream `path`'s JSONL events into `tx`. Each line is one
/// `TapEvent`; blank lines skipped, malformed lines logged
/// + dropped (the replay continues so one bad line doesn't
///   take out the demo).
///
/// `received_at_unix_micros` is stamped at replay time so the
/// downstream pipeline can't tell a replayed event from a
/// live one — useful for exercising L2 baseline diff / L3
/// advisor without seeding fake timestamps.
pub async fn run_replay_file<P: AsRef<std::path::Path>>(
    path: P,
    tx: tokio::sync::mpsc::Sender<TapEvent>,
) -> std::io::Result<usize> {
    let file = tokio::fs::File::open(&path).await?;
    let mut reader = tokio::io::BufReader::new(file);
    let mut accepted = 0usize;
    let mut skipped = 0usize;
    let mut line_no = 0usize;
    while let Some(outcome) = read_capped_line(&mut reader, TAP_REPLAY_MAX_LINE_BYTES).await? {
        line_no += 1;
        let line = match outcome {
            LineOutcome::Overlong(len) => {
                OVERLONG_LINE_WARN.warn(|suppressed| {
                    let suffix = if suppressed > 0 {
                        format!(" ({suppressed} more suppressed in the last second)")
                    } else {
                        String::new()
                    };
                    format!(
                        "tap-replay: line {line_no}: {len} bytes exceeds cap \
                         {TAP_REPLAY_MAX_LINE_BYTES}, skipped{suffix}"
                    )
                });
                skipped += 1;
                continue;
            }
            LineOutcome::Line(s) => s,
        };
        match parse_replay_line(&line) {
            None => {}
            Some(Ok(mut event)) => {
                event.received_at_unix_micros = now_unix_micros();
                // Use `.send().await` for replay specifically:
                // backpressure here means the operator wants
                // the events in the App, so blocking the file
                // pump until the channel has room is more
                // useful than dropping replayed events.
                if tx.send(event).await.is_err() {
                    break; // receiver gone
                }
                accepted += 1;
            }
            Some(Err(e)) => {
                MALFORMED_LINE_WARN.warn(|suppressed| {
                    let suffix = if suppressed > 0 {
                        format!(" ({suppressed} more suppressed in the last second)")
                    } else {
                        String::new()
                    };
                    format!("tap-replay: line {line_no}: {e}{suffix}")
                });
                skipped += 1;
            }
        }
    }
    tracing::info!(
        "tap-replay: {accepted} event(s) accepted, {skipped} line(s) skipped, from {}",
        path.as_ref().display()
    );
    Ok(accepted)
}

/// Outcome of one [`read_capped_line`] call.
enum LineOutcome {
    /// A complete line within the cap, newline stripped.
    Line(String),
    /// A line whose byte length exceeded the cap. The stream
    /// has already been advanced past its trailing `\n` (or to
    /// EOF if it had none), so the caller can just skip it and
    /// keep reading — the `usize` is the line's true length,
    /// for the warn message.
    Overlong(usize),
}

/// Read one `\n`-delimited line from `reader` without ever
/// buffering more than `max_len` bytes of it, regardless of how
/// long the actual line on disk is. Unlike
/// `AsyncBufReadExt::lines()` / `read_line()` — which grow
/// their buffer until they find the delimiter — this drains and
/// discards anything past `max_len` as it's read, so a replay
/// file with one absurdly long line can't blow up memory before
/// the caller gets a chance to react.
///
/// Returns `Ok(None)` on clean EOF with nothing left to read.
/// A trailing line with no final `\n` before EOF is still
/// returned (matches `AsyncBufReadExt::lines()` behaviour).
async fn read_capped_line<R>(reader: &mut R, max_len: usize) -> std::io::Result<Option<LineOutcome>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    let mut collected: Vec<u8> = Vec::new();
    let mut total_len = 0usize;
    let mut saw_any = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if !saw_any {
                return Ok(None); // clean EOF, no partial line pending
            }
            break; // EOF mid-line (no trailing newline) — return what we have
        }
        saw_any = true;
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            if collected.len() < max_len {
                let take = pos.min(max_len - collected.len());
                collected.extend_from_slice(&available[..take]);
            }
            total_len += pos;
            reader.consume(pos + 1);
            break;
        }
        let n = available.len();
        if collected.len() < max_len {
            let take = (max_len - collected.len()).min(n);
            collected.extend_from_slice(&available[..take]);
        }
        total_len += n;
        reader.consume(n);
    }
    if total_len > max_len {
        return Ok(Some(LineOutcome::Overlong(total_len)));
    }
    // The replay format is JSON, always valid UTF-8 for any
    // line pgman itself wrote; `from_utf8_lossy` is a defensive
    // fallback for a hand-edited/corrupt file rather than the
    // expected path. A trailing `\r` (CRLF-saved file) is left
    // in place — `parse_replay_line`'s `.trim()` strips it, the
    // same as it always has.
    Ok(Some(LineOutcome::Line(
        String::from_utf8_lossy(&collected).into_owned(),
    )))
}
