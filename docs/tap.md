# The JDBC tap

The tap shows the queries your Java application runs, live, from the
application's side: the SQL as the JDBC driver sent it, how long it
took, what it returned, which service, pool, connection and transaction
it ran on, and the code that called it. pgman listens; the application
sends. Nothing is read from the database.

Open the monitor with `F4` (or `:tap`) from any mode. Its views — List,
Hotspots, Callers, Transactions, Pools, N+1, Baseline — and their keys
are in [keys.md](keys.md#jdbc-tap-monitor).

## Three ways in

| Flag | Transport | Sender |
|---|---|---|
| `--tap-otlp <ADDR>` | OTLP/HTTP, JSON | Any JVM running the OpenTelemetry Java agent. The route that works today. |
| `--tap-listen <ADDR>` | TCP, length-prefixed frames | The planned pgman-tap Java library. Reliable and ordered. |
| `--tap-udp <ADDR>` | UDP, one frame per datagram | A sender that must never block on telemetry, and accepts losing events. |

`ADDR` is `:PORT` or a bare `PORT`, which bind `127.0.0.1`, or a full
`host:port`, which binds exactly that (a host name is resolved, IPv4
preferred). Several flags can be given at once; all of them feed the
same monitor. A listener that can't bind (port in use) is reported on
the status line and pgman starts without it.

In a Java project — a `pom.xml`, `build.gradle` or `build.gradle.kts`
in the directory pgman starts in — `--tap-listen` is on by default, on
`127.0.0.1:7432`, and the status line says so. `--no-tap` turns that
off. `--tap-otlp` is never turned on for you: its usual port, 4318, is
the OpenTelemetry Collector's.

Until something arrives, the monitor's List view says where pgman is
listening and how to point a JVM at it.

### OpenTelemetry (OTLP/HTTP)

```sh
pgman --tap-otlp :4318
```

and start the JVM with the agent and two environment variables:

```sh
OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://localhost:4318/v1/traces \
OTEL_EXPORTER_OTLP_PROTOCOL=http/json \
java -javaagent:opentelemetry-javaagent.jar -jar app.jar
```

What pgman accepts:

- **`POST /v1/traces` only.** Any other path is a `404`, any other
  method a `405`. The per-signal `…_TRACES_ENDPOINT` variable is used
  as given, so it needs the `/v1/traces` path; the generic
  `OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318` has the exporter
  add it.
- **JSON only** (`Content-Type: application/json`, which
  `http/json` sends). Protobuf, the exporter's default, is a `415`.
- **gzip or no compression** (`Content-Encoding: gzip`, `x-gzip`,
  `identity` or none). Anything else is a `415`.
- **A `Content-Length` body** of at most 4 MiB (a `400` past it), and at
  most 4 MiB once inflated (a `413`). Chunked transfer encoding is a
  `501`. One request per connection, which must arrive within 10
  seconds (a `408`).

Each span becomes one query event when its `db.system` attribute is
`postgresql` and it has a `db.statement`, both as string attributes;
other spans are skipped. Spans that carry only the newer
`db.system.name` / `db.query.text` attributes are not read. The
mapping:

| Span | Event field |
|---|---|
| `db.statement` | `sql` |
| resource `service.name` | `app` |
| `startTimeUnixNano` | `ts_unix_micros` (the statement's start) |
| `endTimeUnixNano − startTimeUnixNano` | `duration_micros` |
| `status.code = 2` (error) | `error`: the status message, a one-entry chain |

OpenTelemetry spans carry no bound values, pool, connection, transaction
or caller, so the Callers, Pools and Transactions views have little to
show on this route, and the List view's parameters read `params
(withheld by source)`.

### TCP and UDP frames

Both carry the v1 frame below as UTF-8 JSON.

- **TCP** (`--tap-listen`): each frame is a 4-byte big-endian length,
  then that many bytes of JSON. Frames are read in order, any number
  per connection.
- **UDP** (`--tap-udp`): one frame per datagram, no length prefix.
  Datagrams that are lost are gone; nothing counts them.

A frame that doesn't parse is logged and skipped; the connection stays
open. A TCP frame whose length is over the cap closes its connection.

## The v1 frame

One JSON object per frame. `v`, `kind` and `ts_unix_micros` apply to
every kind; each kind adds its own.

```json
{
  "v": 1,
  "kind": "query",
  "ts_unix_micros": 1734567890123456,
  "app": "billing-service",
  "pool": "primary",
  "conn": "primary-7",
  "txn": "primary-7#42",
  "sql": "SELECT * FROM accounts WHERE id = ?",
  "params": ["[redacted]"],
  "params_redacted": true,
  "duration_micros": 4521,
  "rows": 17,
  "error": null,
  "caller": [
    "com.example.OrderService.findById:42",
    "com.example.OrderController.show:88"
  ]
}
```

| Field | Kinds | Required | Meaning |
|---|---|---|---|
| `v` | all | yes | Protocol version. Must be `1`; any other value is rejected, so a mismatched sender fails loudly. |
| `kind` | all | no | `query` (the default when absent), `heartbeat` or `txn_boundary`. |
| `ts_unix_micros` | all | yes | Microseconds since the Unix epoch. For a `query`, when the statement **started**: the Pools view treats each query as holding a connection over `[ts, ts + duration_micros)`, so an end time shifts every statement right by its own duration and overstates peak concurrency. For the other kinds, when it was sent. |
| `app` | all | no | Service name — which application, when several send to one pgman. |
| `pool` | all | no | Connection-pool name (HikariCP's `poolName`, for example). The Pools view groups by it. |
| `conn` | all | no | A stable id for the physical connection. Events on one connection share it. |
| `txn` | all | `txn_boundary` | Transaction id, such as `conn-id#sequence`. The Transactions view groups by it (by `conn` when it is absent). |
| `sql` | `query` | yes | The SQL as the application sent it, `?` placeholders and all. |
| `params` | `query` | no | Bound values, as strings, in placeholder order. Absent for a plain `Statement` or a batch. |
| `params_redacted` | `query` | no | `true` when the sender replaced one or more values with a marker such as `[redacted]`. pgman then labels them `params (some redacted)`. |
| `duration_micros` | `query` | yes | How long the JDBC call took, network and server time included. |
| `rows` | `query` | no | Rows returned, or rows affected for DML. |
| `error` | `query` | no | The exception chain, outermost first and **root cause last**. Absent, `null` or empty for a success. A failed row is red in the List view; `Enter` on it shows the chain. |
| `caller` | `query` | no | A short stack of non-framework frames as `class.method:line`, **innermost first**. The Callers view groups by the first; `Enter` on a List row shows them all. |
| `dropped_events_total` | `heartbeat` | no | Events the sender itself dropped, cumulative since it started. |
| `txn_outcome` | `txn_boundary` | yes | `commit` or `rollback`. |

The kinds:

- **`query`** — one completed statement.
- **`heartbeat`** — sent periodically, so pgman can tell a quiet
  application from no application, and report the sender's own drops.
  Heartbeats are counted, not listed.
- **`txn_boundary`** — sent on `commit()` / `rollback()`. It closes the
  transaction named by `txn` in the Transactions view, with its outcome,
  and is listed as a `[COMMIT]` / `[ROLLBACK]` row. The monitor's
  status counts these as `commits/rollbacks`.

Fields pgman doesn't know are ignored, so a newer sender can add some
without a pgman upgrade.

## Limits

Everything the tap receives comes from outside pgman, so every size is
bounded where it arrives:

| What | Limit | Past it |
|---|---|---|
| A TCP frame | 256 KiB | The connection is closed. |
| A UDP datagram | 64 KiB read | The rest is cut, so it fails to parse. |
| An OTLP request body | 4 MiB as sent, and 4 MiB inflated | `400` / `413`. |
| `sql` | 8 KiB | Cut, ending `…`. |
| Any other string: `app`, `pool`, `conn`, `txn`, each parameter, each cause, each frame | 1 KiB each | Cut, ending `…`. |
| Entries in `params`, `error`, `caller` | 64, the last being `… +N more` | The rest are dropped. A chain that long loses its root cause, and says so. |
| `duration_micros` | 1 hour | Clamped to 1 hour, with a warning in the log. |
| Connections per listener (TCP, OTLP) | 64 at once | Further connections are closed unread. |
| A TCP connection with no complete frame | 30 seconds | Closed; a live sender's every event resets the clock. |
| Events held | 2 000, and 64 MiB of text | Oldest out first. |

A warning a sender can provoke — a malformed frame, a refused
connection, an idle close — is logged at most once a second per kind,
with a count of the ones held back.

### Drops

Two different counts, shown in two places:

- **`DROP ×N`** in the footer: events pgman's own listeners received
  but couldn't hand on, because pgman was behind (its queue holds
  4 096). Counted across every live transport for the session.
- **`· N dropped`** in the monitor's title (and `N dropped (JAR
  backpressure)` in its status): the sender's own
  `dropped_events_total`, from its latest heartbeat — events that never
  left the application.

## Recording and replay

```sh
pgman --tap-otlp :4318 --tap-record capture.jsonl
pgman --tap-replay capture.jsonl
```

`--tap-record PATH` appends every event pgman receives, from every
transport, to `PATH` as JSON Lines: one v1 frame per line, as on the
wire. The file is created owner-only (`0600`), its directory `0700`.
Recording stops, with one warning in the log, once the file would pass
256 MiB; start a new file to go on. When pgman's listeners have dropped
events since the last line, a `heartbeat` line carrying that count goes
in first. `--tap-record` with no transport warns at startup that
nothing will be written.

`--tap-replay PATH` reads such a file and feeds each line through the
same pipeline as a live listener: the monitor, the views, the N+1
detector and `--tap-record`, if given. Replay doesn't wait between
events and never drops one. Blank lines are skipped; a line that
doesn't parse, or is over 256 KiB, is skipped with a warning in the log.
The time each event was received is not on the wire, so a replayed
event is stamped when it is replayed; `ts_unix_micros` keeps the
original.

## Redaction

Bound values are data, and the tap shows them as the application sent
them. The only masking they get is the sender's: a value replaced with
a marker, and `params_redacted: true`, which pgman shows as `params
(some redacted)` in the warning colour so the marker isn't read as the
value. pgman masks nothing in an event itself, and `--redact` (for
screenshots) leaves parameters readable, except a value that matches
one of its patterns (an IP address, an account ID), as anywhere on
screen. See [safety-and-privacy.md](safety-and-privacy.md).

Where the monitor shows parameters, and in the whole-event overlay
(`Enter` on a List row), control characters are made visible: a tab as
`⇥`, a newline as `⏎` on one line or as a line break.

## Security

The listeners are **unauthenticated ingest**: anything that can reach
the port can send events, and they'll be shown as your application's.

- `:PORT` and a bare `PORT` bind `127.0.0.1`, and the Java-project
  default binds `127.0.0.1:7432`. Only a full `host:port` such as
  `0.0.0.0:7432` listens beyond the machine, and only because you asked.
  Do that only on a network you trust or firewall.
- Events are held in memory only, within the limits above, unless you
  pass `--tap-record`.
- An event's parameters are production values unless the sender masks
  them. A `--tap-record` file holds them too: it's created owner-only,
  but treat it like a database dump.
