# Design: masked dumps, and the life of a copy

Status: **proposal, third draft; phases 1, 1b and 2a built (2026-10-28/29), the rest not.** First draft 2026-10-28,
reviewed the same day by a four-reviewer panel (privacy and compliance,
security, DBA and feasibility, developer experience) and revised with
Tom's decisions; the second draft was reviewed again by the same four and
one fresh reviewer, and this draft folds that in. What changed is at the
end.

## Why, and who it is for

`pgman dump` (v0.3) puts a copy of any environment two commands away,
production included. A copy of production is useful — testing against the
real shape of the data, its volumes and its odd rows — and it is personal
data on a laptop: a new place the data is processed under UK and EU GDPR,
a file that lingers, gets synced and backed up, and a breach when the
laptop is lost.

**The aim is to help a good developer practise good hygiene, and show it
if asked.** pgman is one layer of several, as Article 32 expects:

- **AWS is the access boundary.** IAM decides who can read the database
  secret and reach the bastion; CloudTrail records it; KMS (section 5)
  decides who can open an encrypted copy. That is the trail no developer
  can edit.
- **pgman adds minimisation, encryption, lifecycle and a record**: copies
  masked by default, encrypted, tracked from dump to deletion, with a
  journal of who, why and when it was gone.
- **The organisation adds the rule**: production copies go through pgman,
  masked unless a named person approves otherwise, deleted when the ticket
  closes. A tool default is a technical measure; it needs that
  organisational one beside it, and the privacy note (`docs/privacy.md`)
  cites it.

What pgman guards against itself are mistakes, not malice:

- **A coding agent whose only access is MCP**, wrong or steered by text it
  has read, asking for more than it should. MCP's limits (section 10)
  bound that agent. An agent with a shell on the operator's machine *is*
  the operator — it can edit any config file, drive the TUI and run
  pg_dump — and the line for it is its own permission prompts, not pgman.
- **Accidents**: a raw copy left in a synced folder, a rules file that
  quietly stopped covering a new column, a restored copy nobody dropped.

Pseudonymised data is still personal data; masked copies get the same
lifecycle as raw ones, just fewer of the people in them. And masking is a
control on what reaches the disk: MCP `query` against the live protected
database still returns its rows, by decision (Decided, 12).

## The shape, in one paragraph

A protected environment's dumps are **masked by default** with built-in
rules that need no configuration; the developer sees exactly what was
masked and adjusts it in a committed rules file or pgman's Masking screen.
Every dump of a protected environment is **encrypted** to a key the
operator's account controls. pgman **tracks every copy** — the file and
any database restored from it — gives each an expiry, asks why it was
taken, and deletes it (and the databases it made from it) in one step when
it's done, writing each step to a **journal**, locally and to the client's
CloudWatch.

## 1. Policy

In the operator's own `environments.toml`, per environment:

```toml
[[environments]]
name = "production"
dump = "masked"            # unmasked | masked | schema | never
encrypt = "kms:alias/pgman-dumps"   # kms:… | age:… | none
masking_key = "secret:prod/pgman/masking"   # optional: a stable key (section 5)
dump_ttl = "14d"           # how long a copy lives
dump_reason = "required"   # required | optional
journal = { group = "/pgman/dumps" }   # section 7
```

| `dump` | A dump may be |
|---|---|
| `unmasked` | raw (the old behaviour), masked, or schema only |
| `masked` | masked, or schema only |
| `schema` | schema only |
| `never` | nothing |

Defaults:

- **Protected environments: `dump = "masked"`, `encrypt` required,
  `dump_ttl = "14d"`, `dump_reason = "required"`.** Unprotected:
  `unmasked`, no encryption, `30d`, reason optional.
- **Until masking ships (phase 2a)**, the protected default is `unmasked`
  with a confirmation: a raw dump of a protected environment takes the
  environment's name typed at a terminal, a warning that it copies
  personal data, and a reason. **Without a terminal it is refused** — no
  flag answers it — which deliberately ends v0.3's "no question asked" for
  raw protected dumps (`docs/dump-restore.md` changes with phase 1). Cron
  and CI take masked or schema dumps, with `--reason`, which need no typed
  name.
- **A masked or schema dump takes a reason only**, never the typed name.
- `unmasked` for a protected environment is deliberate: written in the
  operator's own file, typed name each time, a reason, journaled,
  encrypted once 2b ships, never over MCP.
- **`encrypt` before phase 2b**: parsed; `none` is the only value accepted,
  with a warning on each protected dump that it isn't encrypted yet. The
  requirement switches on with 2b. Until then a protected dump is a
  plaintext file, as it is in v0.3; phase 1 makes nothing worse.
- A project's `.pgman/pgman.toml` can tighten any of this and never loosen
  it: `[dumps.environments.production] dump = "masked"`, merged as the
  minimum of `never < schema < masked < unmasked`. A client that wants
  "pgman won't take a raw copy of our production" commits that cap in its
  own repo — and the privacy note says what that does and doesn't promise
  (a developer can still open psql).

The new fields are new keys under `deny_unknown_fields`: an older pgman
reading the same `environments.toml` refuses it. The release notes say so.

**The reason** is one line, at most 120 characters, prompted as "ticket id
or short reason — no names". People will type the same thing every time,
so pgman makes the useful answer the easy one:

- **The ticket from the branch.** When the current git branch names a
  ticket (`feature/APP-101-invoice-rounding`), the prompt offers `APP-101`
  as the default (Enter accepts) and records it as the copy's `ticket`,
  separately from any free text. The pattern defaults to Jira-style keys
  (`[A-Z][A-Z0-9]+-[0-9]+`) and is settable per project (`[dumps]
  ticket_pattern`).
- **Otherwise the last reason** given for that target is offered.
- **A link** when the project sets `[dumps] ticket_url =
  "https://examplecorp.atlassian.net/browse/{ticket}"`: `pgman copies` and
  the report show each copy's ticket as a link, so a DPO or reviewer goes
  from "why was this copy taken" to the ticket in one click.
- **Checking the ticket exists** (and is open) against Jira is a later
  item, opt-in, through the operator's own Jira credentials.

Without a terminal, `--reason` (or `--ticket`) is required where the policy
requires one. Over MCP the operator gives it in the confirmation form,
prefilled from the branch; the agent may propose one, never supply it.

## 2. Masking that works out of the box

**No rules file is needed for a first masked dump.** pgman classifies every
column from the catalog (no rows read) and applies a built-in default to
each column it finds personal:

| Column | Default rule |
|---|---|
| email, login | a unique fake: `firstname.lastname.<hash>@example.test`, shortened to fit (names dropped, then the hash to 16 hex); refused at check below that |
| name (first, last, full, display) | a fake name from a built-in list, with a hash suffix where the column is unique |
| phone, mobile | the input's shape (prefix, length, punctuation) with the digits from the hash; Ofcom's drama ranges where the shape is a UK number and the column isn't unique |
| address, street, city | a fake address |
| postcode | the district (`SW1A 1AA` → `SW1A`) |
| date of birth | the year: `date_trunc('year', dob)`, still a date |
| IP address | an address in a documentation range, from the hash |
| password hash | one known test password, hashed as the input was (same algorithm, a `{bcrypt}`-style prefix only if the input had one) |
| token, secret, session, api key, refresh token | NULL |
| special category (health, ethnicity, religion, sexuality, gender, union, political, biometric, genetic, criminal) | NULL |
| government and financial identifiers (nino, nhs, passport, licence, iban, sort code, account number, card) | a fake of the same format, or NULL |
| free text by name (note, comment, description, message, body, detail, remark, bio, reason, summary) | NULL |
| free text, JSON, bytes, `tsvector`, unknown types | NULL |
| everything else | kept |

**NULL where a column can't take NULL** becomes the type's empty value:
`''`, `'{}'` for JSON, arrays and hstore, zero-length `bytea`, `false`, the
enum's first label. An unknown type that is `NOT NULL` is refused at check,
naming it. Under a unique index the placeholder is hash-derived instead. A
CHECK or domain the placeholder fails is found by `masking check` (section
8), which tries each placeholder against the constraint on the server with
no production rows.

Framework presets apply automatically when their tables are found:
Flyway's and Liquibase's history tables kept; Spring Security's users table
gets the test password; session and token tables (`spring_session*`,
`persistent_logins`, OAuth client tables) dumped with no rows; Envers
`*_aud` tables follow their base table's rules.

So `pgman dump production` just works, and says exactly what it did:

```
masked: 41 columns in 18 tables, by default rules
  email      customers.email, users.login, orders.customer_email (follows customers.email)
  name       customers.first_name, customers.last_name, …  (9)
  NULL       customers.notes, events.payload, tickets.body, …  (14)
  …
test password for every account: in the copy's details (pgman copies show)
your account: dev@ourcompany.example → ava.okafor.3f9@example.test
key: fresh for this copy (pseudonyms won't match another copy)
```

Every NULLed column is named, in the summary and in `pgman masking show
<target>`, which prints each one with the line to paste to keep it. The
test password goes to the copy's details, not stdout or a CI log. The
operator's own row is found by their git email when it is in the data, and
the fake it became is shown.

### The classifier

- **By name, on every type, by whole word.** Column names are split on
  `_` and camelCase into words, and matched whole: `age` matches `age` and
  `age_band`, never `stage` or `message`; `ip` matches `ip_address`, never
  `zip` or `recipient`. The vocabulary:
  - contact and identity: email, mail, phone, mobile, tel, the name
    family, title, address, street, city, postcode, zip;
  - birth, age and family: dob, birth, age, marital, nationality;
  - identifiers: ssn, nino, national insurance, nhs, passport, licence,
    iban, sort code, account number, card number, salary, pay, income;
  - network and device: ip, mac address, user agent, device;
  - location: lat, lng, latitude, longitude, location;
  - images: photo, avatar, signature;
  - people in audit columns: `*_by`, author, actor;
  - secrets: token, secret, password, session, cookie, api key, mfa,
    credential;
  - free text: note, comment, description, message, body, detail,
    remark, bio, reason, summary;
  - **special-category and criminal-offence data:** gender, sex,
    ethnicity, race, religion, health, medical, diagnosis, disability,
    sexual, union, political, biometric, genetic, criminal, conviction,
    offence.

  Precedence is specific over general: `password_reset_token` is a token,
  not a password; `health_check_at` is a timestamp, not health data (a
  system-timestamp suffix wins).
- **By type**: `text`, unbounded `varchar`, `citext`, `json`, `jsonb`,
  `xml`, `bytea`, `tsvector`, arrays of text, `inet`, `cidr`, `macaddr`,
  PostGIS types, and any type pgman doesn't know. **A bounded `varchar(n)`
  is judged by name only**: Hibernate makes every `String` a
  `varchar(255)`, and flagging them all would bury the real columns.
- **Not personal, by name**: `*_code`, `*_type`, `status*`, `currency`,
  `locale`, `sku`, `slug`; surrogate keys (`id`, an integer or uuid `*_id`
  that is a foreign key); business numbers (`order_number`,
  `invoice_number`, `line_number`, a `*_ref` that isn't a person's); the
  system's own timestamps (`created_at`, `updated_at`, `deleted_at`).
- **External identifiers** are personal when the name pairs a person or
  provider word with an id (`customer_ref`, `stripe_customer_id`,
  `payment_account_id`) and the column isn't a foreign key.
- **Large objects** are one item to decide, like a column: kept or left
  out.
- The vocabulary is extendable per project (`[classify] personal = […]`,
  `not_personal = […]`). A project `not_personal` entry loosens, so it
  counts as a `keep` on every column it uncovers: it needs a reason and
  shows in the rules digest's diff. Sampling values is used only to
  *suggest* (on the Masking screen and in validate), never to decide, so
  the same schema masks the same way everywhere, including CI.

## 3. Adjusting it: the rules file

Differences from the defaults live in a committed, reviewable file, one per
database: `.pgman/masking/<database>.toml`, found by walking up from the
current directory (or `--masking FILE`). It is created the first time
someone changes something, on the Masking screen (section 9) or by hand:

```toml
version = 1

[tables.customers]            # public.customers
notes = { rule = "keep", reason = "support notes, reviewed: no personal data" }
contact = "email"             # a varchar(255) the classifier couldn't tell
tier = "keep"

[tables.customers.when]
keep_if = "email LIKE '%@ourcompany.example'"   # the team's own accounts stay as they are

[tables.users]
password_hash = { rule = "password", value = "pgman-test" }

[tables.events]
payload = { rule = "json", keep = ["$.type", "$.version"] }   # all other keys NULLed

[tables."*_aud"]
like = "*"                    # each audit table follows its base table
```

- `keep` on a column the classifier calls personal needs a `reason`; the
  Masking screen asks for one.
- Rules **follow foreign keys** from any referenced column (primary or
  unique, composite included): every column referencing it gets the same
  transform, so joins still join. Surrogate integer and uuid keys are never
  masked.
- A `when` condition on a referenced table may read that column only, so
  both sides of a join decide the same way. `when` conditions are
  evaluated on the server, inside the dump's read-only transaction, under
  the same bounds as `sql`.
- **Dates shift per row**, keyed by the row's primary key, the same amount
  for every date in the row: an order is never shipped before it was
  placed. A partition-key date is never shifted.
- `sql` (an expression for what nothing else covers) may read only the
  row's own columns and an allow-list of functions — no subquery, no
  `FROM` — and is refused over MCP.
- `stable = true` on a rule (section 5) is a project choice that makes a
  column linkable across copies; it shows in the digest diff like a
  `keep`.

Transformers preserve the column's type and fit its length (read through
domains to the base type). A column under a unique index — expression and
partial ones included — gets a transform that stays unique (from at least
64 bits of the keyed hash); a lossy transform there (a generalisation, a
word-list fake) takes a hash suffix, or is refused at check where the
suffix doesn't fit.

## 4. The data path

1. One `REPEATABLE READ` transaction; `pg_export_snapshot()`; `ACCESS
   SHARE` locks on every table copied, as pg_dump takes; `statement_timeout`,
   `lock_timeout` and `idle_in_transaction_session_timeout` off; and
   `row_security = off`, so an RLS-filtered read fails loudly instead of
   dumping a subset. All of this works on a hot standby (verified).
2. `pg_dump -Fc --snapshot=<that snapshot> --exclude-table-data='*.*'
   --no-comments`: the schema, every sequence's value and the large
   objects, **no table rows and no comments**. String literals in column
   defaults, CHECKs, view bodies, policies and function bodies are to be
   scanned and reported in the summary (they are schema, kept as they
   are; a team that finds a personal value there fixes the schema) — not
   built yet: see "Phase 2a as built".
3. Each table's rows (leaves for partitioned and inherited tables, `FROM
   ONLY`; stored generated columns left out; foreign tables skipped unless
   `--include-foreign-data`) through `COPY (SELECT …) TO STDOUT` in the
   same snapshot. Only NULLs, placeholders and dropped columns are pushed
   into the `SELECT`; every other rule runs in pgman, so **the masking key
   never reaches the server** (not `pg_stat_activity`, not RDS statement
   logs). One table at a time in phase 2a; `--verbose` reports each
   table's time; parallel COPY over the exported snapshot is later.
4. Written as pgman's own archive, a directory:

```
production-ledger-20261028T1403Z.pgman-dump/
  header            # plain: format, pgman version, dump id, environment,
                    #        database, created, expires, masked, rules digest
  manifest.toml     # encrypted: tables, row counts, classifier results,
                    #            key fingerprints, word-list version,
                    #            sha256 of every data file
  schema.dump       # encrypted: the no-rows pg_dump archive
  data/0001.copy.zst …   # encrypted: one file per table, generated names
```

`pgman restore` reads it: checks every file's checksum against the
manifest; `pg_restore --section=pre-data`; the rows by `COPY … FROM STDIN`,
only into tables pre-data created, identifiers quoted; then
`--section=data` (sequence values, large objects); then
`--section=post-data` (indexes, constraints, triggers), so foreign keys are
checked on the masked rows. A constraint error's `DETAIL` (which prints the
offending value) is redacted. The row COPY and the provenance write are
`Db` effects in `src/query/`, as candor requires. Only `pgman restore`
reads the archive; pg_dump's directory format is a later goal.

Options after `--` are limited, on a masked or schema dump, to a short safe
list (`--exclude-table*`, which composes with pgman's own `'*.*'`, and
`--exclude-schema`); anything that could bring raw rows back
(`--data-only`, `--section`, `--table`, `--inserts`, large-object options)
is refused.

## 5. Keys

**Masking key.** By default **a fresh random key per dump**, used and
discarded. Pseudonyms are consistent within the copy (joins hold) and match
nothing else, and nobody (the person who took the dump included) can test
a guess against them afterwards. Nothing to set up. A fresh key doesn't
hide frequencies: the most common fake surname is still the most common
real one. Validate reports columns where that matters (few distinct
values).

**A stable key, when results must repeat** — the same customer gets the
same pseudonym in today's copy and next week's, so a bug report can name
one — is an opt-in at the operator's level:

- `masking_key = "secret:…"` on the environment: its own secret, not the
  database password (refused if equal), ideally in the client's AWS account
  (who can read it is an IAM decision, every read is in CloudTrail, the
  client can revoke it), at least 16 bytes; `pgman masking key new`
  creates one;
- optionally per rule (`stable = true`, section 3), so only the columns
  that must line up between copies use it and the rest stay fresh per
  dump;
- each dump records the key's fingerprint, never the key, so it's visible
  which copies can be linked; `pgman masking key rotate` stops new copies
  linking to older ones (the older ones stay linked to each other); rotate
  on a schedule and when someone leaves;
- the bundled word lists are versioned and recorded, so an upgrade doesn't
  change everyone's fake name.

With a stable key, copies can be linked, and whoever holds the key can test
a guess. That is the trade for repeatability, made per environment by the
operator.

**Encryption key** — a separate secret, never derived from the masking key
and never wrapping it, so being able to restore never means being able to
reverse:

- `kms:alias/…` for AWS environments (one key per environment or per
  client): each dump gets a fresh age identity; the archive is encrypted to
  it with the stock `age` crate (no home-made streaming crypto); KMS wraps
  the identity with the header's fields as the encryption context, and
  the header is repeated inside the ciphertext, so editing it fails
  decryption. Restoring calls `kms:Decrypt` — an IAM decision, a CloudTrail
  line, gone with a leaver's AWS access, and disabling the key makes every
  copy unreadable.
- `age:age1…` for the rest: the operator's own key (Keychain or Secure
  Enclave). Works offline; no central revocation.
- `none`, only as an explicit opt-out the operator writes, and never for a
  raw dump of a protected environment. No passphrases.
- `--jobs` restores need a seekable file: decrypted to a private, exclusive
  temp file under pgman's cache directory (never beside the dump, never
  `/tmp`), removed however the restore ends, and the result says one
  existed.

## 6. The life of a copy

pgman tracks every copy from the dump to its deletion, so the hygiene is
one command and the record writes itself.

- **Identity.** Every dump gets a dump id (a uuid) at the moment it
  starts, from phase 1: in the journal, the file's header (from 2a) and
  the provenance of every database restored from it. The **copies
  registry**, built from the journal in pgman's data directory, is the
  truth about what exists; everything else is checked against it.
- **Why.** A reason (section 1), kept with the copy.
- **Where** (from 2a). Dumps go to pgman's dump directory by default
  (`util::data_dir()/dumps`), with an explicit path allowed; MCP may write
  there as well as under its working directory. pgman refuses to write a
  raw dump into a synced folder (iCloud Drive, Dropbox, OneDrive, Google
  Drive) and warns for a masked one, excludes every dump from Time Machine
  (`tmutil addexclusion`), and warns when FileVault is off. The result says
  where the file went; phase 1 keeps v0.3's working-directory default.
- **Restored copies** are tracked: a `restore` event in the journal (from
  phase 1), and from 2a a provenance record inside the database — a
  `pgman.provenance` table in its own schema (out of the app's sight;
  Flyway's `clean` of `public` and Hibernate's validate leave it alone),
  plus `COMMENT ON DATABASE` carrying the dump id (survives anything short
  of dropping the database). A copy restored outside pgman is untracked,
  and pgman says so wherever it lists copies.
- **`pgman copies`** lists every dump and restored database the registry
  knows of: age, expiry, reason, masked or not, and whether the file is
  still there (path, size and mtime; no checksum of big files on listing).
  A copy is named by a prefix of its dump id, or by its file. `pgman copies
  show <copy>` gives its details, the test password included.
- **`pgman copies rm <copy>`** deletes the dump file (a `-latest` symlink is
  removed or repointed, never followed) and, from 2a, the databases pgman
  restored from it — **only** databases pgman itself created, whose
  provenance carries the same dump id, on a local server, never a
  protected one (restore's guards, run again at drop time), after a
  `[y/N]` naming the file and each database (as built; typing each
  name was the plan). Connected sessions are listed and need
  `--terminate`, as `restore --clean` does. A server that can't be reached
  keeps its entry, and pgman says so. Never over MCP. `pgman copies prune`
  does the same for everything past its expiry, confirming each database.
- **Expiry.** Expired copies are flagged in the TUI header and on the next
  dump. Restore refuses an expired dump unless `--expired`, which takes the
  typed name and a reason, is journaled, is never available over MCP, and
  the restored database keeps the dump's expiry.
- **Older dumps.** Dumps made by v0.3 aren't in the journal. `pgman
  copies` says so and lists `*.dump` files it finds in the dump directory
  and working directory as untracked, which `rm` deletes like any other.

## 7. The journal

Every step goes to an append-only journal, `util::data_dir()/journal.jsonl`
(0600, opened `O_APPEND`, one `write` per line, so the TUI, CLI and MCP
server can all append), one JSON line per event:

```json
{"v":1,"id":"<ulid>","event":"dump","at":"2026-10-28T14:03:11Z",
 "copy":"<dump id>","env":"production","database":"ledger",
 "protected":true,"mode":"masked","ticket":"APP-101","reason":"invoice rounding repro",
 "by":"arn:aws:sts::123456789012:assumed-role/dev/ava","machine":"m-7f3a9c",
 "via":"cli","file":"production-ledger-20261028T1403Z.pgman-dump",
 "bytes":24117248,"rules":"<digest>","keys":["<fingerprint>"],
 "aws_request_ids":["<secret read>"]}
```

| Event | When |
|---|---|
| `dump` | a dump completed (or failed: `"ok":false`); `via` is `cli`, `tui` or `mcp` |
| `restore` | a dump restored, into which server and database |
| `delete` | a file removed, a database dropped; why (by hand, expired) |
| `expired` | a copy past its expiry still present |
| `restore_expired` | a restore with `--expired` |
| `rules_confirmed` | a rules digest confirmed, at a terminal or a TUI save, with what loosened |
| `policy_changed` | the effective policy for an environment changed since the last event |

**What it holds about developers**, since it goes to a client's account:
one identity (the IAM ARN for AWS environments; for the rest, a name the
operator sets once, `journal_identity`), a random machine id generated once
(never the hostname), the file's name (never its path, which carries the OS
username), and the reason. No values, no secrets, no hostnames. Each event
is pure data; a reason can't carry a newline. The privacy note tells
developers what is recorded and where. The local journal is the
developer's own, theirs to delete at the end of an engagement (`pgman
copies` keeps working from what remains).

`pgman copies report [--since DATE] [--format text|markdown|json]` prints
the journal as an account — taken, why, restored where, deleted when — for
a client's DPO, an audit, or a ticket.

**The central journal is CloudWatch Logs** (phase 1b). Per environment, in
the operator's own `environments.toml`:

```toml
journal = { group = "/pgman/dumps" }
# region and profile default to the environment's AWS scope; a static
# environment names them: { group = "…", region = "eu-west-2", profile = "…" }
```

pgman writes each event to that group with the `aws` CLI (as the rest of
pgman's AWS calls do), one stream per machine id. The local journal is
written first; a cursor in `cache_dir()` records what has been sent, and
unsent events go on the next pgman run. A failed send never blocks a dump,
but the result says the events haven't reached the log yet. The minimal
policy for developers is:

```json
{"Effect":"Allow",
 "Action":["logs:CreateLogStream","logs:DescribeLogStreams","logs:PutLogEvents"],
 "Resource":"arn:aws:logs:*:123456789012:log-group:/pgman/dumps:*"}
```

No `Delete*` and no `PutRetentionPolicy`, so what reaches CloudWatch can't
be removed by the person who sent it. The group lives in the client's
account, with the client's retention (one to two years suggested) and
readers.

What the journal is and isn't: a faithful record kept by a tool used in
good faith, which is what a good developer needs to demonstrate good
practice. It is self-reported; the trail no developer can edit is
CloudTrail's (secret reads, KMS decrypts) and CloudWatch's once sent, and
the journal carries the request ids that match them.

## 8. Validate

`pgman masking check <target>` reads the catalog only (no rows), runs in
CI against an empty migrated database, and exits non-zero when:

- a column's default or placeholder can't be stored (NOT NULL with an
  unknown type, a CHECK or domain the placeholder fails, a unique column
  with a lossy rule, a fake that won't fit the length);
- a rule no longer fits the schema;
- with `--strict`, a column is decided only by the defaults, or the schema
  changed since the digest recorded in the rules file (`schema_digest`,
  updated by `--update`).

`pgman masking show <target>` lists what the defaults and rules do to each
column, NULLed ones first, with the TOML line to change each.

`pgman dump <target> --validate` adds a sample, writes nothing, and prints
**counts, never values**:

- per column: values changed, unchanged, NULL;
- a "masked" column more than 90% unchanged (a broken rule);
- re-identification risk: the smallest group of rows sharing all kept and
  generalised columns on a table, warned under 5 (`postcode district +
  birth year + tier` is often a group of one);
- columns with few distinct values, where frequencies survive masking;
- rows a `when` condition kept as they were.

Validate is refused under `dump = "never"`, since it reads rows. `--preview
N` (original beside masked) is opt-in, needs a terminal and the typed
environment name for a protected environment, is capped at 20 rows, and is
refused over MCP and in machine-readable output.

## 9. In pgman's screens

Developers configure masking without editing files:

- **A Masking screen** (from the environments view or the schema browser)
  lists every table and column, its classification, and its rule (default,
  preset or the team's), risky first (personal and kept, NULLed, free text,
  JSON). Keys:
  - `/` to filter, `j`/`k` to move, Enter to pick a rule from a list;
  - `r` for a `keep` reason;
  - a per-table action (keep all, drop rows);
  - the consequences shown: a rule that follows a foreign key names the
    other columns;
  - `s` to save, which shows the diff and writes
    `.pgman/masking/<database>.toml` with its comments and layout kept
    (`toml_edit`, a new direct dependency);
  - `q` back.

  Hints on screen.
- **The copies view** is `pgman copies`: Enter for a copy's details
  (reason, test password, databases), `x` to delete (with the list of what
  goes, confirmed as `copies rm`), `p` to prune.
- **Policy** stays in `environments.toml` for now: it is set once per
  environment. A policy editor in the environments view is a later item;
  when it comes it writes through `toml_edit` to a temp file renamed into
  place, 0600, and every reader (the MCP server included) re-reads policy
  per use rather than caching it.

A save on the Masking screen is the operator at their terminal, so the new
rules digest is confirmed as it is saved, with a `rules_confirmed` event
naming what loosened. A rules file changed any other way (an editor, a
branch, an agent) is shown on the next dump's confirmation as "rules
changed since you last confirmed them: 3 columns newly `keep`, 1 `sql`",
and an MCP dump refuses rules that are uncommitted or not yet confirmed.

## 10. MCP

- **Phase 1**: `dump` of a protected environment is **schema only**
  (`schema_only`, a new field), or refused where the policy says `never`.
  v0.3 allowed a raw protected dump over MCP after a typed name; phase 1
  removes that, and the release notes say so. From 2a, `dump` takes at most
  `masked`.
- The operator's confirmation names the rules file, what changed in it
  since last confirmed, and what the classifier found, and asks the
  operator for the reason.
- `sql` rules, `--preview` and `--expired` are refused over MCP; there is
  no MCP `copies rm`.
- No MCP tool writes the rules file or the policy; MCP dumps and restores
  are journaled like any other, `via = "mcp"`.
- **`query` refuses a database the copies registry says is a raw copy of a
  protected environment**, and also any database whose provenance says so
  (registry first; the record inside the database is the second check, for
  a copy the registry lost). Masked copies are queryable.
- Validate's counts are available to an agent; values never are.

## 11. Testing

- The classifier (whole-word matching, precedence, the vocabulary), rules
  parser, each transformer, the placeholders, the policy merge: unit
  tests.
- **The leak test**, for every path: a fixture seeded with canary values
  (`canary-7f3a@leak.test`, a distinctive name, a phone number, one in a
  `varchar(255) notes`, one in a comment) is dumped masked, restored, and
  every byte searched — the archive, the journal, stdout, stderr, pgman's
  log, and a restore made to fail on a constraint. Any hit fails, and
  breaking a transformer must make it fail. A canary in a column default
  must show in the summary's literal report.
- Round trip: the masked copy restores; foreign keys, unique indexes, NOT
  NULL columns and sequences hold; row counts match the manifest.
- Determinism: a fixed key gives the same bytes twice; a fresh one,
  different.
- Errors and `Debug` output never carry a value: values live in types that
  redact themselves.
- `copies rm`: never drops a database without a matching dump id, a
  protected one, or one pgman didn't create (each demonstrated failing).
- Every test uses scratch data and config directories, never the real ones.

## Phases

| Phase | What | Size |
|---|---|---|
| 1 | Policy fields and the project cap (`policy::effective`, pure); typed confirmation, warning and reason for raw protected dumps, refused without a terminal; reason for every protected dump, with the ticket from the branch and `ticket_url` links; MCP protected dumps schema-only; the dump id; the local journal; `pgman copies` list, show, rm for files (untracked v0.3 dumps listed); docs updated | 3½ days |
| 1b | The journal to CloudWatch: `aws logs` builder (pure), cursor, retries, the IAM policy in the docs | 1½ days |
| 2a | Classifier with defaults, placeholders and presets; core transformers (keep, null, fixed, password, hash, email, name, phone, ip, per-row date shift, drop rows); FK following; the data path and archive; restore with checksums and redaction; provenance; `copies rm` for databases; the dump directory and synced-folder checks; `masking show` and `check`; the leak test; `docs/privacy.md`. **The protected default moves to `masked`.** | 9 days |
| 2b | Encryption: KMS envelope and age; header binding; private temp for `--jobs`. **`encrypt` becomes required.** | 2–3 days |
| 3 | The Masking screen and the copies view | 5 days |
| 4 | Sampled validate, including the re-identification and frequency warnings | 2 days |
| 5 | The rest of the transformers (addresses, postcodes, generalisations, JSON paths, conditions); stable keys and rotation; the policy editor | 3–4 days |
| 6 | Subsetting, outbound references first, with virtual and polymorphic references (id sets held client-side; no temp tables on a standby) | 1–2 weeks, if asked for |

About four to five weeks to the end of phase 5; each phase ships on its
own. Until 2b, protected dumps aren't encrypted (as in v0.3), and each one
says so.

### Phase 1 as built

What shipped, and where it differs from the plan above:

- `src/policy.rs` (the merge and the gate, pure), `src/journal.rs`,
  `src/copies.rs`; `dump::run_cli` and MCP's `dump` call the gate.
- Parsed now: `dump`, `dump_ttl`, `dump_reason`, `unattended`. Not yet:
  `encrypt`, `masking_key` and `journal`, which arrive with the phases
  that use them (an unknown key is still an error, so nothing is
  silently ignored).
- The local journal records the dump's full path, which `pgman copies`
  needs to find it; events sent to CloudWatch (1b) carry the file's name
  only. It records no identity yet: 1b adds the IAM ARN, when events
  first leave the machine.
- Events: `dump` (and failed ones, `"ok": false`), `restore`, `delete`.
  `expired`, `restore_expired`, `rules_confirmed` and `policy_changed` come
  with the features that make them; a newer pgman's event is kept, not
  counted as a bad line.
- `copies report` (text, markdown, JSON) shipped in phase 1, since it is
  what demonstrates the hygiene. Expired copies are flagged in `copies`
  and after each dump; the TUI header flag waits for the copies view.
- Untracked dumps are found in the working directory; the dump directory
  comes with 2a.

### Phase 2a as built

- `src/masking/` (pure): `catalog` (the model, domains resolved, keys),
  `classify` (the defaults by whole word and type, presets, FK
  following with the shortest length, problems), `rules` (the file),
  `transform` (HMAC-SHA256 with ring; a fresh key per dump), `copytext`,
  `archive` (header, manifest, summary), `report` (show, check).
  `src/query/masking.rs` holds every Db effect: the catalog, the
  snapshot, locks, COPY out and in, sequences, provenance. `src/masked.rs`
  runs it.
- **Sequences** travel in the manifest, read in the snapshot and set
  after the rows: pg_dump 18's `--exclude-table-data='*.*'` drops their
  values along with the rows (the DBA review had verified pg_dump 16).
- **The archive** is JSON (`header.json`, `manifest.json`), not TOML, and
  unencrypted until 2b. Data files are gzip (flate2, already in the
  tree), not zstd.
- **The test password** is fixed (`pgman-test`), its bcrypt hash made
  once offline, so pgman holds no bcrypt; a rules file's own `value` is
  used as written.
- **Rules confirmation**: a masked dump at a terminal shows what a new or
  changed rules file keeps and asks (recorded as its sha256 in the data
  directory); an MCP dump needs the file committed as it is and
  confirmed. Not yet: a diff against the previous version, or a
  `rules_confirmed` journal event.
- **Provenance** is written for archives and for raw dumps the journal
  knows; its comment says raw/masked and protected, which MCP's refusal
  reads when the journal has lost the copy.
- **After the review panel** (2026-10-29): UNIQUE constraints seen (a
  constraint's index depends on the constraint, not its columns); a
  joined group unique and NOT NULL together; code-like last words
  (`_code`, `_status`, `_id`) no longer hide postcodes and identifiers;
  people's tables by their last word and `<whose>_name`; arrays NULLed;
  the rules file reaching partitions, children and `_aud` tables; the
  `--exclude-*` options applied to rows and sequences; an unconfirmed
  loosening rules file refused without a terminal; a dump of a raw
  protected copy treated as protected; dumps fingerprinted, so a copied
  raw dump is known, and MCP restoring only masked or known dumps; a
  failed restore marked and journaled; restores streamed, stoppable,
  redacted live; Ctrl-C stopping a masked dump; Time Machine excluded
  by the attribute, not tmutil's ten seconds; the same placement steps
  for MCP dumps; a `.gitignore` in every archive.
- **Not built in 2a** (open in BACKLOG.md): the literal scan of the schema
  (defaults, view bodies), the operator's own fake login, the
  classifier's summary in the MCP confirmation, `copies prune`, the TUI
  header's expiry flag, fake addresses (NULL until phase 5).

### Phase 1b as built

- `src/journal_send.rs`: `journal = { group, region?, profile? }` on the
  environment (region and profile default to an `rds` source's, its
  `account` is checked first). After each event, and on `pgman copies`,
  pending events go out with `aws logs put-log-events`, one stream per
  machine id, in batches of 50 spanning under a day.
- `by` is the IAM ARN of the sender, read with `sts get-caller-identity`
  when the events are sent, not when they happened; the local journal
  still records no identity.
- What has been sent is recorded in `journal.sent` in the data directory
  (not the cache, so clearing the cache doesn't resend everything).
- Events more than 13 days old when a group is first set are left out:
  CloudWatch refuses events older than 14 days, and one in a batch would
  hold up every later one.

## Decided

1. Protected default: **`masked`**, from the release that ships masking;
   typed-name `unmasked` until then.
2. Masking runs **in pgman**, with only NULLs, placeholders and dropped
   columns pushed to the server.
3. Masking key: **fresh per dump by default; a stable per-environment key
   in the operator's own config when results must repeat**, optionally per
   column, rotatable.
4. Classification by **name and type** decides; sampling only suggests.
5. **`unmasked` stays** for protected environments, deliberate and
   recorded, never over MCP, capped by a project when a client asks.
6. **Encrypted** for protected environments from 2b: KMS for AWS, the
   operator's age key otherwise, a separate key from masking.
7. **Threat model**: layered. AWS is the boundary; pgman adds
   minimisation, encryption, lifecycle and a record; the organisation adds
   the rule. pgman itself guards against an MCP-only agent's mistakes and
   against accidents.
8. **The central journal is CloudWatch Logs**, a log group per environment
   in the client's account, set in the operator's config.
9. **A privacy note ships with masking**: `docs/privacy.md`, for a client's
   DPO and for the developers whose actions the journal records — what pgman
   masks and how, encryption and keys, the life of a copy, what the journal
   records and where, the layers, and what pgman does not claim. Written
   with phase 2a and kept true to what is built.
10. **A raw protected dump needs a terminal.** No flag answers the typed
    name; scripts take masked or schema dumps — unless the operator sets
    `unattended = true` on that environment in their own
    `environments.toml` (never a project file), which lets its raw dumps
    run without a terminal, each with `--reason` or `--ticket`, journaled
    as `via = "unattended"` (Tom, 2026-10-28).
11. **Undecided means NULL, or the type's empty value** where NULL can't be
    stored; never a refusal for a missing rule, and every NULLed column is
    named.

12. **MCP `query` against the live protected environment stays as it
    is.** Masking keeps raw rows off the disk; `query` still reads
    production for the agent, because querying it is what MCP is for. The
    privacy note says so plainly. Masking `query` results with the same
    rules is a possible later item, not planned.
13. **The reason prefers a ticket**, taken from the branch name, shown as a
    link where the project configures one; checking it against Jira is
    later.

## Changes from the second draft

From the second review pass, 2026-10-28:

- **NULL where NULL can't go** becomes the type's empty value, checked
  against constraints; every NULLed column named; `masking show` in 2a.
- **The classifier matches whole words** (no `age` in `stage`), and adds a
  free-text name family (the commonest leak in Java schemas), more
  vocabulary, precedence, business numbers, and `tsvector`.
- **Defaults fit real columns**: phones keep their shape, password
  prefixes mirror the input, emails shorten to fit, dates of birth stay
  dates, special-category and identifier columns have explicit rows.
- **Phase 1 is honest about what isn't built yet**: no encryption until 2b
  (said on every protected dump), MCP schema-only, `encrypt` accepts only
  `none`.
- **Raw protected dumps need a terminal**, ending v0.3's "no question
  asked" for them; masked and schema dumps take a reason only, which the
  prompt remembers.
- **`copies rm` can't drop the wrong database**: dump ids, provenance
  match, pgman-created, local, unprotected, typed names, sessions.
- **The registry is the truth** for MCP `query`'s refusal; provenance moves
  to a `pgman` schema plus a database comment.
- **The journal**: a defined schema; one identity, a machine id, and file
  names, not paths; MCP events, rules and policy events; the IAM policy;
  the AWS CLI and a send cursor; CloudWatch in its own phase.
- **The threat model** is written as layers, with the shell-equals-operator
  line and masking as a disk control.
- **Comments** are left out of masked dumps; literals in defaults and
  bodies are reported.
- **Phases re-sized**: four to five weeks to phase 5.

## Changes from the first draft

From the panel and Tom, 2026-10-28:

- defaults out of the box (the first draft needed a rules file and refused
  without one);
- configurable in the TUI;
- the lifecycle and journal, with the hygiene threat model;
- rules that can't be loosened behind the operator's back;
- restored raw copies kept from the agent;
- the data path corrected: one snapshot, sequences and large objects
  through a no-rows `pg_dump`, `FROM ONLY` leaves, `row_security = off`,
  redacted restore errors;
- name-first classification for bounded `varchar`;
- transformers that preserve type and length, stay unique, shift dates
  per row and follow foreign keys;
- validate split into a catalog check and a sampled run;
- encryption;
- limited pass-through;
- re-sized phases.

## Prior art

- **Greenmask**: pg_dump-compatible masking; deterministic hashing;
  transforms that follow keys and partitions; conditions; subsetting with
  virtual and polymorphic references; a validate command with a safe
  summary mode. Much of sections 3, 5 and 8 follows it.
- **PostgreSQL Anonymizer**: declarative masking as security labels, static
  and dynamic; needs the extension on the server, which managed databases
  often don't allow.
