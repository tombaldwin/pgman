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
- **`sslmode=require` / `prefer` / `allow` encrypt without verifying the
  server certificate** (matching libpq semantics). Use `sslmode=verify-full`
  on untrusted networks where you need certificate + hostname verification.
  An `sslmode` value outside `disable | allow | prefer | require | verify-ca
  | verify-full` (case-insensitive, whitespace-trimmed) is a hard connection
  error — it never falls back to a weaker mode silently.
- **The JDBC-tap listeners (`--tap-listen` / `--tap-otlp` / `--tap-udp`) are
  unauthenticated ingest** and bind to `127.0.0.1` by default. Only bind a
  non-loopback address (e.g. `0.0.0.0`) on a trusted/firewalled network — doing
  so exposes an open ingest endpoint.
- **Destructive SQL is gated client-side** by the per-database safety profile
  (`safety.rs`). Every statement in a script is classified and guarded, and
  what reaches the server is the re-joined statements that were checked — not
  the original buffer. A script the splitter cannot verify (an unterminated
  literal, identifier, dollar-quote, or block comment) is **refused**, because
  guards computed from guessed statement boundaries approve the wrong
  statements. `SELECT … INTO`, and a `SELECT` calling one of a short list of
  destructive functions (`pg_terminate_backend`, `pg_read_file`, `lo_import`,
  `dblink`, …), are treated as writes rather than reads. This is a guard rail,
  not a substitute for least-privilege database roles — run pgman with a role
  scoped to what you actually need.
- **A read-only profile cannot be turned off from inside the session.**
  `read_only = true` is applied as `SET default_transaction_read_only = on`,
  but that setting is an ordinary session GUC any role may change, so Postgres
  does not defend it. pgman refuses the statements that would lift it —
  including under `--batch --yes`.

## Supported versions

pgman is pre-1.0; only the latest release/`main` receives security fixes.
