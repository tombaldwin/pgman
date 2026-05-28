//! SSH tunnel for connections that have to traverse a bastion host.
//!
//! Subprocess-based — we shell out to the system `ssh` binary rather than
//! re-implement a Rust SSH client. The trade-off is intentional:
//!
//! - The operator's existing `~/.ssh/config`, agent, `ProxyCommand`,
//!   `IdentityFile`, etc. all work for free — exactly what someone who
//!   "just runs `ssh bastion` every day" expects.
//! - No new crate dep; the system `ssh` is already on every dev box that
//!   would run pgman.
//! - The cost is platform fragility (Windows has its own ssh story) and
//!   we lose static-binary-only-deploy.
//!
//! The tunnel is opened with `BatchMode=yes` so a missing key fails fast
//! rather than blocking the TUI behind an invisible password prompt
//! (alt-screen would swallow it). Operators get a clear error pointing
//! at agent / key setup.
//!
//! [`SshTunnel`] owns the child process; its `Drop` impl sends SIGTERM
//! so the tunnel goes away with the connection.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// `[user@]host[:port]` — what the operator types as the bastion target.
/// `port` is optional and defaults to the OpenSSH default (22). `user` is
/// optional and defaults to whatever `~/.ssh/config` resolves (we don't
/// pass `-l` when unset, so ssh's own resolution wins).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTunnelSpec {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecError {
    Empty,
    BadPort(String),
    MissingHost,
}

impl std::fmt::Display for SpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpecError::Empty => write!(f, "empty ssh tunnel spec"),
            SpecError::BadPort(p) => write!(f, "invalid ssh port {p:?}"),
            SpecError::MissingHost => write!(f, "ssh tunnel spec missing host"),
        }
    }
}

impl std::error::Error for SpecError {}

impl SshTunnelSpec {
    /// Parse `[user@]host[:port]`. IPv6 hosts can be bracketed —
    /// `[::1]:22` — to disambiguate the port colon.
    pub fn parse(s: &str) -> Result<SshTunnelSpec, SpecError> {
        let s = s.trim();
        if s.is_empty() {
            return Err(SpecError::Empty);
        }
        // Split user@rest, if any. An empty user before `@` (i.e.
        // `@bastion`) is treated as "no user, host=bastion" — the
        // forgiving interpretation. Without this, `@bastion` would
        // be parsed as host=`@bastion` and we'd hand ssh a literal
        // `@bastion` argument that resolves to a confusing
        // `Could not resolve hostname @bastion`.
        let (user, hostport) = match s.rsplit_once('@') {
            Some((u, hp)) if !u.is_empty() => (Some(u.to_string()), hp),
            Some((_, hp)) => (None, hp),
            None => (None, s),
        };
        // Split host and port. Bracketed IPv6 wins; otherwise last `:`.
        let (host, port) = if let Some(stripped) = hostport.strip_prefix('[') {
            let (h, rest) = stripped.split_once(']').ok_or(SpecError::MissingHost)?;
            if h.is_empty() {
                return Err(SpecError::MissingHost);
            }
            let port = match rest {
                "" => None,
                r => {
                    let p = r.strip_prefix(':').ok_or(SpecError::MissingHost)?;
                    Some(p.parse::<u16>().map_err(|_| SpecError::BadPort(p.into()))?)
                }
            };
            (h.to_string(), port)
        } else {
            match hostport.rsplit_once(':') {
                Some((h, p)) if !h.is_empty() => (
                    h.to_string(),
                    Some(p.parse::<u16>().map_err(|_| SpecError::BadPort(p.into()))?),
                ),
                _ => {
                    if hostport.is_empty() {
                        return Err(SpecError::MissingHost);
                    }
                    (hostport.to_string(), None)
                }
            }
        };
        Ok(SshTunnelSpec { user, host, port })
    }

    /// Display form — what we render in provenance and logs. Mirrors the
    /// parse format so `parse(spec.to_string())` round-trips.
    pub fn to_display(&self) -> String {
        let mut s = String::new();
        if let Some(u) = &self.user {
            s.push_str(u);
            s.push('@');
        }
        if self.host.contains(':') {
            s.push('[');
            s.push_str(&self.host);
            s.push(']');
        } else {
            s.push_str(&self.host);
        }
        if let Some(p) = self.port {
            s.push(':');
            s.push_str(&p.to_string());
        }
        s
    }
}

/// Live SSH tunnel — owns the child `ssh` process and the local port it
/// bound. Drop terminates the child so the tunnel disappears with the
/// owner (i.e. the postgres connection).
#[derive(Debug)]
pub struct SshTunnel {
    child: Child,
    pub local_port: u16,
}

impl SshTunnel {
    /// Open a tunnel: `ssh -N -L 127.0.0.1:<local>:<remote_host>:<remote_port> [user@]bastion[:port]`.
    /// Picks a free local port via the kernel (`bind 127.0.0.1:0` then drop),
    /// then asks ssh to bind it. Polls the local port until it accepts a
    /// TCP connect (or the deadline expires) so the caller can connect
    /// straight after this returns.
    pub fn open(
        spec: &SshTunnelSpec,
        remote_host: &str,
        remote_port: u16,
    ) -> Result<SshTunnel, String> {
        let local_port = pick_free_local_port()?;
        let forward = format!("127.0.0.1:{local_port}:{remote_host}:{remote_port}");
        let bastion = match &spec.user {
            Some(u) => format!("{u}@{}", spec.host),
            None => spec.host.clone(),
        };
        let mut cmd = Command::new("ssh");
        cmd.arg("-N") // no remote command
            .arg("-T") // no pty
            .arg("-o")
            .arg("BatchMode=yes") // fail fast on missing keys
            .arg("-o")
            .arg("ExitOnForwardFailure=yes")
            .arg("-o")
            .arg("ServerAliveInterval=30")
            .arg("-o")
            .arg("ServerAliveCountMax=3");
        if let Some(p) = spec.port {
            cmd.arg("-p").arg(p.to_string());
        }
        cmd.arg("-L").arg(&forward).arg(&bastion);
        // Keep stdin closed so ssh doesn't try to interact; capture stderr
        // for the failure diagnostic; ignore stdout (ssh -N is silent).
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        tracing::info!(
            "opening SSH tunnel: ssh {} (-L {forward})",
            // log the bastion target only — no creds involved (we never
            // pass passwords; BatchMode requires keys / agent).
            bastion
        );
        let child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn ssh: {e}"))?;
        let mut tunnel = SshTunnel { child, local_port };
        // Poll: ssh needs a moment to authenticate and bind the local
        // port. Probe by connecting to it; success means the tunnel is
        // up. If ssh dies in this window, we surface its stderr.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(Some(status)) = tunnel.child.try_wait() {
                let stderr = tunnel.drain_stderr();
                return Err(format!(
                    "ssh exited before the tunnel was ready (status {status}){}{stderr}",
                    if stderr.is_empty() { "" } else { ": " }
                ));
            }
            if std::net::TcpStream::connect_timeout(
                &format!("127.0.0.1:{local_port}").parse().unwrap(),
                Duration::from_millis(200),
            )
            .is_ok()
            {
                // Drain ssh's stderr to tracing for the lifetime of
                // the tunnel — otherwise the ~64 KiB kernel pipe
                // buffer fills (ServerAlive warnings, `channel N: open
                // failed` lines, anything verbose `~/.ssh/config`
                // emits) and ssh blocks on its next stderr write,
                // hanging the forwarded session mid-operation.
                if let Some(stderr) = tunnel.child.stderr.take() {
                    std::thread::spawn(move || {
                        use std::io::{BufRead, BufReader};
                        let reader = BufReader::new(stderr);
                        for line in reader.lines().map_while(Result::ok) {
                            tracing::debug!("ssh tunnel: {line}");
                        }
                    });
                }
                return Ok(tunnel);
            }
            if Instant::now() >= deadline {
                // Kill + wait + then read stderr to EOF. Waiting
                // before draining guarantees the pipe is closed so
                // `read_to_end` returns promptly instead of relying on
                // SIGKILL having already torn down the pipe.
                let _ = tunnel.child.kill();
                let _ = tunnel.child.wait();
                let stderr = tunnel.drain_stderr();
                return Err(format!(
                    "ssh tunnel didn't open within 10s{}{stderr}",
                    if stderr.is_empty() { "" } else { ": " }
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Read the child's stderr to EOF. **Blocks** until the pipe is
    /// closed — so only call this after the child has exited or been
    /// killed-and-waited. We use this on the failure paths inside
    /// `open` to assemble a useful error message; the live-session
    /// stderr stream is drained by a separate background reader
    /// spawned at the end of a successful open.
    fn drain_stderr(&mut self) -> String {
        use std::io::Read;
        if let Some(mut err) = self.child.stderr.take() {
            let mut buf = Vec::new();
            let _ = err.read_to_end(&mut buf);
            return String::from_utf8_lossy(&buf).trim().to_string();
        }
        String::new()
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        // `Child::kill` sends SIGKILL on Unix (and the equivalent
        // forceful terminate on Windows). ssh has no on-disk state to
        // flush, so a hard kill is appropriate — the alternative is
        // bookkeeping a SIGTERM + grace timeout for no real benefit.
        // We `wait` afterwards to reap the zombie.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Ask the kernel for a free TCP port: bind `127.0.0.1:0`, read back the
/// assigned port, drop the listener so ssh can re-bind it. There's a
/// tiny race window between drop and ssh-bind where another process
/// could snatch the port; we accept it — ssh would fail fast and the
/// operator can retry.
fn pick_free_local_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("couldn't reserve a local tunnel port: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("local addr lookup failed: {e}"))?
        .port();
    drop(listener);
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_host() {
        let s = SshTunnelSpec::parse("bastion.example.com").unwrap();
        assert_eq!(s.user, None);
        assert_eq!(s.host, "bastion.example.com");
        assert_eq!(s.port, None);
    }

    #[test]
    fn parses_user_at_host() {
        let s = SshTunnelSpec::parse("tom@bastion").unwrap();
        assert_eq!(s.user.as_deref(), Some("tom"));
        assert_eq!(s.host, "bastion");
        assert_eq!(s.port, None);
    }

    #[test]
    fn parses_user_host_port() {
        let s = SshTunnelSpec::parse("tom@bastion:2222").unwrap();
        assert_eq!(s.user.as_deref(), Some("tom"));
        assert_eq!(s.host, "bastion");
        assert_eq!(s.port, Some(2222));
    }

    #[test]
    fn parses_bracketed_ipv6_no_port() {
        let s = SshTunnelSpec::parse("[::1]").unwrap();
        assert_eq!(s.host, "::1");
        assert_eq!(s.port, None);
    }

    #[test]
    fn parses_bracketed_ipv6_with_port() {
        let s = SshTunnelSpec::parse("tom@[::1]:22").unwrap();
        assert_eq!(s.user.as_deref(), Some("tom"));
        assert_eq!(s.host, "::1");
        assert_eq!(s.port, Some(22));
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(SshTunnelSpec::parse(""), Err(SpecError::Empty));
        assert_eq!(SshTunnelSpec::parse("   "), Err(SpecError::Empty));
    }

    #[test]
    fn rejects_bad_port() {
        assert!(matches!(
            SshTunnelSpec::parse("bastion:NaN"),
            Err(SpecError::BadPort(_))
        ));
        assert!(matches!(
            SshTunnelSpec::parse("bastion:99999"),
            Err(SpecError::BadPort(_))
        ));
    }

    #[test]
    fn orphan_at_means_no_user_not_at_in_host() {
        // `@bastion` is forgiving-parsed as user=None, host=bastion
        // (rather than the prior wrong behaviour of host=`@bastion`,
        // which made ssh emit "Could not resolve hostname @bastion").
        let s = SshTunnelSpec::parse("@bastion").unwrap();
        assert_eq!(s.user, None);
        assert_eq!(s.host, "bastion");
    }

    #[test]
    fn display_round_trips_bare_host() {
        let s = SshTunnelSpec::parse("bastion").unwrap();
        assert_eq!(s.to_display(), "bastion");
    }

    #[test]
    fn display_round_trips_user_host_port() {
        let s = SshTunnelSpec::parse("tom@bastion:2222").unwrap();
        assert_eq!(s.to_display(), "tom@bastion:2222");
    }

    #[test]
    fn display_brackets_ipv6() {
        let s = SshTunnelSpec {
            user: Some("tom".into()),
            host: "::1".into(),
            port: Some(22),
        };
        assert_eq!(s.to_display(), "tom@[::1]:22");
    }
}
