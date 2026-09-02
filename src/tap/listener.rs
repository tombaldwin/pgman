//! Listener — TCP length-prefixed (default transport) and
//! UDP datagram (opt-in lossy transport) for pgman-tap.
//!
//! TCP is the default: framed, reliable, backpressure-safe.
//! UDP is for operators who can't tolerate the tap blocking
//! the app under telemetry pressure and are happy to lose
//! events instead.
//!
//! Split from `tap/mod.rs` for code-health; the public
//! functions are re-exported.

use super::{forward_or_drop, now_unix_micros, parse, TapEvent};

// ---------------------------------------------------------
// UDP listener — opt-in transport, one event per datagram.
// Use when the JVM side must never block on telemetry
// (production critical paths) and lossy delivery is fine.
// ---------------------------------------------------------

/// Maximum UDP datagram size we'll process. Stays well under
/// the typical 65 KiB UDP cap and gives a useful upper bound
/// for the parse buffer. Datagrams over this size truncate
/// at the OS layer and would fail parse anyway — we just
/// avoid the alloc.
pub const TAP_UDP_MAX_DATAGRAM: usize = 64 * 1024;

/// Spawn a UDP listener that decodes each datagram as one
/// `TapEvent` JSON and forwards through `tx`. Pairs with
/// the TCP listener — operators pick UDP when they care more
/// about "never blocks the app" than "no events lost."
///
/// Parse failures are logged via `tracing::warn` and dropped.
/// A bad datagram never takes out the listener.
pub async fn run_udp_listener(
    addr: std::net::SocketAddr,
    tx: tokio::sync::mpsc::Sender<TapEvent>,
) -> std::io::Result<()> {
    let socket = tokio::net::UdpSocket::bind(addr).await?;
    tracing::info!("tap: UDP listener bound on {addr}");
    let mut buf = vec![0u8; TAP_UDP_MAX_DATAGRAM];
    loop {
        let (len, peer) = match socket.recv_from(&mut buf).await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!("tap-udp: recv failed: {e}");
                continue;
            }
        };
        match parse(&buf[..len]) {
            Ok(mut event) => {
                event.received_at_unix_micros = now_unix_micros();
                if forward_or_drop(&tx, event, "udp").is_err() {
                    return Ok(());
                }
            }
            Err(e) => {
                tracing::warn!("tap-udp: dropped malformed datagram from {peer}: {e}");
            }
        }
    }
}

// ---------------------------------------------------------
// Listener — TCP length-prefixed, the default transport.
// ---------------------------------------------------------

/// Maximum frame size we'll accept on the TCP stream. Bigger
/// payloads mean a misbehaving (or hostile) client tries to
/// pull pgman into a large allocation; we cap it well above
/// any reasonable SQL string + parameters. 256 KiB is generous
/// against the per-field caps in `tap::enforce_field_caps`
/// (8 KiB `sql`, 1 KiB everything else) while keeping the
/// worst case for a full ring (`app::TAP_CAP` frames) in the
/// hundreds of MB rather than the multiple GB a 1 MiB cap
/// allowed.
pub const TAP_MAX_FRAME_BYTES: usize = 256 * 1024;

/// Spawn a TCP listener that accepts pgman-tap connections,
/// reads length-prefixed frames, decodes each via [`parse`],
/// stamps `received_at_unix_micros` at receive time, and
/// forwards events through `tx`. The returned task handle
/// resolves only on socket-bind failure; per-connection
/// errors are logged via `tracing` and the connection is
/// dropped so other clients keep flowing.
///
/// Framing: a 4-byte big-endian length prefix followed by
/// that many JSON bytes (one event per frame). The JAR
/// trivially produces this from any Java `OutputStream`
/// (`writeInt(json.length); write(json)`).
pub async fn run_tcp_listener(
    addr: std::net::SocketAddr,
    tx: tokio::sync::mpsc::Sender<TapEvent>,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("tap: TCP listener bound on {addr}");
    loop {
        let (sock, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!("tap: accept failed: {e}");
                continue;
            }
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_tcp_conn(sock, &tx).await {
                tracing::warn!("tap: conn {peer} ended: {e}");
            }
        });
    }
}

pub async fn handle_tcp_conn(
    mut sock: tokio::net::TcpStream,
    tx: &tokio::sync::mpsc::Sender<TapEvent>,
) -> std::io::Result<()> {
    loop {
        match read_frame(&mut sock, TAP_MAX_FRAME_BYTES).await? {
            None => return Ok(()), // peer closed cleanly
            Some(bytes) => match parse(&bytes) {
                Ok(mut event) => {
                    event.received_at_unix_micros = now_unix_micros();
                    if forward_or_drop(tx, event, "tcp").is_err() {
                        // Receiver gone — abandon the listener.
                        return Ok(());
                    }
                }
                Err(e) => {
                    tracing::warn!("tap: dropped malformed frame: {e}");
                }
            },
        }
    }
}

/// Read one length-prefixed frame from any [`AsyncRead`].
/// Returns `Ok(None)` on a clean EOF before the length prefix
/// (peer closed); `Ok(Some(bytes))` on success; `Err` on a
/// short read mid-frame or a length larger than `max_size`.
///
/// Pure-ish: parameterised over the reader so the test can
/// drive it with an in-memory buffer.
pub async fn read_frame<R>(reader: &mut R, max_size: usize) -> std::io::Result<Option<Vec<u8>>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut len_buf = [0u8; 4];
    // First byte specifically — distinguish "clean close at
    // a frame boundary" (Ok(None)) from "short read inside
    // the prefix" (Err).
    match reader.read_exact(&mut len_buf[..1]).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    reader.read_exact(&mut len_buf[1..]).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > max_size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("tap frame too large: {len} > {max_size}"),
        ));
    }
    if len == 0 {
        return Ok(Some(Vec::new()));
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    Ok(Some(buf))
}
