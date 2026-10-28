# Design: masked dumps, and the life of a copy

Status: **proposal, second draft, not built.** First draft 2026-10-28;
reviewed the same day by a four-reviewer panel (privacy and compliance,
security, DBA and feasibility, developer experience), then revised with
Tom's decisions. What changed from the first draft is at the end.

## Why, and who it is for

`pgman dump` (v0.3) puts a copy of any environment two commands away,
production included. A copy of production is useful — testing against the
real shape of the data, its volumes and its odd rows — and it is personal
data on a laptop: a new place the data is processed under UK and EU GDPR,
a file that lingers, gets synced and backed up, and a breach when the
laptop is lost.

**The aim is to help a good developer practise good hygiene, and show it
if asked.** pgman does not try to stop someone determined to take data
they already have access to: anyone who can read the secret and reach the
bastion can run pg_dump by hand. It makes the right way the easy way —
masked by default, encrypted, tracked from dump to deletion, with a record
that says who, why, and when it was gone.

Two things it does guard against, because they are mistakes, not malice:

- **A coding agent** (over MCP) that is wrong, or steered by text it has
  read, asking for more than it should;
- **accidents**: a raw copy left in a synced folder, a rules file that
  quietly stopped covering a new column, a restored copy nobody dropped.

Pseudonymised data is still personal data; masked copies get the same
lifecycle as raw ones, just fewer of the people in them.

## The shape, in one paragraph

A protected environment's dumps are **masked by default** with built-in
rules that need no configuration; the developer adjusts them in pgman's
Masking screen or a committed rules file. Every dump of a protected
environment is **encrypted** to a key the operator's account controls.
pgman **tracks every copy** — the file and any database restored from it —
gives each an expiry, asks why it was taken, and deletes it (and its
restored databases) in one step when it's done, writing each step to a
**journal** that can be shown on request.

## 1. Policy

In the operator's own `environments.toml`, per environment, also editable
in pgman's environments view (section 9):

```toml
[[environments]]
name = "production"
dump = "masked"            # unmasked | masked | schema | never
encrypt = "kms:alias/pgman-dumps"   # kms:… | age:… | none
masking_key = "secret:prod/pgman/masking"   # optional: a stable key (section 5)
dump_ttl = "14d"           # how long a copy lives
dump_reason = "required"   # required | optional
```

| `dump` | A dump may be |
|---|---|
| `unmasked` | raw (the old behaviour), or masked |
| `masked` | masked, or schema only |
| `schema` | schema only |
| `never` | nothing |

Defaults:

- **Protected environments: `dump = "masked"`, `encrypt` required,
  `dump_ttl = "14d"`, `dump_reason = "required"`.** Unprotected: `unmasked`,
  no encryption, `30d`, reason optional.
- **Until masking ships**, the protected default stays `unmasked` but takes
  the environment's name typed at a terminal (which `--yes` doesn't answer)
  and a warning that it copies personal data. The default moves to
  `masked` in the release that brings masking, so a working prod dump
  doesn't break before there's something better to use.
- `unmasked` for a protected environment is a deliberate act: written in
  the operator's own file, typed name each time, a reason, recorded in the
  journal, encrypted, and never over MCP.
- A project's `.pgman/pgman.toml` can tighten any of this (cap an
  environment at `masked`, require encryption) and never loosen it. A
  client that wants "pgman can't take a raw copy of our production"
  commits that cap in its own repo.

## 2. Masking that works out of the box

**No rules file is needed for a first masked dump.** pgman classifies every
column from the catalog (no rows read) and applies a built-in default to
each column it finds personal:

| Column | Default rule |
|---|---|
| email, login | a unique fake: `firstname.lastname.<hash>@example.test` |
| name (first, last, full, display) | a fake name from a built-in list |
| phone, mobile | a fake in Ofcom's drama ranges (01632 960…, 07700 900…) |
| address, street, city | a fake address |
| postcode | the district (`SW1A 1AA` → `SW1A`) |
| date of birth | an age band |
| IP address | an address in a documentation range |
| password hash | one known test password, printed at the end of the dump |
| token, secret, session, api key, refresh token | NULL |
| free text, JSON, bytes, unknown types | NULL |
| everything else | kept |

Framework presets apply automatically when their tables are found:
Flyway's and Liquibase's history tables kept; Spring Security's users table
gets the test password (with the `{bcrypt}` prefix it expects); session and
token tables (`spring_session*`, `persistent_logins`, OAuth client tables)
dumped with no rows; Envers `*_aud` tables follow their base table's rules.

So `pgman dump production` just works, and says what it did:

```
masked: 41 columns in 18 tables (12 email, 9 name, 6 phone, 4 address,
  6 free text → NULL, 4 tokens → NULL); 3 framework tables
test password for every account: pgman-test-3b7e
key: fresh for this copy (pseudonyms won't match another copy)
```

**Anything the classifier can't decide is NULLed**, never refused for want
of a rule. Refusal is for policy only. Nothing personal-looking leaves
unmasked unless someone said `keep`.

### The classifier

- **By name, on every type**, from a bundled vocabulary that includes:
  - contact and identity: email, mail, phone, mobile, tel, the name
    family, address, street, city, postcode, zip;
  - birth and age: dob, date_of_birth, birth, age;
  - identifiers: ssn, nino, national_insurance, nhs, passport, licence,
    iban, sort_code, account_number, card, salary, pay, income;
  - network and device: ip, mac, user_agent, device;
  - location: lat, lng, location;
  - secrets: token, secret, password, session, cookie, api_key, mfa,
    credential;
  - **special-category and criminal-offence data:** gender, sex,
    ethnicity, race, religion, health, medical, diagnosis, disability,
    sexual, union, political, biometric, genetic, criminal, conviction,
    offence.

  Name matching runs on every type, so an enum `gender` or a boolean
  `is_vulnerable` is caught.
- **By type**: `text`, unbounded `varchar`, `citext`, `json`, `jsonb`,
  `xml`, `bytea`, arrays of text, `inet`, `cidr`, `macaddr`, PostGIS types,
  and any type pgman doesn't know. **A bounded `varchar(n)` is judged by
  name only**: Hibernate makes every `String` a `varchar(255)`, and flagging
  them all would bury the real columns.
- **Not personal, by name**: `*_code`, `*_type`, `status*`, `currency`,
  `locale`, `sku`, `slug`; surrogate keys (`id`, an integer or uuid `*_id`
  that is a foreign key); the system's own timestamps (`created_at`,
  `updated_at`, `deleted_at`).
- **External identifiers**: a string `*_id`, `*_ref` or `*_number` that
  isn't a foreign key (a payment provider's customer id) is personal.
- **Large objects** are one item to decide, like a column: kept or left
  out.
- The vocabulary is extendable per project (`[classify] personal = […]`,
  `not_personal = […]`). Sampling values is used only to *suggest* (on the
  Masking screen and in validate), never to decide, so the same schema
  masks the same way everywhere, including CI.

## 3. Adjusting it: the rules file

Differences from the defaults live in a committed, reviewable file, one per
database: `.pgman/masking/<database>.toml`, found by walking up from the
current directory (or `--masking FILE`). It is created the first time
someone changes something, on the Masking screen (section 9) or by hand:

```toml
version = 1

[tables.customers]            # public.customers
notes = "keep"                # reason = "support notes, reviewed: no personal data"
contact = "email"             # a varchar(255) the classifier couldn't tell
tier = "keep"

[tables.customers.when]
keep_if = "email LIKE '%@ourcompany.example'"   # the team's own accounts stay as they are

[tables.users]
password_hash = { rule = "password", value = "pgman-test", prefix = "{bcrypt}" }

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
  both sides of a join decide the same way.
- **Dates shift per row**, keyed by the row's primary key, the same amount
  for every date in the row: an order is never shipped before it was
  placed. A partition-key date is never shifted.
- `sql` (an expression for what nothing else covers) may read only the
  row's own columns and an allow-list of functions — no subquery, no
  `FROM` — and is refused over MCP.

Transformers preserve the column's type and fit its length (read through
domains to the base type). A column under a unique index — expression and
partial ones included — gets a transform that stays unique (from at least
64 bits of the keyed hash); lossy transforms (generalisations, word-list
fakes without a hash suffix) are refused there.

## 4. The data path

1. One `REPEATABLE READ` transaction; `pg_export_snapshot()`; `ACCESS
   SHARE` locks on every table copied, as pg_dump takes; `statement_timeout`,
   `lock_timeout` and `idle_in_transaction_session_timeout` off; and
   `row_security = off`, so an RLS-filtered read fails loudly instead of
   dumping a subset. All of this works on a hot standby (verified).
2. `pg_dump -Fc --snapshot=<that snapshot> --exclude-table-data='*.*'`: the
   schema, every sequence's value and the large objects, **no table rows**.
3. Each table's rows (leaves for partitioned and inherited tables, `FROM
   ONLY`; stored generated columns left out) through `COPY (SELECT …) TO
   STDOUT` in the same snapshot. Only `NULL` and dropped columns are pushed
   into the `SELECT`; every other rule runs in pgman, so **the masking key
   never reaches the server** (not `pg_stat_activity`, not RDS statement
   logs).
4. Written as pgman's own archive, a directory:

```
production-ledger-20261028T1403Z.pgman-dump/
  header            # plain: format, pgman version, environment, database,
                    #        created, expires, masked, rules digest
  manifest.toml     # encrypted: tables, row counts, classifier results,
                    #            key fingerprints, word-list version
  schema.dump       # encrypted: the no-rows pg_dump archive
  data/0001.copy.zst …   # encrypted: one file per table, generated names
```

`pgman restore` reads it: `pg_restore --section=pre-data`, then the rows by
`COPY … FROM STDIN`, then `--section=data` (sequence values, large
objects), then `--section=post-data` (indexes, constraints, triggers), so
foreign keys are checked on the masked rows. A constraint error's `DETAIL`
(which prints the offending value) is redacted. Only `pgman restore` reads
the archive; pg_dump's directory format is a later goal.

Options after `--` are limited, on a masked or schema dump, to a short safe
list (`--exclude-table*`, `--exclude-schema`, `--no-comments`); anything
that could bring raw rows back (`--data-only`, `--section`, `--table`,
`--inserts`, large-object options) is refused.

## 5. Keys

**Masking key.** By default **a fresh random key per dump**, used and
discarded. Pseudonyms are consistent within the copy (joins hold) and match
nothing else, and nobody (the person who took the dump included) can test
a guess against them afterwards. Nothing to set up.

**A stable key, when results must repeat** — the same customer gets the
same pseudonym in today's copy and next week's, so a bug report can name
one — is an opt-in, at the operator's level and never from a project file:

- `masking_key = "secret:…"` on the environment: its own secret, not the
  database password, ideally in the client's AWS account (who can read it
  is an IAM decision, every read is in CloudTrail, the client can revoke
  it), at least 16 bytes; `pgman masking key new` creates one;
- optionally per rule (`stable = true`), so only the columns that must line
  up between copies use it and the rest stay fresh per dump;
- each dump records the key's fingerprint, never the key, so it's visible
  which copies can be linked; `pgman masking key rotate` breaks the link to
  every older copy (on a schedule, and when someone leaves);
- the bundled word lists are versioned and recorded, so an upgrade doesn't
  change everyone's fake name.

With a stable key, copies can be linked, and whoever holds the key can test
a guess. That is the trade for repeatability, made per environment by the
operator.

**Encryption key** — a separate secret, never derived from the masking key
and never wrapping it, so being able to restore never means being able to
reverse:

- `kms:alias/…` for AWS environments (one key per environment or per
  client): each dump gets a fresh data key from KMS; the archive is
  encrypted with age (the stock crate; no home-made streaming crypto); the
  header's fields are bound as the KMS encryption context and repeated
  inside the ciphertext, so editing them fails decryption. Restoring calls
  `kms:Decrypt` — an IAM decision, a CloudTrail line, gone with a leaver's
  AWS access, and disabling the key makes every copy unreadable.
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

- **Why.** A protected dump asks for a reason (`--reason "APP-101: invoice
  rounding repro"`, or the prompt), kept with the copy.
- **Where.** Dumps go to pgman's dump directory by default, with an
  explicit path allowed; pgman refuses to write a raw dump into a synced
  folder (iCloud Drive, Dropbox, OneDrive, Google Drive) and warns for a
  masked one, excludes every dump from Time Machine (`tmutil
  addexclusion`), and warns when FileVault is off.
- **Restored copies** are tracked too: `pgman restore` writes the copy's
  origin into the database it restores (a `pgman_provenance` table: masked
  or raw, source environment, when, by whom, expiry, the dump it came
  from).
- **`pgman copies`** lists every dump and restored database pgman knows
  of, with its age, expiry, reason and whether it's masked. `pgman copies
  rm <copy>` deletes the dump and drops every database restored from it,
  in one step; `pgman copies prune` does it for everything past its expiry.
  Expired copies are flagged in the TUI header and on the next dump.
- **Restore refuses** an expired dump, or a raw one past its time, unless
  told to (`--expired`).

## 7. The journal

Every step goes to an append-only journal, `pgman.journal` in pgman's data
directory, one JSON line per event:

| Event | Records |
|---|---|
| `dump` | who (git identity and, for AWS, the IAM identity), host, when, environment, database, reason, masked or raw, rules digest, key fingerprints, file, size, checksum |
| `restore` | the dump, the target database, when |
| `delete` | the dump file removed, the databases dropped, when, why (by hand, expired) |
| `expired` | a copy passing its expiry still present |

`pgman copies report [--since DATE]` prints the journal as a readable
account — taken, why, restored where, deleted when — for a client's DPO,
an audit, or the end of an engagement.

**The central journal is CloudWatch Logs.** Per environment, in the
operator's own `environments.toml`:

```toml
journal = "cloudwatch:/pgman/dumps"   # a log group in the environment's AWS account
```

pgman writes each event to that log group as it happens, with the
operator's own AWS credentials (`logs:PutLogEvents` on that group only),
one log stream per developer and machine. The local journal is written
first and is the developer's own record; a CloudWatch write that fails is
retried on the next pgman run and never blocks a dump, but a dump of an
environment with a `journal` set says when its events haven't reached the
log yet. The log group lives in the client's account, so its retention,
who may read it, and who may delete from it are the client's IAM and
CloudWatch settings, not pgman's. Events carry no values, no secrets and
no hostnames beyond the environment's name.

What the journal is and isn't: it is a faithful record kept by a tool used
in good faith, which is what a good developer needs to demonstrate good
practice. It is self-reported, and someone determined can step around it;
the trail no developer can edit is CloudTrail's — secret reads and, with
KMS encryption, every restore — and the journal carries the request ids to
match the two.

## 8. Validate

`pgman masking check <target>` reads the catalog only (no rows), runs in
CI against an empty migrated database, and exits non-zero when a column is
undecided by a rule the team relies on (strict mode), a rule no longer
fits the schema, or something new appeared since the last check.

`pgman dump <target> --validate` adds a sample, writes nothing, and prints
**counts, never values**:

- per column: values changed, unchanged, NULL;
- a "masked" column more than 90% unchanged (a broken rule);
- a rule that would break a unique index, a check, NOT NULL, a type or a
  length (tried with fake values, never production ones);
- re-identification risk: the smallest group of rows sharing all kept and
  generalised columns on a table, warned under 5 (`postcode district +
  birth year + tier` is often a group of one);
- rows a `when` condition kept as they were.

`--preview N` (original beside masked) is opt-in, needs a terminal and the
typed environment name for a protected environment, is capped at 20 rows,
and is refused over MCP and in machine-readable output.

## 9. In pgman's screens

Developers configure all of this without editing files:

- **A Masking screen** (from the environments view or the schema browser):
  every table and column, its classification, its rule — default, preset
  or the team's — with the risky ones highlighted (personal and kept, free
  text, JSON, undecided). Pick a rule from a list; see a sample *fake*
  value (never a real one); add a `keep` reason; save, which shows the
  change and writes `.pgman/masking/<database>.toml` with its comments and
  layout kept.
- **The environments view** edits the policy fields (section 1) with each
  one's effect explained, writing the operator's `environments.toml` in
  place (formatting and comments kept).
- **The copies view** is `pgman copies`: dumps and restored databases, age,
  expiry, reason; delete one or prune the expired from there.

Changes made on these screens are the operator at their own terminal, so
the new rules digest is confirmed as it is saved. A rules file changed by
any other route (an editor, a branch, an agent) is shown on the next dump's
confirmation as "rules changed since you last confirmed them: 3 columns
newly `keep`, 1 `sql`", and an MCP dump refuses rules that are uncommitted
or not yet confirmed at a terminal.

## 10. MCP

- `dump` takes at most `masked` from a protected environment, never
  `unmasked`; the operator's confirmation names the rules file, what
  changed in it since last confirmed, and what the classifier found.
- `sql` rules and `--preview` are refused over MCP.
- No MCP tool writes the rules file, the policy or the journal.
- **`query` refuses a database whose provenance says it's a raw copy of a
  protected environment**, so a restored production copy can't reach the
  agent's context. Masked copies are queryable.
- Validate's counts are available to an agent; values never are.

## 11. Testing

- The classifier, rules parser, each transformer, the policy merge: unit
  tests.
- **The leak test**, for every path: a fixture seeded with canary values
  (`canary-7f3a@leak.test`, a distinctive name, a phone number, one in a
  column default and one in a comment) is dumped masked, restored, and
  every byte searched — the archive, the journal, stdout, stderr, pgman's
  log, and a restore made to fail on a constraint. Any hit fails, and
  breaking a transformer must make it fail.
- Round trip: the masked copy restores; foreign keys, unique indexes and
  sequences hold; row counts match the manifest.
- Determinism: a fixed key gives the same bytes twice; a fresh one,
  different.
- Errors and `Debug` output never carry a value: values live in types that
  redact themselves.

## Phases

| Phase | What | Size |
|---|---|---|
| 1 | Policy fields; typed confirmation and warning for raw protected dumps; MCP cap; reason prompt; the journal, local and to CloudWatch; `pgman copies` list and `rm` for dump files | 2½ days |
| 2a | Classifier with defaults and presets; core transformers (keep, null, fixed, password, hash, email, name, phone, ip, per-row date shift, drop rows); FK following; the data path and archive; restore with redaction; provenance; the leak test; `docs/privacy.md`. **The protected default moves to `masked`.** | 6½ days |
| 2b | Encryption: KMS envelope and age; header binding; private temp for `--jobs` | 2–3 days |
| 3 | The Masking screen and the policy editor; the copies view | 3–4 days |
| 4 | Validate (check and sampled), including the re-identification warning | 2 days |
| 5 | The rest of the transformers (addresses, postcodes, generalisations, JSON paths, conditions); stable keys and rotation; the central journal | 3 days |
| 6 | Subsetting, outbound references first, with virtual and polymorphic references | 1–2 weeks, if asked for |

About three weeks to the end of phase 5; each phase ships on its own.

## Decided

1. Protected default: **`masked`**, from the release that ships masking;
   typed-name `unmasked` until then.
2. Masking runs **in pgman**, with only NULL and dropped columns pushed to
   the server.
3. Masking key: **fresh per dump by default; a stable per-environment key
   in the operator's own config when results must repeat**, optionally per
   column, rotatable.
4. Classification by **name and type** decides; sampling only suggests.
5. **`unmasked` stays** for protected environments, deliberate and
   recorded, never over MCP, capped by a project when a client asks.
6. **Encrypted** for protected environments: KMS for AWS, the operator's
   age key otherwise, a separate key from masking.
7. **Threat model**: hygiene for good developers and protection against
   agent mistakes and accidents; the journal is a faithful self-record,
   CloudTrail the record no one can edit.

8. **The central journal is CloudWatch Logs**, a log group per
   environment in the client's account, set in the operator's config.
9. **A privacy note ships with the feature**: `docs/privacy.md`, written
   for a client's DPO — what pgman masks by default and how, encryption
   and who holds the keys, the life of a copy, what the journal records
   and where, the threat model, and what pgman does not claim. It is
   written with phase 2a and kept true to what is built.

## Changes from the first draft

From the panel and Tom, 2026-10-28:

- **Defaults out of the box**: automatic masking from the classifier and
  framework presets, NULL for anything undecided. The first draft needed a
  rules file and refused without one.
- **Configurable in the TUI**: the Masking screen, the policy editor, the
  copies view.
- **Lifecycle and journal** (sections 6, 7), with the hygiene-not-policing
  threat model.
- **Rules can't be loosened behind the operator's back**: the key and
  policy only in the operator's file, digest confirmation, MCP refuses
  unconfirmed rules; `sql` bounded and terminal-only.
- **Restored raw copies are kept from the agent** (provenance, `query`
  refuses).
- **Data path corrected** (DBA review): one snapshot, sequences and large
  objects through a no-rows `pg_dump`, leaves with `FROM ONLY`,
  `row_security = off`, redacted restore errors.
- **Classifier**: name-first for bounded `varchar` (Hibernate), wider type
  net, special-category vocabulary, external identifiers.
- **Transformers**: type- and length-preserving, unique where indexed,
  per-row date shift, FK following from any referenced column.
- **Validate** split into a catalog-only check for CI and a sampled run,
  with a re-identification warning.
- **Encryption** added (section 5).
- **Pass-through** options limited on masked dumps.
- **Phases** re-sized (phase 2 was a third of its real size) and
  re-ordered so the default doesn't change before masking exists.

## Prior art

- **Greenmask**: pg_dump-compatible masking; deterministic hashing;
  transforms that follow keys and partitions; conditions; subsetting with
  virtual and polymorphic references; a validate command with a safe
  summary mode. Much of sections 3, 5 and 8 follows it.
- **PostgreSQL Anonymizer**: declarative masking as security labels, static
  and dynamic; needs the extension on the server, which managed databases
  often don't allow.
