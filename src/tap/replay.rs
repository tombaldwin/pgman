//! Replay — feed a captured event stream from a file into
//! the same pipeline as the live listeners. Lets pgman be
//! demoed and downstream layers (L3 advisor, L4 evidence,
//! L6 index advisor) be developed before the JVM-side JAR
//! exists or against deterministic fixture data.
//!
//! Split from `tap/mod.rs` for code-health; the public
//! functions are re-exported.

use super::{now_unix_micros, parse, TapEvent};

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
    use tokio::io::AsyncBufReadExt;
    let file = tokio::fs::File::open(&path).await?;
    let reader = tokio::io::BufReader::new(file);
    let mut lines = reader.lines();
    let mut accepted = 0usize;
    let mut skipped = 0usize;
    let mut line_no = 0usize;
    while let Some(line) = lines.next_line().await? {
        line_no += 1;
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
                tracing::warn!("tap-replay: line {line_no}: {e}");
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
