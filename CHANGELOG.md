# Changelog

All notable changes to pgman are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims
to follow [Semantic Versioning](https://semver.org/) once it reaches 1.0.

## [Unreleased]

### Added

- The TUI's footer shows `EXPIRED ×N` while N dump copies are past
  their expiry with something left to delete (`pgman copies prune`).
- `pgman copies prune`: deletes every copy past its expiry, with the
  databases restored from it, as `copies rm` does, asking about each in
  turn unless `--yes`. A copy kept (not confirmed, sessions open)
  doesn't stop the rest. `pgman copies` points at it under the list.
- MCP `explain` suggests an index for a big sequential scan filtered by
  plain comparisons on columns no index of its table leads with: the
  `CREATE INDEX CONCURRENTLY` that could serve it, to weigh rather than
  run as-is.
- A patch dry run (`pgman apply --dry-run`, MCP `apply_patches` with
  `mode: "dry_run"`) now says what its patches change in the schema —
  tables, columns, keys and indexes added, dropped or changed — read in
  its transaction before the rollback. The TUI's patch view (`P`) lists
  them too.
- `pgman diff <A> <B>` and MCP `schema_diff`: what differs between two
  databases' schemas — tables and views in one only, and per table,
  columns added, dropped or changed, the primary key, indexes and
  foreign keys. For drift between environments; a report, not a
  migration.
- `pgman query --target ENV/DATABASE` (and `--batch --target`): run a
  statement against an environment's database, or a connections.toml
  name, with its secret and ssh tunnel, as `pgman dump` reaches it. Its
  replica, read-only, unless `--primary`; safety.toml's `ENV/DATABASE`
  keys apply.
- MCP `explain`: one statement's plan, with findings — big sequential
  scans, row estimates ten times off, sorts and hashes spilled to disk,
  nested loops run thousands of times. `analyze: true` runs it in the
  read-only session `query` uses, for actual rows and times.
- MCP `slow_queries`: the statements `pg_stat_statements` says cost the
  most, by total or mean time, as the TUI's slow-query panel reads them,
  for an agent to find the code that sends them.
- MCP `describe`: the tables a connection can see (kind, column count,
  size), or one table in full — columns, primary key, foreign keys both
  ways, indexes, size, partitions and its `CREATE` statement — from the
  catalog read the schema browser makes. An agent no longer hand-writes
  `information_schema` queries through `query`.
- `pgman doctor`: what pgman sees from the directory it runs in — each
  config file and whether it loads (exit 1 when one doesn't), the
  programs it runs and where they are, and every connection it knows,
  usable or why not. It connects to nothing and names where a password
  comes from, never the password.
- The environments overview (`E`) lists the `connections.toml` entries
  no environment names, under `connections`, and connects to them with
  their tunnel, secret and `password_env`: a first connection no longer
  needs an `environments.toml` too. The empty picker and the start card
  point at it.
- MCP's `dump` fills in the agent's `reason` (or the branch's ticket id)
  as the default of the operator's reason field, so the operator
  confirms it or edits it instead of retyping it. The operator still
  confirms every dump, and what they send is recorded. A suggestion the
  journal wouldn't take (over-long, several lines) isn't filled in.
- `restore_confirm = "clean_only"`, on a connection in connections.toml
  or an environment in environments.toml, lets MCP's `restore` create a
  new database on this machine without asking the operator (`create`,
  no `clean`, nothing of that name there). A `clean`, a restore into an
  existing database, and anything protected or off this machine are
  still asked about. The result says it ran unasked; the journal
  records it as before. Only the operator's own files can set it.

### Changed

- `D` with a pinned A from another table (or sharing no column with the
  current result) pins the current result as the new A, and says so.
  It diffed them, every row removed and added.
- A cell edit whose UPDATE matched no row now says a row-level
  security policy may be why (one can show a row and not let it be
  updated), as well as the row being deleted or its key changed.
- A rules file's confirmation lists every rule it sets, not only `keep`,
  `null`, `fixed` and `hash`. A `postcode`, `year` or `date.shift` is
  listed with what it keeps, because on a column the defaults mask whole
  it keeps more than they do: `postcode` keeps all but the last three
  characters of a value with no space in it. Such a file is no longer
  used unconfirmed without a terminal unless the environment is
  `unattended`. The confirmation also says a kept column keeps what a
  foreign key joins to it, and the dump's summary lists those columns.

### Fixed

- The schema wizard (`W`) says on its bottom border how many lines of
  the focused finding a short terminal cuts. The detail strip just
  stopped.
- Filtering the grid after scrolling down a long result shows the
  matches from the top. Only the last match showed, with blank rows
  under it.
- The result diff (`D`) matches B's columns to A's by name when a query
  lists the same columns in another order, so it keys on the primary
  key and compares column by column. Every row read as removed and
  added.
- The result diff (`D`) says when a side held only the rows fetched so
  far (a capped or paged result), and that a row past its end reads as
  added or removed. A pinned side forgot it was partial.
- The result diff (`D`) no longer keys rows on a column holding a NULL
  (one NULL on each side paired two unrelated rows), and a change line
  shows text reading `NULL` as `'NULL'`, apart from SQL NULL.
- A cell edit the server refuses (an invalid number, a constraint)
  opens the editor again holding what was typed, with the error below.
  It closed, and the value had to be typed again.
- MCP `query` and `explain` say why when the read-only session refuses
  a statement that writes (a function's INSERT, `nextval`): MCP runs
  every query read-only, and schema changes go through
  `apply_patches`. They gave the server's message alone.
- The row-as-INSERT yank (`I`), `:fixture` and cell editing no longer
  take a subquery's alias, a function or a CTE for a table:
  `SELECT * FROM (SELECT …) sub` was read as `public.sub`, which
  doesn't exist. They now say they need a single-table result.
- The sessions panel (`S`) stays open when it can't read
  `pg_stat_activity` and says why, as the slow-query panel does. It
  used to close at once, so the `q` pressed to close it quit pgman.
- A statement starting `explain8` (EXPLAIN's letters, then more) sent
  the safety classifier into endless recursion and crashed pgman with a
  stack overflow; a long run of `EXPLAIN EXPLAIN …` recursed as deep.
  Found by the new fuzz job's first run; the classifier now strips
  EXPLAINs in a loop and calls such a statement Other.
- The unknown-command message now lists `key` (and `doctor`) with the
  other commands.
- A DSN with no password stopped at "password missing" with no hint.
  It now says where a password can come from: the DSN, `PGPASSWORD`, or
  a connection's `password_env` or `secret`.
- `pgman query --dsn URL 'SELECT 1'` and `pgman query --json 'SELECT 1'`
  were refused, the SQL taken for a DSN: the SQL had to come first. It
  is now the first argument that isn't a flag, a flag's value or a
  `postgres://` DSN, wherever it comes.
- Masked dumps now mask a few more personal columns by name: WhatsApp
  numbers, Instagram, Facebook, TikTok, Telegram and Snapchat handles,
  and what someone typed into a form (`custom_field_values.value`,
  `survey_answers.answer`, a response's `response`).
- Masked dumps kept Flyway's `installed_by` and Liquibase's `author`
  as they were (people's names, in every row), since a migration tool's
  table is kept whole. Who ran a migration is now `pgman`.
- A masked dump's schema now leaves out subscriptions
  (`--no-subscriptions`, pg_dump 10 on): dumped as a superuser, their
  connection strings can hold another server's password.
- A masked dump of a PostgreSQL 18 table with a virtual generated column
  copied the column and named it in `COPY … FROM`, which PostgreSQL
  refuses, so the table's restore failed. Virtual generated columns are
  left to the server, as stored ones are.
- pgman ran `git status` and `git log` in the checkout to check a rules
  file, so a `.git/config` an agent wrote (an fsmonitor hook, a clean
  filter, a signature check's program) could run a program with the MCP
  server's environment. Every git pgman runs now turns those off, and
  "committed as it is" compares the file with its blob in HEAD instead
  of asking `git status`.
- MCP's `restore` with `clean` into a database named `y` was confirmed
  with a tick instead of the typed name: the name to type was inferred
  from the answer that accepts the question. It now comes from the plan.
- MCP's `apply_patches` confirmation shows at most 6000 characters of
  SQL; past that it was still confirmed with a tick, so a statement
  after the cut ran unseen. A cut confirmation now takes the
  environment's name typed.
- The editor ran a script with writes and its own `COMMIT` (or
  `PREPARE TRANSACTION`) inside pgman's `auto_tx` transaction, so its
  writes were committed at its `COMMIT` and the rollback pgman offered
  after undid nothing. Such a script is now refused while `auto_tx` is
  on, saying why.
- A read-only session could be made writable by `set_config` with a
  column for the setting's name
  (`SELECT set_config(n, 'off', false) FROM (SELECT
  'default_transaction_read_only' AS n) s`): a bare word was read as the
  name itself. Any first argument but a string literal not naming a
  read-only setting now counts as lifting read-only.
  `SET default_transaction_read_only = 'on'` is no longer refused as one.
- MCP's `restore` took any masked archive in the working directory
  without asking the journal, so an agent could build an archive of its
  own, whose schema pg_restore would run, and have it restored (without
  a question, under `restore_confirm = "clean_only"`). A raw `.dump` was
  known by its path, so one overwritten in place passed too. `restore`
  now takes only a dump this machine's pgman took, known by its
  content: an archive by its id and manifest fingerprint, a `.dump` by
  the sha256 the journal now records for each single-file dump. A dump
  taken before this release has no sha256 and is refused over MCP;
  restore it at a terminal or dump it again. pg_restore also reads a
  private copy taken and checked before the operator is asked, so a
  file changed while they read the question changes nothing; an
  archive's schema is always restored from such a copy, and each of its
  data files is checked against the manifest as it is loaded.
- MCP's `query` no longer lets a safety.toml guard set to `allow` reach
  the agent: such a guard counts as `confirm` there, which nobody can
  answer. With `ddl = "allow"` or `other = "allow"` for a database, an
  agent could run `VACUUM FULL`, `CLUSTER`, `REINDEX`, `CHECKPOINT` or
  `COPY … TO PROGRAM`, which Postgres runs even in a read-only session.
  The default guards were never affected.
- Masked dumps kept three kinds of personal column raw. A text column
  named `email_id`, `emailid` or `mobile_id` was taken for a key; it is
  now masked as an email or phone number. `born_at` was kept as a
  timestamp; it now keeps only the year, as `date_of_birth` does. `name`
  on a holder's table (`passengers`, `payees`, `beneficiaries`) is now
  masked as a person's name. Masked copies of a database with such
  columns should be taken again.
- Masked dumps judge a column whose name says nothing by its domain's
  name too, so `contact email_address` is masked as an email instead of
  kept. Masked copies of a database with such columns should be taken
  again.

## [0.5.2] — 2026-11-11

### Added

- safety.toml entries can be keyed `[databases."ENV/DATABASE"]` or
  `[databases."ENV/SERVER/DATABASE"]`, with environments.toml's names,
  so a demo database can be made writable without opening production's
  database of the same name. A qualified entry applies when patching, in
  `pgman run`, and in a TUI session opened from the environments
  overview, where it wins over the bare name. A project's `[safety]`
  block tightens qualified entries too.
- A patch plan lists every statement a `block` guard stops, with its
  file and line: all of them in the patch it stops at, and those in the
  pending patches after it, so one change to safety.toml covers the run.
- A patch can allow its own statement past a `block` guard with
  `-- pgman: allow <guard>` on the line before it. The plan names each
  one, and confirming takes the environment's name typed, so `--yes`
  doesn't confirm it. A mistyped or misplaced declaration refuses the
  patch.
- MCP's `apply_patches` puts a read-only database, or statements a
  guard blocks, to the operator instead of refusing: the confirmation
  lists each, and typing the environment's name lifts them for that one
  run, leaving safety.toml as it is. The agent can't ask for it, and
  what a project's `.pgman/pgman.toml` `[safety]` refuses on its own is
  never lifted. The result's `lifted_for_this_run` and pgman's log say
  what was lifted.
- MCP's `dump` lets the operator confirm a project's masking rules
  file that is committed but new or changed since they last confirmed
  it, instead of refusing until `pgman dump --masked` runs at a
  terminal. The question shows the file's commit and sha256, every
  column it keeps that the defaults would mask, with its reason, and
  every `null`, `fixed` and `hash` it sets; typing the database's name
  confirms. The agent can't confirm it, a decline records nothing, and
  an uncommitted or edited file is still refused. Confirmations, at a
  terminal or over MCP, are recorded with when and how.

### Fixed

- Applying patches when the first pending one is refused (read-only, or
  a blocked statement) says "refused, nothing applied" and why. MCP's
  `apply_patches` and `pgman apply` said "nothing to apply", which read
  as up to date; the TUI's hint now says the first patch is refused.
- A patch refused because the database is read-only names the file and
  the narrowest entry to change: the environment's own. It also says
  which tools read the change at the next apply, and warns that a bare
  `[databases.<name>]` entry opens that name on every server, protected
  ones included. `:readonly off` and `pgman run` name the same entry.
- A safety.toml key that can never match (`[databases."a/b/c/d"]`, or
  one with an empty part) is reported in the patch refusal, MCP's
  `doctor` and pgman's log rather than ignored.

## [0.5.1] — 2026-11-09

### Added

- pgman's MCP server is listed in the official MCP Registry as
  `io.github.tombaldwin/pgman`, which MCP directories read from.

## [0.5.0] — 2026-11-02

### Added

- Encrypted dumps. `encrypt = "kms:<key>"` on an environment wraps a
  fresh key per dump with AWS KMS, bound to the dump's header; `encrypt =
  "age:<public key>"` encrypts to your own key, made by `pgman key new`.
  Masked and raw dumps are both encrypted, pg_dump's output piped straight
  into the encryptor; `pgman restore` opens them. The journal records
  what each copy is encrypted to, and `pgman copies show` says it.
  `pgman key new|list` makes and lists your age keys.

### Changed

- **A protected environment's raw and masked dumps need `encrypt`.**
  Until the environment says `kms:`, `age:` or `none` (masked copies
  only), they are refused, with the lines that settle it. Schema-only
  dumps, and protected targets that aren't environments, are unchanged
  (the latter dumped masked and unencrypted, which is said).
- An encrypted raw dump is pgman's archive, a `.pgman-dump` directory,
  and its link `<label>-latest.pgman-dump`, not `<label>-latest.dump`:
  a script restoring the old link name finds the last unencrypted dump.
  Masked and raw copies of one label now share that link.

### Fixed

- Over MCP, the confirmation for `dump`, `restore` and `apply_patches`
  starts ticked. Claude Code kept showing the form again while its box
  was unticked, and the call failed after 300s with "no answer from the
  operator". Accept or Decline is the answer; a protected environment
  still takes its typed name.
- Over MCP, `dump` and `restore` given a connection name only
  discovery knows (one `connections` lists from the project) say so,
  and to pass its URL or add it to connections.toml. They used to say
  there was no such name.
- A restore's result says that owners and grants were left out, who
  owns what it restored, and how to grant another role (the
  application's) access: until then that role can read none of it.
- A dump refused because its user can't read everything names the
  first 20 tables and sequences, then counts partitions under their
  parent (`app.events: 106 partitions`). It used to name the first 8
  and say "and 106 more", which could hide the few tables that decide
  whether `--skip-unreadable` is safe.
- Masking rehashes a hash whose word isn't last
  (`long_value_hash_lower_case`), a bare `hash` on a cards or users
  table, and a card number's (`ccnum_hash`).
- A key into a lookup that refines another (`sub_religions` into
  `religions`) is NULLed as one into the lookup is; a text-keyed lookup
  of categories keeps its key; a lookup's translations keyed by a
  `language_id` aren't refused.
- The tap monitor's status says a sender's drop count is events
  "dropped in the app, never sent", not "JAR backpressure": OTLP
  senders never report one, and pgman's own drops are the footer's.
- The tap's transactions and pools views drop numeric columns on a
  narrow screen instead of their text column: at 60 columns a row
  ended on a bare `…`, and at 80 the transactions view read `con…` and
  the pools view showed the pool under a `POOL · APP` header with no
  app. The pools header names the app only when a row has room for one.
- A connection the server ends while a page of rows loads, a transaction
  commits or rolls back, or pgman is first connecting shows the
  connection-lost card with the server's reason, as a failed query's
  did. A failed `:readonly on` leaves the header as the session is.

## [0.4.4] — 2026-10-31

### Added

- Masked dumps keep Odoo's models, views and menus whole (their names
  are JSON, which the defaults NULL, and the restored app wouldn't
  start). Its attachments, settings and mail servers are masked as before.

### Fixed

- A key into a lookup of personal answers (`gender_id` into `genders`,
  `marital_status`, `country_of_birth`) took the lookup's kept id, or
  was hashed alongside it, so the copy still joined to the answer. It is
  NULLed now, a rules-file `null` on it too, and where it can't be NULL
  the plan asks the rules file to decide. The lookup's own key is kept.
  Masked copies taken with an earlier version of a schema with such keys,
  key-value or settings tables, data columns, or the hashes below should
  be deleted (`pgman copies rm`, then by hand whatever it says it kept,
  and any dump file copied elsewhere).
- Keycloak's session tables are dumped empty: their notes hold a
  device's address and details.
- A key-value table's value is NULLed whatever its length: Keycloak's
  `user_attribute.value` (a user's phone or address) and
  `realm_smtp_config.value` (the SMTP password) were kept as bounded
  `varchar`. So is a column named for a bag of data (`metadata`,
  `extra`, `raw_data`).
- A hash of a card, an account or a user (`card_hash`, `account_hash`,
  `user_hash`) is rehashed, not kept: there are few enough of each to
  try every one.

## [0.4.3] — 2026-10-31

### Added

- Masked dumps keep Rails', Django's, Alembic's and Knex's migration
  tables whole, as Flyway's and Liquibase's are, and dump Django's
  sessions empty.

### Fixed

- A CHECK constraint that only bounds a column's length, an array's
  cardinality, or asks for a JSON object no longer stops a masked dump:
  the empty value and a hash pass it for certain. A bound on a text
  column's length is taken as its length, so a hash or a fake fits it.
  On GitLab's schema this takes `pgman masking check` from 850 problems
  to 87 real ones.
- A domain's CHECK constraint (`CREATE DOMAIN … CHECK (VALUE <> '')`) is
  read too: a masked value it refuses stopped the restore part-way.
- A unique column too short for the hash that keeps it unique is a
  problem however the hash came about. A NOT NULL unique column NULLed by
  default is hashed, and a short one passed the plan and lost its unique
  index on restore.
- Masking keeps a date, time or boolean column named for a password or
  secret (`password_changed_at`, `token_expires`), which can't hold one.
- Masking finds comment, description and similar words inside a folded
  camelCase name whose other words it doesn't know. A free-text word no
  longer masks a
  number, date or boolean (`note_events`, a `failure_reason` code), which
  can't hold free text.
- Masking keeps settings named for a personal thing: a feature switched
  off (`emails_disabled`; a bare `disabled` is still special-category
  data), a theme, colour or mode (`login_theme`, `mobile_view_mode`; not
  an eye, skin or hair colour), a bounded template, layout or style, and
  a boolean about a signature. An email's subject or body is NULLed
  as free text, not given a fake address.
- Masking knows more personal words, found by running it over GitLab's,
  Keycloak's, Odoo's, Synapse's and AdventureWorks' schemas: an
  `identification_id` (a national ID number), a visa or work permit, an
  abbreviated `acc_number`, a bonus or commission, social handles
  (`linkedin`, `twitter`, …), OAuth device and user codes, an access key,
  sick leave, pronouns, a login or external id (`extern_uid`), a
  reviewer's or attendee's name, a partner's or guest's name, `reply_to`,
  an `sms_number`, a browser, a TLS fingerprint (`ja3`). A word with
  digits in it (`ipv4`, `last4`) now matches. A hash of something
  personal and easy to guess (`holder_name_hash`, `last_digits_hash`,
  `email_digest`) is hashed again, so it still joins; any other `*_hash`
  is kept, and the rules file's `[classify]` words reach both.
- `pgman copies rm` says what to do when it can't drop a copy restored
  through a URL with the password in it: drop it by hand, or restore
  through a `connections.toml` entry with `password_env`.

## [0.4.2] — 2026-10-30

### Fixed

- Masked dumps judge a camelCase column name that Postgres folded to
  lower case by its words: an unquoted `dateOfBirth` is stored
  `dateofbirth`, which pgman read as one unknown word and kept. On such
  a schema a masked dump kept dates of birth, phone and mobile numbers,
  email addresses, password hashes, bank sort codes and account numbers,
  passport and social security numbers, and special-category columns
  such as sexual orientation. Run `pgman masking show` again on a schema
  named this way; copies taken with 0.4.0 or 0.4.1 should be deleted
  (`pgman copies rm`). A phrase such as "account number" now matches
  `accountnumber` too, and "social security", "building society" and
  "account name" are known.
- `pgman masking check`, and a masked dump, stop at a masked column a
  validated CHECK constraint names when its masked value may fail it
  (`wage > 0` on a nulled `NOT NULL` wage became 0): the restore stopped
  part-way at the first such row, after the whole dump.
- `pgman copies rm` names the database it keeps and why; one restored
  through a URL with a password in it said only "invalid configuration:
  password missing".

## [0.4.1] — 2026-10-29

### Fixed

- Slow queries work on PostgreSQL 12, which names `pg_stat_statements`'
  times `total_time` and `mean_time`.
- Masked dumps work on PostgreSQL 10 and 11.
- A restore into a database it created counts "already exists" from
  the new database's template as harmless and names it, instead of
  saying the database wasn't empty: every restore into YugabyteDB
  exited 1 for its own foreign server.
- The schema wizard and `schema_lint` leave alone what is in a schema
  an extension made, such as TimescaleDB's chunks.
- A masked dump of a database with TimescaleDB hypertables stops and
  names them, instead of restoring them as plain tables without the rows
  of their compressed chunks.
- The schema wizard's "foreign key without an index" (LINT101) no
  longer flags a key whose column leads an index: it read the index's
  second column.

## [0.4.0] — 2026-10-29

### Added

- Masked dumps: `pgman dump --masked`, the default for a protected
  environment, takes a copy with what looks personal replaced as it is
  read — emails, names, phones, postcodes, dates of birth, IPs and
  passwords by realistic fakes, secrets, special-category data,
  identifiers, addresses and free text removed — judged from each
  column's name and type, with framework presets (Flyway, Liquibase,
  Envers, Spring Session). Foreign keys keep joining; a fresh key per
  copy, never sent to the server. One snapshot, a `.pgman-dump`
  directory with checksums, which `pgman restore` loads rows-then-keys.
- `.pgman/masking/<database>.toml`: the team's own rules (keep with a
  reason, fixed, hash, email, name, phone, ip, postcode, year,
  date.shift, rows dropped), and `pgman masking show` / `check` to see
  and test them from the catalog alone, in CI too.
- `docs/privacy.md`: what pgman does with personal data, for a client's
  DPO.

- `pgman copies`: every dump taken on this machine, from a journal
  pgman now keeps (`~/.local/share/pgman/journal.jsonl`): when, of what,
  raw or schema, why, the file, when it expires, where it was restored.
  `copies show` gives one in full, `copies rm` deletes one (and repoints
  its `-latest` link), `copies report` prints the whole account as text,
  markdown or JSON. No data, connection details or secrets are recorded.
- A dump policy per environment in `environments.toml`: `dump`
  (`unmasked`, `masked`, `schema`, `never`), `dump_ttl`, `dump_reason`
  and `unattended`. A project's `.pgman/pgman.toml` can lower an
  environment's (`[dumps.environments.<name>] dump = "schema"`), never
  raise it.
- `journal = { group = "/pgman/dumps" }` on an environment sends the
  journal's events for its copies to that CloudWatch Logs group with the
  `aws` CLI, as the IAM identity that sent them: file names without
  paths, no server addresses. A send that fails never fails the dump and
  is retried by the next `pgman dump` or `pgman copies`; each event is
  sent once.
- `pgman dump --schema-only`, `--reason` and `--ticket`. The reason
  prompt offers the ticket in the branch's name (`feature/APP-101-…`),
  else the last reason given; `[dumps] ticket_url` links tickets in
  `pgman copies`.

### Changed

- A dump of a protected environment asks why it is taken, and a raw
  dump of one (every row) also takes the environment's name typed,
  after a warning that it copies personal data to this machine. Without
  a terminal, a raw dump of a protected environment is refused unless
  the environment says `unattended = true` in your own
  `environments.toml`; scripts that need only the structure use
  `--schema-only --reason …`. Other environments are dumped as before.
- A protected environment is dumped masked by default; a raw copy of
  one needs `dump = "unmasked"` on it in your own `environments.toml`,
  `--unmasked`, and its name typed at a terminal.
- A dump with no FILE goes to `~/.local/share/pgman/dumps` rather than
  the current directory. A raw dump of a protected environment is
  refused in iCloud Drive, Dropbox, OneDrive or Google Drive; every dump
  is excluded from Time Machine.
- `pgman copies rm` also drops the databases restored from a copy (only
  local, unprotected ones whose comment names it).
- MCP's `dump` takes a protected environment masked (or schema only),
  using a rules file only when it is committed and you have confirmed it
  at a terminal; a raw dump of one over MCP, which v0.3 allowed after its
  name was typed, is refused, and every MCP tool refuses a database
  restored from one. The reason is given by you in the confirmation.
  Each MCP dump and restore is journaled.
- `environments.toml` has new keys (`dump`, `dump_ttl`, `dump_reason`,
  `unattended`): an older pgman reading a file that uses them refuses
  it as unknown keys.
- MCP's `restore` takes a masked dump, or a dump this machine's pgman
  took; a dump the journal doesn't know, or a raw copy of a protected
  environment (copied or moved too: each dump's fingerprint is
  recorded), is refused, yours to restore at a terminal.
- A restore that fails part-way is journaled, and the database it made
  is marked as the copy's from the moment it exists.
- A dump of a database restored from a raw copy of a protected
  environment is treated as a dump of that environment.

### Fixed

- `pgman … | head` (any command whose reader goes away early) ends
  quietly instead of aborting.
- `pgman restore` into a protected environment of replicas only says it
  is protected, not to name or add its primary.

## [0.3.1] — 2026-10-28

### Changed

- The Homebrew formula depends on `libpq`, so `pgman dump` and `pgman
  restore` work from a brew install with nothing else installed or
  linked: its wrapper tells pgman where libpq's `pg_dump` and
  `pg_restore` are (`PGMAN_PG_BIN`).
- `dump` and `restore` look for the tools in `--pg-bin`, then
  `$PGMAN_PG_BIN`, then Homebrew's libpq, then `PATH`, and say which
  they ran. Homebrew's libpq now comes before `PATH`, so an older
  PostgreSQL on `PATH` doesn't win over it.
- A pg_dump older than the server says `brew upgrade libpq` too, since
  the libpq found may be the stale one.

## [0.3.0] — 2026-10-28

### Highlights

- **A local copy of an environment in two commands.** `pgman dump
  staging/ledger` takes it from the replica through the bastion, the
  password read at that moment and never on a command line or on disk;
  `pgman restore … local --as ledger_copy --clean` puts it into a local
  database, never a protected one.
- **Scripts and patches from the command line.** `pgman run` runs a
  script that manages its own transactions, `\c` between databases;
  `pgman apply` is the environments view's `P`, `--outside` included.
- **For a coding agent:** MCP `dump` and `restore`, each confirmed by
  you, so an agent can refresh its copy without ever holding the
  environment's credentials.

### Added

- `pgman dump <target> [FILE]`: a dump by your own `pg_dump`, through
  pgman's connection — the environment's replica unless `--primary`, the
  password read from its secret at that moment and given to pg_dump only
  in its environment, the ssh tunnel opened for it. The local pg_dump's
  version is checked against the server first; the dump is written to a
  hidden name and renamed into place on success, private to you, with a
  `-latest` link for a default name. A replica cancelling the dump (a
  recovery conflict) and a permission failure are explained. What the
  dump's user can't read (no SELECT, no USAGE on its schema, row-level
  security, large objects) is found before pg_dump starts and refused
  by name; `--skip-unreadable` leaves exactly that out and says so. A
  sequence that a readable table owns can't be left out, and is named
  with the GRANT that fixes it. See
  `docs/dump-restore.md`.
- `pgman restore <FILE> <target>`: a dump restored by your own
  `pg_restore`, never into a protected environment and only on this
  machine unless `--allow-remote`. `--as NAME` restores into another
  database, `--create` makes it, `--clean` replaces it; other sessions
  refuse it unless `--terminate`; it is confirmed, or `--yes`. It exits
  1 when pg_restore went past real errors, counting a newer pg_dump's
  `SET` of a setting the server doesn't have as harmless. `--jobs`,
  `--keep-owners` and options after `--` go to pg_restore; a plain SQL
  dump is psql's.
- `pgman run <target> <FILE>`: a script that runs its own transactions
  (`BEGIN … COMMIT`), as `psql -f` does, with `\c DATABASE` moving it
  to another database of the environment. Checked against safety.toml
  first; confirmed (a protected target by typing its name); a failure
  or Ctrl-C reports what stayed committed, what rolled back and what
  never ran. `VACUUM` and `CONCURRENTLY` run without the statement
  timeout; a standby is refused. See `docs/run.md`.
- `pgman apply <target> [--outside] [--dry-run | --copy]`: the
  environments view's `P` from the command line. An `--outside` plan or
  a protected environment takes the typed name, which `--yes` doesn't
  answer.
- MCP: `dump` and `restore` tools, each confirmed by the operator
  through elicitation (a protected environment, or a database to be
  replaced, by typing its name); files stay inside the server's
  working directory, and a restore is local and unprotected only.
- A `<target>` on the command line is `ENV`, `ENV/DATABASE`,
  `ENV/SERVER/DATABASE`, a `connections.toml` name, or a URL. It is
  protected by its environment, by "prod" in its name, or by being on a
  server a protected environment reaches, however that is written.
- `pgman dumpp` says `unknown command "dumpp" (did you mean dump?)`.
- Ctrl-C, SIGTERM and SIGHUP stop `dump`, `restore`, `run` and `apply`
  cleanly: the tool stopped, a partial file removed, a transaction
  rolled back.

### Fixed

- Ctrl-C in the editor (and Esc twice while patches run) now cancels
  the running statement on a server reached with `sslmode=require` or
  `verify-*`, such as RDS: the cancel request went out without TLS,
  which the client library refuses for such a session, so it never
  reached the server.

## [0.2.1] — 2026-10-24

### Fixed

- The help overlay names the JDBC tap and the NOTIFY panel by `:tap` /
  `:notify` first, with `F4` / `F3` beside them, and the start card
  does the same for the tap: on a Mac terminal the F-keys often never
  reach pgman. The section list's teasers lead with their key
  (`:tap / F4 · your app's statements, live`, `S · schemas, …`), so it
  survives at 60 columns, and each fits whole at 80; the tap's ended
  `(:…` at 80x24.
- The result diff's title names its key (`key: id (primary key)`) before
  the A and B query text, so an 80-column terminal no longer cuts it
  off; a title too long for its border ends on `…`. A diff of results
  with different columns says `columns differ` in its title and under
  the counts, which read as a fault (`key: full row`, every row added
  and removed) without it.
- `:fixture` and `Y` on a partial result end their status on "fetched
  rows only, more not fetched", which the footer keeps whole; at 160
  columns it was cut mid-note, along with the file name.
- The schema wizard's detail pane wraps between words (it broke
  `referen` / `ces` at 80x24), and its list sizes the object column to
  leave the title room: at 80 columns the title column was three
  letters wide and its header read `tit`.
- A result column's header squeezed with its column ends on `…`, as
  its cells do, and keeps its sort marker: at 80x24 `billing_email`
  read `billing_` over cells reading `billing…`.
- The result title is fitted to its border: with a filter on and
  five-digit counts, an 80-column title ran into the corner mid-word;
  it now drops whole segments from the end and ends on `…`.
- The JDBC tap's views cut a long cell with `…` rather than at the
  border: N+1 and callers keep the caller whole and add the fingerprint
  only when there is room to read one, hotspots' `fingerprint · caller`
  and transactions' `txn · last sql` fit their column (at 80x24 the
  hotspots header read `FINGERPRINT · LAST CA` over `· InvoiceS`), and
  any row or header too wide for the panel ends on `…`.
- The NOTIFY panel cuts a long payload, and its title's key hints, with
  `…` at the border rather than mid-token, and shows a payload's
  newline as `⏎` on its row.
- The start card's two key columns line up: `:tap  JDBC tap (F4)`
  started four columns right of the `S` / `T` / `L` / `ctrl-t` above
  and below it, as `ctrl-l`'s key is wider than the others. At 100
  columns the card takes the short `ctrl-l` wording so
  `S  schema browser (26 tables)` fits, and a right-hand hint too long
  for a narrower card ends on `…` rather than at the border.
- A start card too short for every key hint keeps its `?  all keys`
  line and says how many it left off (`+3 more`); at 60x16 the hints
  went with nothing to say so.
- The README lists cell editing (`u`) and paging (`M`), and its links
  to `docs/` and the licence files are absolute, so they resolve on
  crates.io too; it says to run `brew trust tombaldwin/tap` first on
  Homebrew 6 or newer.
- `--demo` no longer writes `pgman.log` or creates pgman's cache,
  config and data directories, as its "no disk writes" promised, and
  `\fixture` / `\report` with no path write nothing to the cache
  dir: they say `--demo writes no files` and ask for a path.
- docs/tap.md says the monitor's "JAR backpressure" drop count comes
  from TCP and UDP senders only, and never appears with `--tap-otlp`.
- A connect no longer hangs at "connecting…" behind another session's
  `ACCESS EXCLUSIVE` lock: the schema fetch skips the sizes and indexes
  the lock covers, the connect's own catalog reads give up after a 2 s
  `lock_timeout` (the session's own setting is restored), and the status
  line says the schema is incomplete, names the holder's pid first (a
  narrow footer cut it off) then the lock — for a read that gave up
  behind a lock on a catalog too — and how to reload. The schema
  browser says `size: not fetched (locked)` and `indexes: not fetched`
  for such a table, which read as a table with no indexes. A reload (`S`
  then `r`, or the refresh after DDL) is bounded the same way, puts back
  your own `lock_timeout` with nothing of yours running in between, and
  no longer says "schema reloaded" over a read the server cancelled: any
  failed catalog read is named. Inside an open transaction it reads
  nothing and says so, since a read the server cancelled there would
  abort your transaction.
- A session the server ends under a running statement
  (`pg_terminate_backend`, a crash shutdown) shows the server's reason
  on the connection-lost card, not "the connection to the server closed"
  with the reason in the footer; a session lost while still connecting
  keeps its reason too. The server's hint comes with it: a hot-standby
  conflict's says to reconnect and repeat the command.
- The header no longer says `Session: writable` on a standby whose
  connect-time probe went unanswered: when a paged run's probe finds the
  server in recovery, the session is pinned read-only there, in the
  header and on the server, as the connect would have pinned it; nor
  after a `:readonly on` on a standby that failed to apply, or a
  `:readonly off` the server took just as the standby was pinned.
- A primary is no longer pinned read-only as "a read replica" when a
  schema ahead of `pg_catalog` on the search path has a
  `pg_is_in_recovery()` of its own: the connect's probe, and MCP's,
  the dashboard's and the advisor's, call `pg_catalog`'s. So do
  `apply_patches`' `statement_timeout` restore and the sessions panel's
  terminate, which a shadow could make report a session ended that
  wasn't.
- Under a read-only session the confirm card no longer says Postgres
  will refuse a statement pgman can't classify, such as `SET`, which it
  runs: it says Postgres refuses it if it writes.

### Changed

- The header and About box say the date beside the version is the
  release date: `pgman 0.2.0 · released 2026-10-23`. At 60
  columns the header now shows the version alone.

## [0.2.0] — 2026-10-23

### Highlights

- **Edit a cell in place.** `u` on a result cell writes a one-row
  `UPDATE` by primary key, shown in the same safety confirm as any
  statement you run.
- **Page past 1000 rows.** `M` fetches the next 1000 rows of a
  `SELECT`, through a server-side cursor that pgman closes after 60 s
  idle, 10 min open, or before any other work; a standby holds none.
- **A config advisor in the schema wizard (`W`).** Read-only findings
  on cache hit ratio, temp-file spills, dead tuples, lock waits and
  checkpoints, each with its evidence, a conservative setting and the
  RDS / Aurora parameter to change.
- **The whole JDBC tap event.** `Enter` on a tap row shows the full
  SQL, every bound parameter, the error's cause chain and the caller
  stack; the list shows the focused statement's parameters beneath it.
- **Help in sections.** One section at a time instead of one long page,
  with a summary for each and a filter across them all.
- **More logs read.** Logs → SQL now reads Postgres's `csvlog` and
  `jsonlog`, p6spy and datasource-proxy logs, and statements that
  failed with an `ERROR:`.
- **`sslrootcert`** in a connection string pins the trusted CAs, as in
  libpq.

### Added

- Cell editing: `u` on a cell in the grid (or a field in row detail)
  edits it in place. Enter writes a one-row `UPDATE … WHERE <primary
  key>` and sends it through the safety guard like any editor statement,
  the statement shown in the confirm; `Ctrl-N` sets NULL (and NULL ↔ the
  empty string). It works on a SELECT from one table with its primary
  key in the result, and otherwise says why not (a join, a view, no
  primary key, the key not selected, a computed column, read-only). The
  UPDATE changes exactly one row or is rolled back, and the cell then
  shows the value as the server stored it. Before it commits, pgman
  checks the server still calls the table, the column and the key what
  pgman's schema cache does; a column renamed since pgman read the
  schema (and another added under its old name) is rolled back with
  `the table changed since pgman read it`, never written to the wrong
  column.
- `M` in the results fetches past the first 1000 rows. A plain `SELECT`
  from the editor now reads through a server-side cursor; while it has
  more, the title says `partial · M fetches more`, and each `M` appends
  1000 rows (up to 100 000 held), keeping the sort and filter. While
  the cursor is open the session is idle in transaction, holding an
  `ACCESS SHARE` lock on the tables the query reads: DDL on them (a
  migration, `ALTER`, `TRUNCATE`, `VACUUM FULL`, `REINDEX`) waits behind
  it, and later statements on those tables queue behind that DDL. pgman
  closes the cursor after 60 s without a fetch or 10 min open (sooner if
  the server's own timeouts are), before any other database work, and
  at the end of the result; `M` then re-runs the query. On a standby
  no cursor is held at all (a recovery conflict would end the session):
  the first page commits at once and `M` re-runs the query for more.
  `EXPLAIN`, scripts, `\watch`, locking `SELECT`s, a `SELECT` that calls
  a function other than a short list of read-only built-ins (`count`,
  `coalesce`, `lower`, `now`, …) — or one of those names that a schema
  has its own function for — and a `SELECT` inside an open
  transaction stay capped at 1000 as before.
- Config advisor in the schema wizard (`W`): read-only findings from the
  server's statistics — buffer cache hit ratio, temp-file spills (with the
  spilling statements from `pg_stat_statements`), dead tuples and the
  autovacuum setting behind them, lock-wait chains, and requested
  checkpoints (`pg_stat_checkpointer` from PG17), blamed on
  `max_wal_size` only when `pg_stat_wal` shows the WAL to have forced
  them. Each shows its
  evidence, how long the counters have accumulated, a conservative
  setting, and its RDS / Aurora parameter-group name, static or dynamic.
  `e` yanks a finding's evidence query.
- `Enter` on a JDBC tap List row opens the whole event: the full SQL,
  `pool` / `conn` / `txn`, every bound parameter, the error's cause chain
  down to the root cause, and the caller stack. A failed row used to be
  only coloured red, and only the innermost caller frame reached the
  Callers view.
- The JDBC tap's List view shows the focused statement's bound
  parameters beneath the list, labelled `params (some redacted)` when
  the producer masked a value and `params (withheld by source)` for
  OpenTelemetry spans, which carry none.
- `sslrootcert=<file>` in a connection string pins the trusted CAs, as
  in libpq (and `require` with a CA file checks the chain). It used to
  be ignored, with the default trust store used instead. `sslcrl` and
  `sslcrldir` are refused rather than ignored.
- A Postgres log's `ERROR:` + `STATEMENT:` pair is reconstructed as a
  query marked failed with the error message — what the default
  `log_min_error_statement` logs with nothing else on. A statement
  `log_statement` already logged is marked failed rather than listed
  twice. The log picker marks a failed pick `✗` and shows its error.
- Logs → SQL reads Postgres's `csvlog` and `jsonlog` (PG 15+) formats as
  well as stderr: same pairing of parameters and errors, by pid.
- Logs → SQL reads p6spy (default single-line format) and
  datasource-proxy query logs, tagged `[p6spy]` / `[ds-proxy]` in the
  picker.
- `Tab` after `:help ` completes the help section names, as it completes
  command names.
- `docs/tap.md`: the JDBC tap for pgman users — the OpenTelemetry, TCP
  and UDP ways in, the v1 event fields, the limits and drop counts,
  `--tap-record` / `--tap-replay`, redaction and security.

### Changed

- `Ctrl-C` in the results cancels a running query, as it does in the
  editor, instead of arming quit; with nothing running it still asks
  for a second press.
- `Y` and `:fixture` say `fetched rows only` when the result has more
  rows than pgman holds.
- The result diff (`D`) matches rows on the table's real primary key
  when A and B are the same columns from one table and the key is in
  both (`key: id (primary key)` in the title), instead of the leftmost
  column that happens to be unique; otherwise it infers as before.
- Help is a list of sections and one section at a time instead of one
  ~275-row page: from 110 columns the list sits beside the section, and
  on a narrower terminal the list comes first, each section with a
  one-line summary, and `Enter` opens one. `Tab` / `]` / `[` move between
  sections, `/` filters every section live (matching rows under their
  headings), and `:help all` keeps the single page. `:help <topic>`
  takes any section name or the start of one, and an unknown topic lists
  every section.
- One size format everywhere: database sizes on the start card and in
  the environments overview, and table sizes in the schema browser, read
  `9.0 MB` / `212 MB` / `1.2 GB`. The start card used `pg_size_pretty`,
  which stays in kB up to 10 MB (`10199 kB` beside `10 MB`), and the
  schema browser used `MiB` with two decimals.
- In the editor and the prompts the error footer points at `F2 / :error
  detail`, not F2 alone: F2 takes fn on a Mac.
- The start card's `recent` list says how to bring an entry back: `in
  the editor, ctrl-p recalls · ctrl-r searches`.
- Loading a saved query (after its `:param` values, if any) ends the
  status with the key that runs it: `⏎ runs it`, or `alt-⏎ runs it` when
  the statement has no `;` and Enter would be a newline.
- The tab bar's hint reads `[ ] prev/next tab` instead of `] [ switch`,
  which looked like a stray pair of brackets.
- One TLS crypto provider (ring) instead of two: no cmake / C build of
  aws-lc, and a smaller binary (x86_64 macOS 11.9 MB to 9.1 MB). The
  Postgres connection no longer offers the post-quantum X25519MLKEM768
  key exchange, which only the aws-lc provider had.
- Logs → SQL: a Postgres `FATAL:` or `PANIC:` followed by its
  `STATEMENT:` (a read replica's `terminating connection due to
  conflict with recovery`) is reconstructed as a failed query, like an
  `ERROR:`'s.

### Fixed

- `:readonly on` under a writable profile could be undone from the
  editor: `SET default_transaction_read_only = off` got the `other`
  guard's confirm, and after `y` the session wrote while the header still
  said `read-only`. A session read-only by `:readonly on` or a standby's
  pin now blocks that `SET` (and `BEGIN READ WRITE`, `set_config(…)`) as
  a read-only profile does, and says `:readonly off` is the way out.
- A connection that dropped under a running pgman — a session the server
  ended (`pg_terminate_backend`, a restart, an idle timeout) or a network
  that went — left the header saying `connected`, and every later command
  failed with `connection closed`; the log said only `postgres connection
  error: db error`. pgman now shows the connection-failed screen, with
  `connection lost` and the server's own message (`terminating connection
  due to administrator command`), and `r` reconnects; the log keeps the
  message and its SQLSTATE.
- A long pattern typed into a footer prompt (the schema browser's and
  help's `/` filter, the grid's `/` filter and `f` find, `ctrl-r`
  history search) no longer loses its `filter: /` / `find:` head or the
  cursor: the head stays, the pattern shows its end after a `…`, and
  the cursor sits after it. The key hints go first.
- The JDBC tap monitor read "no events yet", and the footer showed no
  `TAP` badge, while only commits / rollbacks had arrived; the status
  now counts them (`2 commits/rollbacks`).
- The schema wizard, slow queries, sessions and notifications lists,
  and the JDBC tap's List, Hotspots, Callers, Transactions, N+1 and
  Baseline views, scrolled as if their header row were a list row, so
  moving the cursor past the bottom left the focused row hidden one
  line below the edge.
- After `SELECT … FROM ONLY t`, `I` yanked an `INSERT INTO public.only`:
  `ONLY` was read as the table's name. It is now skipped; a quoted
  `"only"` or a schema `only.t` is still a name.
- Dependencies: `ctutils` 0.4.5 (RUSTSEC-2026-0385, non-constant-time
  comparisons in the SCRAM path's digest crates) and `clipboard-win`
  5.4.3 (5.4.1 was yanked).
- `lo_from_bytea`, `lo_put`, `lo_unlink`, `lo_creat`, `lowrite` and
  `lo_truncate` classified as plain reads, so a writable connection ran
  them with no confirmation. They now need one, like other writes.
- If the TLS connector couldn't be built, a `require` / `verify-*`
  connection fell back to plaintext. It now fails instead.
- Help opened from a mode (`F1` / `?`, or `:help editor`) now shows the
  section's heading; it was hidden under the `↑ N more above` marker,
  so the section opened on its first key.
- The help overlay's footer shows its own keys; a status left from before
  help opened used to take the footer instead.
- The digits `1`–`9` no longer switch editor tabs while help is open:
  the tab changed unseen, behind the page being read.
- `I` (row as INSERT) named a quoted mixed-case table bare: a row
  yanked from `"OrderLine"` was inserted into a lowercase twin
  `orderline` if there was one, and failed otherwise. The FROM parse
  now folds unquoted names to lower case and keeps quoted ones (spaces
  and `""` included), and the INSERT quotes the table. `:fixture`
  refuses such a table instead of capturing it under the twin's name:
  DBUnit names tables case-insensitively, so no fixture can name it.
  `F` (FK follow) prefers the exact table over a case-insensitive match.
- A fixture's apply script quotes a reserved table or column name
  (`user`), which was a syntax error, and still reads every other
  name as DBUnit does, case-insensitively (`<UserAccount Id="1">` is
  `useraccount (id)`). `:fixture` refuses, naming it, a column that
  can't round-trip — one with upper-case letters (`createdAt`) or one
  XML can't carry (`a"b`, `with space`, `?column?`) — instead of
  renaming it with `_` into a column the table doesn't have.
- `\fixture`, `\report` and `\i` (and their `:` spellings) take the
  rest of the line as the path, so a path with spaces works; a
  double-quoted path has its quotes removed. They took the first word,
  so `:fixture my dir/f.xml` wrote `./my`.
- `I` (row as INSERT), `F` (FK follow) and the fixture apply script
  write a value holding a backslash as an `E'…'` string with the
  backslash doubled, so it inserts the same value whatever
  `standard_conforming_strings` is. With it off, a bytea's `\x0102`
  was read as the escape `\x01`.
- `I`'s footer counted the INSERT's bytes and called them characters;
  it counts characters now. Every yank reports a clipboard failure the
  same way (`yank failed: …`), and the test suite no longer writes to
  the system clipboard.
- `--batch` with a script of several statements, one of them a
  `SELECT`, says on stderr that a script prints only its status, not
  rows: `--format json` printed `[{"status":"batch executed"}]` and the
  rows seemed lost. Run the `SELECT` on its own to get them.
- `:fixture` with a grid filter active captures the rows the filter
  shows, as `Y` exports them, and the status says `N of M row(s)
  (filtered)`. It captured every row, hidden ones included.
- Sorting a column (`s`) decides once per column whether to compare
  numbers or text: a column that is all numbers sorts by exact value
  (int8 past 2^53 no longer ties, `NaN` sorts above `Infinity` as in
  Postgres); any other column sorts as text, byte by byte. Deciding per
  pair was not an order (`2 < 10 < 1a < 2`), so ascending and descending
  disagreed, and text such as `inf` or `nan` sorted as a number.
- Connected to a standby by `:connect` or `--dsn`, nothing said it was
  one; only an environment naming a replica did. Every connect now asks
  `pg_is_in_recovery()`, and the header wears a `replica` badge on a
  standby whichever way it was reached. The session is pinned read-only
  there too, as an environment's replica is, whatever `safety.toml`
  says: the header and start card say read-only, and `u` and
  `:readonly off` say it is a replica instead of sending a write the
  server refuses.
- A result stopped at 1000 rows said only `capped at 1000`, which read as
  though that was the whole table. It now says `more rows not fetched`,
  and the status line adds that `pgman query '…' --json` returns every
  row.
- The completion popup's title was cut to `Tab to cyc`: its width was
  measured without the title's decoration.
- Counts read as English: `1 match · Tab to cycle`, `4 saved queries`,
  `2 environments · 6 databases` (the bare first number now says what it
  counts), and the row-detail title `row 1 of 50 · field 1 of 24` like
  the cell detail's. Row detail's `↓ N more below` counts fields, as the
  title does, rather than wrapped lines of text (`↓ 3 more fields
  below`); the cell detail says `more lines`.
- A footer tip too wide for the terminal cut a key hint down to its
  label (`… expand-all/collapse-all`, `y… suggestion`). Key hints are now
  dropped whole, from the end; prose goes before them, and a prose
  segment that would shrink to a stub (`r…`) is dropped whole too.
- `pgman --dsn X query 'SQL'` failed with clap's "unexpected argument":
  a sub-command (`query`, `env`, `mcp`) now works after global options.
  `--help`'s Usage lists the three sub-commands, and `query`'s no-DSN
  error mentions Spring `application*.yml` alongside the other configs
  it reads.
- The no-connection start card wraps at 80 columns instead of cutting
  its lines off mid-word.
- `--redact` blocked out pgman's own name — the title read `▓▓▓▓▓ 0.1.0`
  — when a database name had `pgman` in it (`pgman_test`). The product
  name is now never redacted; the rest of such a name still is.
- A typo like `selec 1;` no longer reads as a guarded write. It is still
  guarded (pgman never assumes an unrecognised statement is safe), but the
  confirm card now says "`selec` isn't an SQL statement pgman recognises —
  send it to Postgres anyway? (it will likely be a syntax error)" rather
  than that a read-only session will refuse it, and `pgman query` says it
  isn't a statement pgman recognises (typo?) — re-run with `--yes` to send
  it anyway.
- After loading a query from the log picker, `Ctrl-L` / `F8` said "no
  queries found", because the loaded query had replaced the log in the
  buffer. With no log in the buffer it now reopens the last log's picker
  (`reopened the last log · N queries`); a new log replaces it.
- Two pgman windows no longer lose each other's editor draft. A window
  that never changed the draft writes nothing when it quits, so it can't
  overwrite what another window saved meanwhile; and a window with edits
  of its own, finding `draft.sql` changed by another window, saves to a
  sibling `draft-YYYYMMDD-HHMMSS.sql` and says so, instead of clobbering it.
- The tap monitor's status line kept reading "no events yet" after
  events had arrived; it now follows them.
- OpenTelemetry spans were stamped with their end time rather than
  their start, which inflated the Pools view's peak concurrency.
- A burst of tap events no longer piles up in memory behind the
  screen. pgman redrew once per event, so 4 000 large events took over
  30 s to land and held more than 800 MiB on the way; they now land in
  a few frames (under a second, 100–200 MiB at peak). The hop into the
  app is bounded, so events pgman cannot keep up with are dropped and
  counted on the `DROP` badge instead of queueing without limit.
- A tap listener that cannot bind (port in use) is now reported on the
  status line and on stderr — `--tap-listen`, `--tap-udp`, `--tap-otlp`
  and the listener a Java project turns on by itself. The status line
  used to announce the port as listening while the failure went only
  to the log.
- `--tap-otlp` accepts gzip-compressed bodies, which the OpenTelemetry
  Collector's `otlphttp` exporter sends by default; they failed with
  `400 not utf-8`. The inflated body has the same 4 MiB cap as the raw
  one. Any other `Content-Encoding` gets a 415 saying to send gzip or
  none.
- A pgman-tap event claiming an absurd duration is clamped to one hour,
  as OpenTelemetry spans already were; `u64::MAX` showed as
  `18446744073709.55s`, broke the column alignment and topped every
  sort by time.
- The tap List view's app column is cut by display width, so a CJK app
  name no longer pushes its row's SQL out of line. A name too long for
  the column ends in `…`.
- The tap monitor's status line says `1 query · 1 heartbeat`, not
  `1 queries · 1 heartbeats`, as its title already did.
- `--tap-listen`, `--tap-udp` and `--tap-otlp` take a host name
  (`localhost:7432`), which failed as `invalid socket address syntax`.
  A name that does not resolve says to use an IP address instead.
- The tap monitor's empty state and status line say where pgman is
  listening (`listening on TCP 127.0.0.1:7432 · OTLP/HTTP …`). They
  told the operator to start pgman with `--tap-otlp :4318` even with a
  listener — that one included — already bound; the hint now points
  the JVM at the bound OTLP port.
- The tap monitor's title is no longer cut mid-word by its border: the
  room it was fitted to left out the decoration round its name.
- A producer's huge drop counter no longer pushes the tap monitor's key
  hints off its title: counts from 10 000 up read `12.3k`, `4.5M`, …,
  `1.0e18`, and the drop count now comes after the hints, so a narrow
  terminal gives it up first (the status line still shows it).
- The tap monitor's OpenTelemetry hint gives
  `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://localhost:4318/v1/traces`.
  Without the path, which the exporter does not add to that variable,
  the OpenTelemetry Java agent's exports got a 404.
- A write refused by a read-only connection no longer puts the full
  `safety.toml` path in the footer, where at 80 columns it took three
  rows and was cut anyway. The footer names `safety.toml`; the F2
  overlay and `--batch` stderr give the path. And it names
  `safety.toml` only when the profile made the session read-only: a
  standby, `:readonly on`, or a `SET` / `BEGIN READ ONLY` / role default
  under a writable profile each says so instead of sending the operator
  to a file that already says `read_only = false`. A database the
  project's `.pgman/pgman.toml` keeps read-only is described as that
  file's block, by the refusal and by `:readonly off`, instead of as a
  `[databases.<db>]` section of `safety.toml` that the operator's file
  doesn't have.
- A completion popup the editor opened on its own (after `.` or `FROM `)
  that narrows to nothing as you type on now closes without leaving
  `completion: no matches for …` in the footer. A popup Tab opened still
  says why it closed.
- **NULL and the empty string no longer look the same.** The grid
  draws a NULL as a dimmed `NULL` (a new theme colour, `null_cell`) and
  an empty string as nothing; row and cell detail say `NULL` and
  `(empty string)`. The `I` INSERT yank wrote `NULL` for an empty
  string, changing the row it copied; it now writes `''`.
- **The `I` yank writes back the row it copied.** Every value is a
  quoted literal (a text `007` came back as `7`, `TRUE` as `true`),
  column names are quoted when they need it (`createdAt`), and reserved
  words are quoted everywhere pgman builds SQL (a column named `order`).
- `--format csv` and `Y` write a NULL as an empty field and an empty
  string as `""`, as `COPY … CSV` does; `--format tsv` writes a NULL as
  `\N`, as `COPY`'s text format does. `--format json` already wrote
  `null`, and now does for a multi-statement script too.
- Sort puts NULLs last ascending and first descending (Postgres's
  defaults) and an empty string first ascending, not with the NULLs.
  `/null` and `f null` find NULL cells.
- `F` on a NULL foreign key says so instead of opening `WHERE id = NULL`;
  `y` on a NULL field leaves the clipboard alone.
- The result diff counts a NULL → `''` change and shows it; `\fixture`
  leaves NULL cells out (flat XML's NULL) instead of writing `col=""`.
- Logs → SQL: a Postgres log's bound value with a newline in it no
  longer corrupts the reconstruction. The rest of the value, which
  Postgres writes on the next line, went into the statement's SQL and
  the value was cut short; it now stays in the value.
- Logs → SQL: a multi-line statement from a Postgres log no longer
  keeps the tab Postgres adds to each continuation line, which changed
  a string literal that spans lines.
- Logs → SQL: Hibernate logs in logback's default pattern
  (`%logger{36} - %msg`) are read whole. pgman cut the statement at
  its first `:` (`like 'ref:%'`, a `10:00:00` timestamp), or dropped a
  statement with no `:` in it.
- Logs → SQL: under Spring Boot 3.2's default log pattern, which puts
  `[appName]` before the thread, a Hibernate bind paired with whichever
  thread logged a statement last. The thread is now the bracket group
  nearest the logger.
- Logs → SQL: `BEGIN` / `COMMIT` / `ROLLBACK`, `SET` / `SHOW` / `RESET` /
  `DISCARD` and `select 1` pool pings are no longer reported as N+1
  clusters, where they outranked the real one. They stay in the list.
- Logs → SQL: ANSI colour codes in a pasted log (Hibernate's
  `highlight_sql=true`, a coloured console or pager) are ignored. A
  highlighted formatted statement was dropped, and a one-line one kept
  the codes in its SQL.
- Logs → SQL: a Postgres `duration: … execute` record whose SQL
  contains `statement:` (`like 'statement:%'`) was cut there, leaving
  the tail of the statement as the query.
- Logs → SQL: a Hibernate 6 `ARRAY` bind (`[a, b c]`) is substituted as
  a Postgres array literal (`'{"a","b c"}'`) instead of a string the
  server rejects as a malformed array.

## [0.1.0] — 2026-10-22

First public release. pgman is a terminal UI for PostgreSQL in the k9s
mould, built for teams running Java services on AWS. Point it at a
database — or let it find the connection in your Spring
`application*.yml`, IntelliJ data sources, or a committed
`.pgman/pgman.toml` — and you get a query editor with completion and
history, a schema browser, EXPLAIN trees, slow-query and session panels,
and a results grid you can filter, sort, diff and export. What nothing
else does: paste a Hibernate log, a Postgres server log, or a JDBC
statement with its parameters, and pgman reconstructs the runnable SQL,
clusters N+1 loops, and runs it — no application changes, works offline.
Connections are read-only by default, every statement is classified and
guarded before it reaches the server, and anything discovered in a
working tree is treated as untrusted until you confirm it. A live JDBC
tap (OpenTelemetry today) shows what the application is actually
sending. Install with `brew install tombaldwin/tap/pgman`, `cargo install
pgman --locked`, or a tarball from the releases page; `pgman --demo`
runs the whole thing against synthetic data. Pre-1.0: expect rough edges
and breaking changes.

### What's in it

- **Connect** — auto-discovery from Spring `application*.yml` /
  `.properties` (with profile overlays and `${VAR}` / `${VAR:default}`
  from the environment), IntelliJ `.idea/dataSources.xml`, and a
  committed `.pgman/pgman.toml`; a picker that shows host, `sslmode`,
  tunnel and credential provenance before anything connects; a start
  card on connect with databases, sizes and the keys that matter.
- **Logs and pasted code → runnable SQL** — Hibernate logs (with
  bind-parameter trace logging), Postgres/RDS server logs, and pasted
  JDBC statement + parameters; N+1 detection; a pasted log is recognised
  in the editor; `pgman --log PATH` opens straight into the picks.
- **Editor** — Enter runs a statement that ends in `;` (psql's rule),
  Alt-Enter runs regardless; opens five lines tall and grows with its
  content; syntax highlighting, identifier and keyword completion,
  auto-indent, Ctrl-F formatting (built-in, `pg_format` when
  installed; never on run or paste), history, undo/redo, saved queries
  with `:param` prompts, `$EDITOR`, `\watch`, DBUnit fixture apply and
  capture.
- **Safety** — read-only by default, `statement_timeout`, per-database
  guards (block / confirm / allow) on every statement, DML left in an
  open transaction until you commit or roll it back, `EXPLAIN ANALYZE`
  always rolled back, and a
  project's `[safety]` block can only tighten yours. A read-only
  session also refuses the server-control functions Postgres's own
  read-only mode lets through (`pg_promote`, `pg_wal_replay_pause`,
  replication slots, `pg_stat_reset`…).
- **DBA panels** — schema browser, schema-lint wizard, EXPLAIN tree,
  slow queries, sessions and locks, result diff, `\l`, `\x`, `\c`, `\i`,
  the `\d` family, `\timing`.
- **MCP server** — `pgman mcp serve` gives Claude Code (or any MCP
  client) tools: `query`, `sessions_locks` (a pre-flight before a schema
  change), `pending_patches`, `connections`, `environments` and `doctor`,
  with more below. Every session is read-only whatever `safety.toml`
  says, except `apply_patches` (see Applying patches below);
  personal connections can tunnel and read their password from AWS
  Secrets Manager; a `primary` connection serves only the lock check.
- **Environments overview** — `E` lists every database in each
  environment, replicas under their primary, from RDS (via the `aws`
  CLI) or hand-listed connections, without connecting to any of them.
  `Enter` connects to a row: replicas read-only, the password read from
  AWS Secrets Manager at that moment, through the environment's ssh
  tunnel.
- **`pgman env discover`** proposes environments from an AWS account:
  one per RDS primary with its replicas, the secret matching each
  database by name, the bastion in its VPC. It reads names only, never a
  secret's value; `--save` appends to `environments.toml`. In the
  overview, `d` does the same with a preview of what each save appends.
  Over MCP, `environments_discover` and `environments_save` do it for an
  agent (save refused under `--read-only`).
- **ebman's look** — rounded panels with `[ ◆ name ◆ ]` titles
  (a plain ` name ` under Powerline fonts, as in ebman), ebman's two-box
  header on terminals 24 rows or taller (server, database, user,
  session, and where the connection came from, beside the current view
  and the global keys), coloured key hints in the footer, and uppercase
  panel column headers. The drawing vocabulary now lives in
  `tb-tui-common`, shared with ebman.
- **`--batch --discovered`** accepts a checkout that declares one
  database twice (identical URL and credentials) as a single candidate.
- **MCP tunnels stay open between calls** to the same database (5
  minutes idle), so a run of `query` calls pays the ssh handshake once;
  each call still gets a fresh read-only session.
- **Partitioned tables fold** in the schema browser: one `N partitions`
  row under the table instead of a screenful of partitions. A database
  with one schema opens with it expanded.
- **Slow queries says why it's empty**: without `pg_stat_statements`
  the panel stays open with how to install it, instead of flashing
  shut.
- **Esc from the results goes back to the environments overview.**
- **Environments are home.** pgman starts on the overview (with
  environments configured and no `--dsn`), full screen, on the row you
  used last. Replicas sit under each database. Probing environments
  show `SCHEMA`, `LAG`, `SESSIONS` and `SIZE` columns.
- **Connecting to RDS works for ordinary users.** The start card sized
  every database on the server, and RDS's `rdsadmin` refuses that to
  anyone but RDS, so the whole connection failed. Only databases you
  can connect to are sized now, and a failed list never fails the
  connection.
- **A `docker` environment source** — local Postgres containers, from
  `docker ps`, reached on their published port. Environments can name
  `password_env` for databases without a secret. (Earlier docs said
  `~/.pgpass` and `PGPASSWORD` applied; pgman never read them for
  environments.)
- **Probes** — `probe = true` on an environment adds each database's
  patch level, pending patches and replica lag to the overview. It reads
  through replicas (a primary with a replica isn't touched), uses one
  ssh per bastion, and every connection is read-only.
- **More at a glance in the overview.** RDS rows show the minor and
  major upgrades available (`↑16.15 · 18.6`). Without a `version_table`,
  a probe recognises Flyway, Liquibase, Prisma, sqlx, Django, Alembic,
  and Rails / golang-migrate (their shared `schema_migrations`) by their
  own tables (and counts pending Flyway files
  from `migrations_dir`). `SESSIONS` adds blocked sessions and
  transactions open over 5 minutes. A replica's detail line sketches its
  recent lag. `v` shows schema drift: each database's version by
  environment.
- **Recent errors from RDS logs** — `l` on a server in the overview,
  `environment_logs` over MCP: ERROR, FATAL and PANIC lines from the
  newest log files, grouped with a count. `STATEMENT` and `DETAIL` lines
  are left out, since they can carry row data.
- **Applying patches** — `P` in the overview, `apply_patches` over MCP.
  A read-only plan lists the pending patches, each checked against
  `safety.toml`; after you confirm (a protected environment — `prod` in
  its name, or `protected = true` — by typing its name; over MCP through
  elicitation), each runs in its own transaction with a 5 s
  `lock_timeout`, stopping at the first failure; the result says which
  version the database records afterwards. A pending file numbered
  below the recorded version is left out and named, not run out of
  order. A patch that can't run in a transaction (`VACUUM`,
  `CONCURRENTLY`, …) is refused up front; in the TUI, `o` plans it to
  run outside one, statement by statement and with no statement
  timeout, behind a typed confirm. Esc twice cancels a running patch,
  between statements too: the patch rolls back. The plan names the
  risky statements (`TRUNCATE`, a `DELETE` without `WHERE`…) its one
  confirmation answers; the timeouts are set again before every
  statement, so a `DO` block can't lift them for the rest of the patch;
  a project's `[safety]` block applies here as everywhere else.
  Tab in the plan (`mode` over MCP) runs a **dry run** instead — every
  patch in one transaction, rolled back, with the same locks as the
  real run — or applies the patches to a **copy** of the database made
  on the same server, dropped afterwards.
  Never on a replica, and only on a database `safety.toml` lets write.
- **Safer and more honest, from a four-reviewer robustness pass.** A
  unicode-escaped name (`U&"pg_\0072ead_file"`) no longer slips past the
  safety classifier; MCP `query` results are capped at 16 KiB a value
  and 1 MiB in all; a DSN with two different `sslmode`s is refused;
  connects give up after 10 s; `:connect` takes a URL and never echoes
  its password; `:readonly` applies to the live session, so the header
  tells the truth. `--batch` says when a CSV/TSV export stopped at 1000
  rows (exit 3). A caught-up replica of a quiet primary shows no lag.
  Hibernate's `show_sql` console lines reconstruct. Find no longer eats
  `n`. `ctrl-l` works from the start card. `--demo`'s slow queries,
  sessions and wizard work. The help fits small terminals.
- **The rest of that pass.** Plain keys for the F-keys a Mac keyboard
  hides behind `fn`: `!` opens the error detail (`:error`, `:notify`,
  `:tap` spell F2–F4), and the footer names the key that works where
  you are. `ctrl-c` outside the editor and `ctrl-w` over a tab holding
  SQL take a second press; a `WATCH` badge shows while `\watch` runs;
  a cancel reads `cancelled`, not as the server's error. A later
  success takes a stale error off the footer, and the confirm modal
  keeps its `y`/`n` keys. `r` in the schema browser reloads the schema
  (tables other sessions made), and an empty cache on a live connection
  reloads by itself; `\d` on a name nothing matches says so. Esc after
  Tab gives back what you typed, and Enter runs a finished statement
  under a popup nobody picked from. Bracketed IPv6 hosts parse
  (`postgres://u@[::1]:5433/db`); `\i` refuses files over 8 MB and
  anything that isn't a regular file; `--log -` reads a piped log and
  takes keys from the terminal; `--demo` honours `--log`.
  Environments: Aurora readers sit under their writer; docker finds
  postgis and registry-prefixed images, IPv6-only and ranged ports; a
  dirty golang-migrate version shows as failed; a text id in a version
  table shows instead of an error; drift compares Liquibase, Django and
  Alembic versions as names; `migrations_dir` takes Flyway `locations`,
  recursively, Java and Kotlin migrations included. Long lines in the
  patch and log views wrap instead of being cut, the detail line cuts
  an endpoint at a dot, and the pickers sit centred.
- **The backlog pass.** `\df [pattern]` lists functions with psql's
  patterns. In the schema browser, tables list their indexes, `p`
  previews five rows and `d` copies the `CREATE` statement rebuilt from
  the catalog. LINT107 flags a serial or identity column nothing keeps
  unique. Right after `JOIN … ON`, completion offers the foreign-key
  predicate. `pgman query 'SQL' [--json]` runs one statement without
  the TUI. `sslmode=verify-ca` checks the certificate chain but not the
  hostname, as libpq does. Safety: a `WHERE` inside a subquery no
  longer qualifies an `UPDATE` / `DELETE`, and a non-ASCII dollar-quote
  tag is read as Postgres reads it. The `T` / `L` panels show numeric,
  date, interval and array values instead of `?`. A long error wraps
  onto up to three footer rows; the connection-failure card wraps its
  paths whole so they can be copied; `--demo` opens with the tap quiet
  and its burst arrives two seconds in. The update check caps a
  response while reading it; `--upgrade` refuses rather than running
  from the temp directory; `--batch` repairs pgman's directories to
  owner-only as the TUI does.
- **`schema_lint` over MCP** — the schema wizard's findings (missing
  primary keys, unindexed foreign keys, unused indexes, bloat, …) for an
  agent to work through.
- **A `:` command bar** — `:about`, `:help`, `:update`, `:readonly`,
  `:connect`, and every `\` command; `?` opens help from any mode.
- **Layout you control** — `z` zooms the results and `Z` the editor to
  the whole screen and back; `+` `-` `0` size the editor; a tab bar
  that is always there, labelled by each tab's query, switched with
  `]` `[` and the digits, `ctrl-t` / `ctrl-w` to open and close, and
  `ctrl-]` from inside the editor. Every one of these is a key a Mac
  terminal actually delivers; the Alt chords remain as secondaries.
- **Row detail steps rows** — `J` / `K` (or PageDown / PageUp) move to
  the next and previous row without leaving the view, keeping the
  field you had highlighted, and the grid selection follows.
- **JDBC tap** — live app-side query observability via OpenTelemetry
  (TCP and UDP listeners for the forthcoming `pgman-tap` JAR), hotspots,
  per-caller and per-pool rollups, transaction view, baseline diff,
  live N+1 detection, Markdown / HTML report export.
- **Batch mode** — `--batch --sql … --format csv|tsv|json|expanded`
  for scripts and CI, with typed JSON and the same guards; `--yes`
  confirms guarded writes and never lifts read-only.
- **Install and upgrade** — Homebrew, crates.io, or a release tarball
  for four targets with build provenance; `pgman --upgrade` knows
  which channel installed it; an update check at most every six hours
  that never delays the first frame (`--no-update-check`).
- **`--demo`** — the whole tool against synthetic data, no database,
  answering queries through the real guards. Its database is `ledger`,
  the billing database of a made-up subscriptions product: 26 relations
  across `public` and `reporting` (partitioned usage events, views,
  identity keys, partial indexes), answers that honour a WHERE's
  `column = value` terms, EXPLAIN plans from its row estimates, and
  four environments — production with a replica, staging, a stopped
  dev, a local container — whose versions, drift, pending patches,
  log errors, sessions and slow queries tell one story.
- **Redaction mode** — `--redact`, `:redact` or `redact_default = true`
  blocks out hosts, IPs, account IDs, ARNs and every server, environment
  and secret name pgman knows, on every screen, for screenshots of a
  real database; schema and data stay visible.

### Hardening before release

A five-reviewer release panel, run four times, found and fixed — each
with a test demonstrated failing first — a hostile-checkout trust model
(a repo's config could once auto-connect with your `PGPASSWORD`, resolve
an environment variable into a hostname, or relax your safety profile);
a statement lexer that now understands quoted identifiers, `$` and
non-ASCII in identifiers, `E''` strings and nested comments, refuses a
script it cannot verify, and executes exactly the statements it
checked; passwords with `/ @ ? #` parsed and redacted correctly; an
unknown `sslmode` refused rather than silently downgraded; a read-only
session that cannot be flipped from inside; bounded and throttled JDBC
tap ingest; owner-only files from the first byte; escapes stripped from
reports and batch output; the placeholder resolver and the DSN parser
agreeing on where a URL's host starts, or refusing the URL; the
read-only floor closed against `set_config`, a quoted GUC name, `DO`
and `CALL`, and a `COMMIT` mid-script (batch runs inside one explicit
transaction); the picker naming where every credential comes from;
decision modals that no global chord can escape; `--demo` never
touching your saved queries; and a few dozen first-minute polish
defects found by rendering every screen at five terminal sizes and
by driving the release binary against a live server.

### Dependencies

ratatui 0.30, crossterm 0.29, reqwest 0.12 (rustls only), `tb-tui-common`
0.2 shared with ebman. `toml` 1.x is used but saved-query files are
written with TOML 1.0-compatible escapes, pinned by test.

[Unreleased]: https://github.com/tombaldwin/pgman/compare/v0.5.2...HEAD
[0.5.2]: https://github.com/tombaldwin/pgman/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/tombaldwin/pgman/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/tombaldwin/pgman/compare/v0.4.4...v0.5.0
[0.4.4]: https://github.com/tombaldwin/pgman/compare/v0.4.3...v0.4.4
[0.4.3]: https://github.com/tombaldwin/pgman/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/tombaldwin/pgman/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/tombaldwin/pgman/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/tombaldwin/pgman/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/tombaldwin/pgman/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/tombaldwin/pgman/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/tombaldwin/pgman/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/tombaldwin/pgman/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/tombaldwin/pgman/releases/tag/v0.1.0
