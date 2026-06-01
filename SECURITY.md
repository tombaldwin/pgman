# Security Policy

pgman connects to live PostgreSQL databases and resolves connection
credentials, so security reports are taken seriously.

## Reporting a vulnerability

Please **do not** open a public issue for security problems. Instead, email
**tom@polymorphism.co.uk** with:

- a description of the issue and its impact,
- steps to reproduce (a minimal example if possible),
- the pgman version (`pgman --version`) and platform.

You'll get an acknowledgement within a few business days. Once a fix is
available, we'll coordinate disclosure and credit you in the changelog unless
you prefer to remain anonymous.

## Scope / design notes worth knowing

These are intentional behaviours, not bugs, but they matter for a tool in this
class:

- **Credentials are never logged.** Resolved passwords/tokens are kept out of
  `tracing` output and the UI; only redacted DSNs (`postgres://user:***@host/db`)
  and credential *provenance* are shown or logged.
- **`sslmode=require` / `prefer` encrypt without verifying the server
  certificate** (matching libpq semantics). Use `sslmode=verify-full` on
  untrusted networks where you need certificate + hostname verification.
- **The JDBC-tap listeners (`--tap-listen` / `--tap-otlp` / `--tap-udp`) are
  unauthenticated ingest** and bind to `127.0.0.1` by default. Only bind a
  non-loopback address (e.g. `0.0.0.0`) on a trusted/firewalled network — doing
  so exposes an open ingest endpoint.
- **Destructive SQL is gated client-side** by the per-database safety profile
  (`safety.rs`). This is a guard rail, not a substitute for least-privilege
  database roles — run pgman with a role scoped to what you actually need.

## Supported versions

pgman is pre-1.0; only the latest release/`main` receives security fixes.
