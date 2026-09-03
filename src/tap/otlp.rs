//! OTLP — peer ingest from the OpenTelemetry Java agent.
//!
//! The OTel JDBC instrumentation emits one span per JDBC call
//! with attributes like `db.system=postgresql`, `db.statement`,
//! `db.operation`. We accept those spans on a peer HTTP
//! endpoint (default port 4318) and map them onto `TapEvent`
//! so OTel-equipped JVM shops see live queries in pgman
//! without installing pgman-tap.
//!
//! v1 supports OTLP/HTTP JSON only (`Content-Type:
//! application/json`). The protobuf variant is the wire
//! default for production OTel pipelines but most Java agents
//! also accept `OTEL_EXPORTER_OTLP_PROTOCOL=http/json`. We can
//! add protobuf later if a real user asks.
//!
//! Split from `tap/mod.rs` for code-health; the public
//! functions are re-exported.

use super::{
    enforce_field_caps, forward_or_drop, now_unix_micros, validate_required, TapEvent, TapKind,
    WarnThrottle, PROTOCOL_VERSION,
};

static OTLP_CONN_LIMIT_WARN: WarnThrottle = WarnThrottle::new();
static OTLP_ACCEPT_WARN: WarnThrottle = WarnThrottle::new();
static DURATION_CLAMP_WARN: WarnThrottle = WarnThrottle::new();

/// Sanity cap on OTLP-derived `duration_micros`: 1 hour.
/// Anything beyond this is broken telemetry (clock skew /
/// hostile span / bug). Capping prevents one such span
/// from hijacking the `TotalTime` sort via saturating
/// arithmetic in [`super::group_hotspots`].
pub const OTLP_DURATION_CAP_MICROS: u64 = 3_600_000_000;

/// Attribute key the OTel semantic conventions use to mark a
/// span as a database call (always `"db.system"`, value
/// `"postgresql"` for our case).
pub const OTEL_DB_SYSTEM_KEY: &str = "db.system";
pub const OTEL_DB_SYSTEM_POSTGRES: &str = "postgresql";
pub const OTEL_DB_STATEMENT_KEY: &str = "db.statement";
pub const OTEL_DB_OPERATION_KEY: &str = "db.operation";
pub const OTEL_SERVICE_NAME_KEY: &str = "service.name";

/// Parse one OTLP/HTTP JSON body
/// (an `ExportTraceServiceRequest`) and emit one
/// [`TapEvent`] per spans whose `db.system=postgresql`. Spans
/// that don't look like Postgres calls (no `db.system`, or a
/// different system, or no `db.statement`) are silently
/// skipped — the caller's HTTP response should still be 200
/// because OTLP semantics treat un-acked spans as the
/// receiver's choice.
///
/// Returns the parsed events and a count of spans skipped so
/// the listener can log a one-line summary instead of
/// per-span chatter.
#[must_use = "parse_otlp_json returns events — discarding the Vec loses them"]
pub fn parse_otlp_json(body: &[u8]) -> Result<(Vec<TapEvent>, usize), String> {
    let s = std::str::from_utf8(body).map_err(|e| format!("not utf-8: {e}"))?;
    let root: serde_json::Value = serde_json::from_str(s).map_err(|e| format!("bad json: {e}"))?;
    let mut events: Vec<TapEvent> = Vec::new();
    let mut skipped = 0usize;
    let Some(resource_spans) = root.get("resourceSpans").and_then(|v| v.as_array()) else {
        return Ok((events, 0));
    };
    for rs in resource_spans {
        // service.name lives on the resource and applies to
        // every span inside this resourceSpans bundle.
        let service_name = rs
            .get("resource")
            .and_then(|r| r.get("attributes"))
            .and_then(|a| a.as_array())
            .and_then(|attrs| otlp_attr_string(attrs, OTEL_SERVICE_NAME_KEY));
        let Some(scope_spans) = rs.get("scopeSpans").and_then(|v| v.as_array()) else {
            continue;
        };
        for ss in scope_spans {
            let Some(spans) = ss.get("spans").and_then(|v| v.as_array()) else {
                continue;
            };
            for span in spans {
                match span_to_tap_event(span, service_name.as_deref()) {
                    Some(event) => {
                        // Defensive: enforce the same invariants
                        // `parse` does for tap-protocol frames.
                        // span_to_tap_event always produces a
                        // valid Query event today, but anyone
                        // extending it later won't silently leak
                        // an invalid one past the contract.
                        if validate_required(&event).is_ok() {
                            events.push(event);
                        } else {
                            skipped += 1;
                        }
                    }
                    None => skipped += 1,
                }
            }
        }
    }
    Ok((events, skipped))
}

pub fn span_to_tap_event(span: &serde_json::Value, service_name: Option<&str>) -> Option<TapEvent> {
    let attrs = span.get("attributes").and_then(|v| v.as_array())?;
    // Filter for Postgres-flavoured DB spans. OTel's
    // db.system is "postgresql" for both vanilla Postgres
    // and Aurora; Redshift uses "redshift" so we skip those.
    let system = otlp_attr_string(attrs, OTEL_DB_SYSTEM_KEY)?;
    if system != OTEL_DB_SYSTEM_POSTGRES {
        return None;
    }
    // db.statement is the only field that actually carries
    // the SQL; spans without it (e.g. connection open/close)
    // aren't useful for the tap.
    let sql = otlp_attr_string(attrs, OTEL_DB_STATEMENT_KEY)?;
    // Duration: end - start in nanoseconds → microseconds.
    // The values are protobuf uint64, which JSON encodes as a
    // string per the protobuf-JSON mapping — but some
    // implementations (including OTel collector test fixtures)
    // emit numbers, so accept either.
    let start_ns = otlp_unix_nano(span.get("startTimeUnixNano"))?;
    let end_ns = otlp_unix_nano(span.get("endTimeUnixNano"))?;
    // Cap at 1 hour. A malicious or buggy agent shipping
    // `endTimeUnixNano: u64::MAX` would otherwise hand us a
    // saturating-add monster that hijacks the TotalTime sort.
    // Real queries that take an hour are operator news long
    // before they reach the panel — clamping is safe.
    let raw_micros = end_ns.saturating_sub(start_ns) / 1_000;
    let duration_micros = if raw_micros > OTLP_DURATION_CAP_MICROS {
        // Surface the clamp so an operator with genuine
        // long-running analytical queries can distinguish
        // them from hostile / broken telemetry. Single-line
        // log; the listener's debug-summary line would
        // otherwise hide this entirely.
        DURATION_CLAMP_WARN.warn(|suppressed| {
            let suffix = if suppressed > 0 {
                format!(" ({suppressed} more suppressed in the last second)")
            } else {
                String::new()
            };
            format!(
                "tap-otlp: clamped duration {raw_micros}µs to cap {OTLP_DURATION_CAP_MICROS}µs{suffix}"
            )
        });
        OTLP_DURATION_CAP_MICROS
    } else {
        raw_micros
    };
    // Derive the absolute end timestamp from start + the CAPPED duration, not
    // the raw `end_ns`. A hostile/clock-skewed `endTimeUnixNano` near u64::MAX
    // would otherwise store a year-584942 timestamp that poisons the N+1
    // panel's span window (last.ts - first.ts) and any absolute-time render.
    let ts_unix_micros = (start_ns / 1_000).saturating_add(duration_micros);
    // Status: 2 == ERROR per OTel protocol. Code 1 == OK,
    // 0 == UNSET. Treat 2 as a query error.
    let status_code = span
        .get("status")
        .and_then(|s| s.get("code"))
        .and_then(|c| c.as_u64())
        .unwrap_or(0);
    let error = if status_code == 2 {
        // OTel usually carries the message on the status
        // object; fall back to a generic marker so the
        // renderer flags it even when the agent didn't ship a
        // message.
        let msg = span
            .get("status")
            .and_then(|s| s.get("message"))
            .and_then(|m| m.as_str())
            .map(String::from)
            .unwrap_or_else(|| "OTLP span reported ERROR".into());
        Some(vec![msg])
    } else {
        None
    };
    let mut event = TapEvent {
        v: PROTOCOL_VERSION,
        kind: TapKind::Query,
        ts_unix_micros,
        // Listener stamps this after parse; leave zero here.
        received_at_unix_micros: 0,
        app: service_name.map(String::from),
        // OTel JDBC doesn't expose pool / conn / txn — those
        // are pgman-tap's added value. Leave None.
        pool: None,
        conn: None,
        txn: None,
        sql: Some(sql),
        // OTel typically strips bound parameters for PII
        // safety; we flag that on the event so the renderer
        // can surface "values redacted by source."
        params: None,
        params_redacted: true,
        duration_micros: Some(duration_micros),
        rows: None,
        error,
        caller: None,
        dropped_events_total: None,
        txn_outcome: None,
    };
    // `parse` (the TCP/UDP/replay path) truncates every
    // string-shaped field at ingest — do the same here so an
    // OTLP `db.statement` / status message can't hand pgman an
    // unbounded string the tap-protocol path would have capped.
    enforce_field_caps(&mut event);
    Some(event)
}

/// Look up a string-valued attribute by key in an OTLP
/// attribute list. Attribute values are tagged unions
/// (`stringValue`, `intValue`, `doubleValue`, ...); we only
/// care about `stringValue` for the keys we map.
///
/// A single malformed attribute (missing `"key"`, non-string
/// key, array/object value) does not abort the search — the
/// loop just skips it. The earlier version used `?` which
/// short-circuited the whole function and made e.g. a
/// `service.namespace` attribute landing before `service.name`
/// hide the entire span from pgman.
pub fn otlp_attr_string(attrs: &[serde_json::Value], key: &str) -> Option<String> {
    for attr in attrs {
        let Some(k) = attr.get("key").and_then(|v| v.as_str()) else {
            continue;
        };
        if k == key {
            return attr
                .get("value")
                .and_then(|v| v.get("stringValue"))
                .and_then(|v| v.as_str())
                .map(String::from);
        }
    }
    None
}

/// Decode an OTLP uint64-as-JSON value. Per the protobuf-JSON
/// mapping uint64 should be a string, but some emitters use
/// numbers. Accept both.
pub fn otlp_unix_nano(v: Option<&serde_json::Value>) -> Option<u64> {
    let v = v?;
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    if let Some(s) = v.as_str() {
        return s.parse::<u64>().ok();
    }
    None
}

// ---------------------------------------------------------
// OTLP/HTTP server — accepts POST /v1/traces, feeds the
// receive pipeline.
// ---------------------------------------------------------

/// Maximum OTLP HTTP body size we'll accept. 4 MiB — well
/// above any reasonable single OTLP batch. Bigger would
/// suggest a misbehaving agent or a hostile client trying to
/// exhaust memory; at the old 16 MiB cap, 100 concurrent
/// uploads could hold ~1.6 GB before the connection cap
/// ([`super::listener::TAP_MAX_CONCURRENT_CONNS`]) existed to
/// bound "concurrent" in the first place. With both caps in
/// place the worst case is bounded connections × this cap.
pub const OTLP_MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Spawn an OTLP/HTTP listener on `addr`. Accepts only
/// `POST /v1/traces` with `Content-Type: application/json`;
/// other methods/paths get the standard HTTP error response
/// so a curl-poking operator sees a useful message. Returns
/// only on socket-bind failure; per-connection errors are
/// logged + dropped so other clients keep flowing.
pub async fn run_otlp_listener(
    addr: std::net::SocketAddr,
    tx: tokio::sync::mpsc::Sender<TapEvent>,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("tap: OTLP/HTTP listener bound on {addr}");
    let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(
        super::listener::TAP_MAX_CONCURRENT_CONNS,
    ));
    loop {
        let (sock, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                OTLP_ACCEPT_WARN.warn(|suppressed| {
                    let suffix = if suppressed > 0 {
                        format!(" ({suppressed} more suppressed in the last second)")
                    } else {
                        String::new()
                    };
                    format!("tap-otlp: accept failed: {e}{suffix}")
                });
                continue;
            }
        };
        // Same connection cap as the TCP tap listener — an
        // OTLP POST can carry up to `OTLP_MAX_BODY_BYTES`, so
        // unbounded concurrency here is the same memory risk in
        // a different transport.
        let permit = match permits.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                OTLP_CONN_LIMIT_WARN.warn(|suppressed| {
                    let suffix = if suppressed > 0 {
                        format!(" ({suppressed} more rejections suppressed in the last second)")
                    } else {
                        String::new()
                    };
                    format!(
                        "tap-otlp: rejected connection from {peer}: {} concurrent connections already open{suffix}",
                        super::listener::TAP_MAX_CONCURRENT_CONNS
                    )
                });
                continue;
            }
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = handle_otlp_conn(sock, &tx).await {
                tracing::warn!("tap-otlp: conn {peer} ended: {e}");
            }
        });
    }
}

/// How long we'll wait for a complete request (headers +
/// body) on one connection. Defends against slow-loris
/// clients holding sockets open with trickle reads — at the
/// JVM agent / OTel collector tier this is generous; at the
/// "someone curl'd a malformed POST" tier it's the right cap.
pub const OTLP_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub async fn handle_otlp_conn(
    mut sock: tokio::net::TcpStream,
    tx: &tokio::sync::mpsc::Sender<TapEvent>,
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    // One request per connection in v1; OTel exporters
    // typically open + POST + close. Keep-alive is fine but
    // unnecessary at this stage.
    let req = match tokio::time::timeout(
        OTLP_REQUEST_TIMEOUT,
        read_http_request(&mut sock, OTLP_MAX_BODY_BYTES),
    )
    .await
    {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            let _ = write_http_response(&mut sock, 400, "Bad Request", &e).await;
            return Ok(());
        }
        Err(_) => {
            let msg = format!(
                "request timed out after {}s (slow-loris guard)",
                OTLP_REQUEST_TIMEOUT.as_secs()
            );
            let _ = write_http_response(&mut sock, 408, "Request Timeout", &msg).await;
            return Ok(());
        }
    };
    let response = process_otlp_request(req, tx);
    write_http_response(&mut sock, response.status, response.reason, &response.body).await?;
    sock.shutdown().await.ok();
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub reason: &'static str,
    pub body: String,
}

/// Pure: turn a parsed HTTP request into the response we
/// should send. Routes OTLP `POST /v1/traces` through
/// [`parse_otlp_json`] and forwards each event into `tx`.
/// Exposed for the unit tests that exercise routing without
/// a real socket.
pub fn process_otlp_request(
    req: HttpRequest,
    tx: &tokio::sync::mpsc::Sender<TapEvent>,
) -> HttpResponse {
    if req.method != "POST" {
        return HttpResponse {
            status: 405,
            reason: "Method Not Allowed",
            body: "OTLP/HTTP accepts only POST".into(),
        };
    }
    // Chunked-encoding bodies silently parse as empty because
    // our minimal reader honours Content-Length only. Reject
    // explicitly with 501 so the agent surfaces a real error
    // instead of "succeeded with no events accepted." Most
    // OTel exporters can switch to Content-Length easily.
    if let Some(enc) = req.headers.get("transfer-encoding") {
        if enc.to_ascii_lowercase().contains("chunked") {
            return HttpResponse {
                status: 501,
                reason: "Not Implemented",
                body: format!(
                    "Transfer-Encoding {enc:?} not supported; use Content-Length \
                     (set OTEL_EXPORTER_OTLP_PROTOCOL=http/json on the agent)"
                ),
            };
        }
    }
    // Only /v1/traces in v1. /v1/metrics + /v1/logs can come
    // later if real users need them. Strip a possible
    // trailing slash so `/v1/traces/` matches too.
    let path = req.path.trim_end_matches('/');
    if path != "/v1/traces" {
        return HttpResponse {
            status: 404,
            reason: "Not Found",
            body: format!(
                "unknown path {:?}; v1 OTLP accepts only POST /v1/traces",
                req.path
            ),
        };
    }
    // Require JSON; protobuf is a v2 follow-up.
    let ct = req
        .headers
        .get("content-type")
        .map(String::as_str)
        .unwrap_or("");
    if !ct.starts_with("application/json") {
        return HttpResponse {
            status: 415,
            reason: "Unsupported Media Type",
            body: format!(
                "expected application/json (set OTEL_EXPORTER_OTLP_PROTOCOL=http/json); got {ct:?}"
            ),
        };
    }
    let (mut events, skipped) = match parse_otlp_json(&req.body) {
        Ok(pair) => pair,
        Err(e) => {
            return HttpResponse {
                status: 400,
                reason: "Bad Request",
                body: format!("OTLP parse error: {e}"),
            };
        }
    };
    let total = events.len();
    let mut accepted = 0usize;
    let mut closed_mid_batch = false;
    for event in events.drain(..) {
        let mut event = event;
        // Stamp per span (not once for the batch) so spans
        // arriving in one POST keep a strictly-monotonic
        // received_at — preserves the FIFO-within-batch
        // ordering the downstream Hotspots / N+1 detectors
        // (and tests like
        // `tcp_listener_round_trip_decodes_events_and_stamps_received_at`)
        // implicitly assume.
        event.received_at_unix_micros = now_unix_micros();
        if forward_or_drop(tx, event, "otlp").is_err() {
            // App side has gone away. Don't return a clean
            // 200: the agent should know some spans were not
            // delivered.
            closed_mid_batch = true;
            break;
        }
        accepted += 1;
    }
    if skipped > 0 || accepted != total {
        tracing::debug!(
            "tap-otlp: accepted {accepted} / parsed {total} span(s), skipped {skipped} non-postgres"
        );
    }
    if closed_mid_batch {
        // OTLP/HTTP semantics: report partial success so the
        // agent surfaces the loss instead of treating the
        // batch as fully accepted.
        let rejected = total - accepted;
        return HttpResponse {
            status: 200,
            reason: "OK",
            body: format!(
                "{{\"partialSuccess\":{{\"rejectedSpans\":{rejected},\"errorMessage\":\"pgman App receiver closed mid-batch\"}}}}"
            ),
        };
    }
    // OTel collectors expect an empty
    // ExportTraceServiceResponse on success. `{}` satisfies
    // the protobuf-JSON encoding for "no partial_success."
    HttpResponse {
        status: 200,
        reason: "OK",
        body: "{}".into(),
    }
}

/// A parsed HTTP/1.1 request — enough to route OTLP. We
/// deliberately don't model chunked encoding or
/// `Transfer-Encoding`; OTel exporters always use
/// Content-Length for OTLP bodies.
#[derive(Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub headers: std::collections::HashMap<String, String>,
    pub body: Vec<u8>,
}

pub async fn read_http_request<R>(reader: &mut R, max_body: usize) -> Result<HttpRequest, String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    // Read headers into a chunked buffer (1 KiB at a time)
    // and scan for `\r\n\r\n`. Cap accumulated headers at
    // 16 KiB so a hostile client can't buffer-bomb us before
    // we hit the body. Chunked reads avoid the per-byte
    // syscall cost the original implementation had — the
    // slow-loris guard is a separate deadline at the call
    // site.
    const MAX_HEADER_BYTES: usize = 16 * 1024;
    const READ_CHUNK: usize = 1024;
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let mut chunk = [0u8; READ_CHUNK];
    let mut header_end: Option<usize> = None;
    while buf.len() <= MAX_HEADER_BYTES {
        let n = reader
            .read(&mut chunk)
            .await
            .map_err(|e| format!("read failed: {e}"))?;
        if n == 0 {
            return Err("connection closed before headers complete".into());
        }
        let prior_len = buf.len();
        buf.extend_from_slice(&chunk[..n]);
        // Scan only the new region (plus a 3-byte overlap so
        // a CRLFCRLF straddling the read boundary still hits).
        let scan_from = prior_len.saturating_sub(3);
        if let Some(rel) = find_subsequence(&buf[scan_from..], b"\r\n\r\n") {
            header_end = Some(scan_from + rel);
            break;
        }
    }
    let Some(end) = header_end else {
        return Err(format!(
            "header section exceeded {MAX_HEADER_BYTES} bytes without CRLF-CRLF terminator"
        ));
    };
    let header_text =
        std::str::from_utf8(&buf[..end]).map_err(|e| format!("non-utf8 in headers: {e}"))?;
    // Anything past end+4 is over-read into the body region —
    // keep it and combine with whatever we still need to read.
    let body_prefix: Vec<u8> = buf[end + 4..].to_vec();

    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().ok_or("empty request line")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().ok_or("missing method")?.to_string();
    let path = parts.next().ok_or("missing path")?.to_string();
    let _version = parts.next().unwrap_or("HTTP/1.1");
    let mut headers: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let content_length: usize = headers
        .get("content-length")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if content_length > max_body {
        return Err(format!(
            "Content-Length {content_length} exceeds cap {max_body}"
        ));
    }
    // Read the body incrementally rather than pre-allocating
    // `vec![0u8; content_length]` up front. `content_length` is
    // already capped at `max_body` above, so this isn't about
    // trusting the header less — it's about not letting a slow
    // client (Content-Length: 4 MiB, one byte every few
    // seconds) hold a full-size allocation for the duration of
    // the read on every one of up to
    // `listener::TAP_MAX_CONCURRENT_CONNS` connections at once.
    // The cap is re-checked every chunk rather than relied on
    // once at the top.
    const BODY_READ_CHUNK: usize = 64 * 1024;
    let mut body: Vec<u8> = Vec::with_capacity(body_prefix.len().min(content_length));
    let prefix_take = body_prefix.len().min(content_length);
    body.extend_from_slice(&body_prefix[..prefix_take]);
    let mut chunk_buf = [0u8; BODY_READ_CHUNK];
    while body.len() < content_length {
        if body.len() > max_body {
            return Err(format!("body exceeded cap {max_body} while reading"));
        }
        let want = (content_length - body.len()).min(BODY_READ_CHUNK);
        let n = reader
            .read(&mut chunk_buf[..want])
            .await
            .map_err(|e| format!("body read failed: {e}"))?;
        if n == 0 {
            return Err("connection closed before body complete".into());
        }
        body.extend_from_slice(&chunk_buf[..n]);
    }
    debug_assert_eq!(body.len(), content_length);
    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}

/// Naive byte-substring search. `memchr` would be faster but
/// the needle here is 4 bytes and the haystack is bounded at
/// 17 KiB per request — the naive walk is sub-microsecond.
pub fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

pub async fn write_http_response<W>(
    writer: &mut W,
    status: u16,
    reason: &str,
    body: &str,
) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        len = body.len()
    );
    writer.write_all(response.as_bytes()).await
}
