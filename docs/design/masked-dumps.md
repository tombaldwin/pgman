# Design: masked dumps

Status: **proposal, not built.** Written 2026-10-28 after v0.3.1, to agree
the shape before any code. Open decisions are marked **Decide**.

## Why

`pgman dump` (v0.3.0) makes a copy of any environment two commands away,
production included. A copy of production is useful, for testing against
the real shape of the data: its volumes, its distributions, its odd rows.
But a production dump is personal data on a developer's laptop:

- under UK and EU GDPR that is a new place the data is processed, which
  needs a reason, minimisation and security, and a client's data
  processing agreement may rule it out;
- dumps linger (`~/dumps`), get backed up and synced (Time Machine,
  iCloud, Dropbox), and a lost laptop becomes a reportable breach;
- a fix-up after restoring (resetting passwords, swapping domains) leaves
  the names, emails and addresses where they were.

pgman doesn't open new access (anyone who can read the secret and reach
the bastion can run pg_dump), but it removes the friction that was doing
some of the controlling. This design puts a better control in its place:
**a copy of production that has the data's shape and none of its people.**

Pseudonymised data is still personal data under GDPR; only data that
can't be tied back to a person is out of scope. The aim is the second
where possible, and the first, with fewer copies of fewer fields, where
not. Free text and JSON are where masking most often fails, so they are
treated as personal until a rule says otherwise.

## Goals

1. A per-environment **policy** for what a dump may contain, with
   protected environments defaulting to masked.
2. **Masking rules** in a committed, reviewable file, applied while the
   dump is taken: unmasked rows are never written to the laptop's disk.
3. **Fail closed.** A masked dump is refused while a column that looks
   personal has no rule, including one added after the rules were
   written.
4. A **validate** command that shows what masking does without printing
   production values.
5. A masked dump **restores** with `pgman restore`, joins intact,
   constraints satisfied.
6. Later: **subsetting**, a consistent slice instead of everything.

Not goals: masking a live database in place; dynamic masking (rules
applied per query by role); synthetic data with no source.

## 1. Policy

In `environments.toml`, per environment:

```toml
[[environments]]
name = "production"
dump = "masked"      # allow | masked | schema | never
```

| Value | `pgman dump` may produce |
|---|---|
| `allow` | anything, masked or not |
| `masked` | a masked dump, or schema only |
| `schema` | schema only (`--schema-only`) |
| `never` | nothing |

- Default: `masked` for a protected environment, `allow` otherwise.
  **Decide:** `masked` or `schema` as the protected default.
- An unmasked dump of a protected environment, where `allow` permits it,
  also takes the environment's name typed at a terminal, which `--yes`
  doesn't answer, plus a warning that it copies personal data to this
  machine.
- MCP's `dump` never takes more than `masked` from a protected
  environment, whatever the policy says.
- The policy lives in the operator's own `environments.toml`; a project's
  `.pgman/pgman.toml` may tighten it (as it tightens `safety.toml`),
  never loosen it.

This part stands alone and is worth shipping first.

## 2. Rules

A committed file beside the code that owns the schema,
`.pgman/masking.toml`, reviewed like a migration:

```toml
# What a masked dump does to each column. Every column a classifier
# thinks may be personal needs a rule here, or `keep`.

[defaults]
key = "secret:masking/ledger"     # where the hash key comes from (see 4)

[tables."public.customers"]
email      = "email"              # u_<hash>@example.test, unique
full_name  = "name"               # a fake name, the same one every time
phone      = "phone"
birth_date = { rule = "date.year" }               # keep the year only
postcode   = { rule = "postcode.district" }       # SW1A 1AA -> SW1A
notes      = "null"
tier       = "keep"                               # not personal

[tables."public.customers".when]                  # conditions
keep_if = "email LIKE '%@example.test'"           # test accounts as they are

[tables."public.users"]
password_hash = { rule = "fixed", value = "$2b$12$…" }   # one known password
last_login_ip = "ip"

[tables."public.events"]
payload = { rule = "json", paths = { "$.customer.email" = "email", "$.note" = "null" } }

[tables."public.audit_log"]
"*" = "drop-data"                 # schema kept, no rows
```

- Rules name a **transformer** (section 3), with options.
- A rule on a primary key column **follows foreign keys**: every column
  that references it gets the same transform, so joins still join. Rules
  on a partitioned table apply to its partitions.
- `when` conditions keep or change the rule per row.
- A rule may read another column's **masked** value
  (`{ rule = "date.after", column = "birth_date", min = "18 years" }`),
  so check constraints still pass.
- `keep` is a decision, recorded; an unlisted column is not.

## 3. Transformers

A small, deterministic set, enough for the common cases:

| Rule | Does |
|---|---|
| `keep` | nothing (an explicit decision) |
| `null` / `fixed` | NULL, or one value for every row (`fixed` with a bcrypt hash resets every password) |
| `hash` | a keyed hash, as hex, of the same length class |
| `email`, `username` | `u_<hash>@example.test`: unique wherever the input was |
| `name`, `first_name`, `last_name`, `company`, `address`, `city`, `phone` | a fake from a built-in word list, chosen by keyed hash: the same input always gets the same fake |
| `ip` | an address in a documentation range (192.0.2.0/24, 2001:db8::/32) |
| `date.shift` | moved by a keyed ±N days, so intervals between rows keep their scale |
| `date.year`, `date.month` | generalised |
| `number.round`, `number.noise` | rounded, or ±N% |
| `postcode.district`, `regex` | partial keeps, pattern replace |
| `json` | rules applied at JSON paths; everything else in the value nulled unless kept |
| `drop-data` | the table's schema is dumped, its rows aren't |
| `sql` | a SQL expression of the row, for what nothing else covers |

Fakes are built from bundled word lists (no network, no per-run
randomness), so a dump is reproducible from the same key.

## 4. Engines and keys

- **Deterministic by default.** A rule maps its input through a keyed
  hash (HMAC-SHA-256) before choosing a fake. The same key gives the same
  output across tables (foreign keys join) and across dumps (yesterday's
  copy and today's agree). A different key per copy breaks linkage
  between copies; **Decide** whether that is an option or the default.
- **The key comes from where the password does**: a Secrets Manager
  secret or an environment variable, read at dump time, never written to
  the dump, the rules file or the log. Without the key, a pseudonym can't
  be reversed by guessing inputs, which matters for low-entropy values
  like emails.
- **Uniqueness.** Rules for columns with a unique index produce values
  from the full hash, and validate checks they stay unique. A collision
  fails the dump rather than the restore.

## 5. The data path

Schema and data travel separately:

1. `pg_dump --section=pre-data` and `--section=post-data`, unmasked:
   structure only, no rows.
2. Each table's rows through `COPY (SELECT <masked columns> FROM t) TO
   STDOUT`, transformed, compressed, written to the dump directory.
3. A manifest: tables, row counts, the rules file's hash, the classifier
   results, the pgman version. No key, no values.

`pgman restore` knows the layout: pre-data with pg_restore, rows with
`COPY … FROM STDIN`, post-data (indexes, constraints) last, so foreign
keys are checked on the masked rows.

**Decide: where values are masked.**

| | On the server, in the `SELECT` | On the operator's machine, in pgman |
|---|---|---|
| Raw values over the wire | never | yes, through the tunnel, in memory only |
| The hash key | in the query text: visible in `pg_stat_activity` and in RDS logs that capture statements | never leaves pgman |
| What's possible on a read-only replica | built-in functions only (`sha256()`, no pgcrypto HMAC) | anything |

Recommendation: on the machine, for keyed rules, so the key never leaves
it, with `null`, `fixed`, `drop-data` and generalisations pushed into the
`SELECT`, so the server never sends what masking would throw away. Raw
values for keyed rules exist only in pgman's memory, as with pg_dump
today.

## 6. Fail closed: the classifier

Before any data moves, pgman reads the catalog and classifies every
column:

- **personal by name**: email, mail, phone, mobile, tel, name (first,
  last, full, display), address, street, city, postcode, zip, dob,
  birth, ssn, nino, passport, iban, account_number, card, ip, lat/lng,
  token, password, secret;
- **personal by type**: `inet`, `cidr`, `macaddr`; free text (`text`,
  `varchar` over 64); `json`, `jsonb`, `xml`, `bytea`;
- **not personal**: keys, enums, booleans, numbers that aren't named like
  the above, timestamps of the system's own (`created_at`, `updated_at`).

A masked dump is **refused** while a personal-looking column has no rule
and no `keep`, naming each column and suggesting a rule. A new column
added since the rules were written is caught by the same check, and the
manifest records what was classified, so validate can show what's new.
**Decide:** whether a name list bundled with pgman, extendable in the
rules file, is enough, or whether to sample values (match email and
phone patterns in a column called `contact`) at the cost of reading rows.

## 7. Validate

`pgman dump <target> --validate` checks the rules against the live
schema and a sample, and writes nothing:

- **Summary by default**: per column, how many sampled values changed,
  stayed the same, or stayed NULL. No values are printed, so it is safe
  in a terminal, a CI log or an MCP result.
- **Warnings**:
  - a masked column with more than 90% of its values unchanged (a broken
    rule);
  - a rule that would break a unique index, a check constraint or NOT
    NULL;
  - a column added or retyped since the last dump;
  - an unclassified personal-looking column.
- `--preview N`: original and masked side by side for N rows, opt-in,
  and refused over MCP.
- `--strict`: a non-zero exit on any warning, for CI.

## 8. Subsetting (later)

`--sample` takes a slice rather than everything: a SQL condition on one
or more tables (`customers: created_at > now() - interval '90 days'`),
then every row they reference and that references them, through the
foreign-key graph, recursively for cycles. A nullable foreign key brings
no row the slice doesn't otherwise need.

Many applications keep relationships the database doesn't know about (an
id in a JSON column, `owner_type` plus `owner_id`), so the rules file can
declare **virtual references** and **polymorphic references** for the
graph to follow. Smaller dumps are faster, and expose fewer people.

## 9. MCP

`dump` over MCP takes `masked` (and `schema`) only from a protected
environment, after the operator's confirmation, which names the rules
file and what the classifier found. Validate's summary is available to an
agent; its preview is not.

## 10. Testing

- The classifier, the rules parser and each transformer: unit tests, pure.
- **A leak test** for every path: a fixture database seeded with canary
  values (`canary-7f3a@leak.test`, a distinctive name, a phone number),
  dumped masked, and every byte of the result (data files, manifest,
  pgman's log, stdout) searched for each canary. Any hit fails. Breaking
  a transformer must make it fail.
- Restore round trip: the masked dump restores, foreign keys and unique
  indexes hold, row counts match the manifest.
- Determinism: the same key gives the same bytes twice; a different key,
  different bytes.

## Phases

| Phase | What | Size |
|---|---|---|
| 1 | Policy (`dump =`), typed confirmation for unmasked protected dumps, MCP cap | ½ day |
| 2 | Classifier, rules file, core transformers, the masked archive and its restore, the leak test | 3 days |
| 3 | Validate: summary, warnings, `--preview`, `--strict` | 1 day |
| 4 | Conditions, rules that read other columns, JSON paths | 1 day |
| 5 | Subsetting with virtual and polymorphic references | 2–3 days |

Each phase ships on its own; 1 can go out before 2 is designed in detail.

## Decide

1. The protected default: `masked` or `schema`.
2. Where values are masked (section 5); the recommendation is the hybrid.
3. One key for all copies (linkable across copies) or one per copy.
4. Classification by name and type only, or by sampling values too.
5. Whether `allow` for a protected environment should exist at all, or a
   raw production dump should only ever be taken outside pgman.

## Prior art

- **Greenmask**: pg_dump-compatible masking with a deterministic hash
  engine, rules that follow foreign keys and partitions, dynamic
  parameters, conditions, subsetting with virtual and polymorphic
  references, and a validate command with a safe summary mode. Much of
  sections 2, 4, 7 and 8 follows it. pgman adds fail-closed
  classification of every column, rather than flagging only what changed
  since the last dump, and unique pseudonyms where its hash engine can
  collide.
- **PostgreSQL Anonymizer**: declarative masking rules as security labels
  in the database, static and dynamic masking. It needs the extension
  installed on the server, which managed databases often don't allow.
