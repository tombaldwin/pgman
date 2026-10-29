# pgman and personal data

For a client's data protection officer, and for the developers whose
work the journal records: what pgman does with a copy of a database that
holds personal data, what it records, and what it does not claim.

pgman is a tool developers run on their own machines to work with
PostgreSQL. One of the things it does is take a copy of a database
(`pgman dump`) and restore it locally (`pgman restore`), usually to test
against the real shape of the data. This note covers those copies.

## The layers

pgman is one measure among several, as Article 32 of the UK and EU GDPR
expects:

- **The access boundary is the client's AWS account.** IAM decides who
  can read a database's credentials and reach it through the bastion;
  CloudTrail records each read. pgman holds no credentials of its own: it
  reads them, as the developer, when it connects, and never writes them
  to disk or shows them.
- **pgman minimises and records.** A copy of a protected environment is
  masked by default, kept privately, tracked until it is deleted, and
  each step is journaled, locally and (where configured) in the client's
  CloudWatch.
- **The organisation sets the rule.** A tool's default is a technical
  measure; it needs an organisational one beside it. For example:
  *copies of production are taken only with pgman, masked unless a named
  person approves a raw copy, for a ticket, and deleted when the ticket
  closes.* pgman makes that rule easy to follow and to show was followed;
  it does not enforce it on someone determined not to.

## Which environments are protected

An environment is protected when its configuration says `protected =
true`, or, unless it says otherwise, when its name contains "prod". A
database on a server a protected environment reaches is protected
however it is named.

## Masked copies

A copy of a protected environment is masked unless the developer's own
configuration allows a raw one and they ask for it, at a terminal,
typing the environment's name.

**What is replaced.** Each column is judged from the database's catalog
(its name and type; no rows are read to decide):

- email addresses, names, phone numbers, IP addresses: replaced by
  realistic fakes (`@example.test` addresses, phone numbers in Ofcom's
  drama ranges, documentation IP ranges), the same value always becoming
  the same fake within one copy so that the data still joins up;
- postcodes: reduced to their district; dates of birth: to their year;
- passwords: replaced by one known test password;
- secrets and tokens, special-category data (health, ethnicity,
  religion, sexuality, gender, union membership, political opinion,
  biometric and genetic data, criminal offences), government and
  financial identifiers, pay, addresses, locations, device identifiers,
  and free text (notes, comments, descriptions, JSON documents, any type
  pgman doesn't know): removed (NULL, or an empty value where the column
  can't be empty);
- the rows of session and token tables: left out; large objects: left
  out; comments on tables and columns: left out.

Everything else is kept as it is: amounts, dates (other than birth),
statuses, codes, identifiers internal to the application. The names
pgman recognises are English, and the identifiers British (NI and NHS
numbers, sort codes); a schema in another language, or with other
countries' identifiers, needs them named in its rules file.

A team can change this for its own schema in a rules file committed to
its repository. Keeping a column the defaults would mask needs a written
reason, and every dump states which columns it masked and which it
removed.

**How.** Values are replaced as they are read, on the developer's
machine, by a keyed hash. The key is made for each copy and thrown away
afterwards: nobody, including the developer who took the copy, can
match a masked value back to a real one by trying candidates, and two
copies can't be linked. Columns that are removed are never read from the
server at all.

**What a masked copy still is.** Pseudonymised data, which is still
personal data in law: frequencies survive (the commonest fake surname
stands for the commonest real one); the kept columns, in combination,
can single someone out in a small population; the schema itself (view
definitions, column defaults, function bodies) is copied as it is and
may hold literal values. So a masked copy gets the same care as any
copy — it is just much less of a liability.

## Raw copies

A raw copy (every row as it is) of a protected environment needs:

- the developer's own configuration to allow it for that environment,
  which a project's repository can forbid but never allow;
- the environment's name typed at a terminal for each one (or, for a
  job, an explicit setting for that environment, each run journaled as
  unattended);
- a reason, recorded;
- a folder outside iCloud Drive, Dropbox, OneDrive and Google Drive.

A coding agent working through pgman's MCP server can never take one,
nor restore one (nor any dump this machine's pgman didn't take), and
every MCP tool refuses to read a database restored from one. A copy of
the file is still the same copy (pgman knows it by its contents), and
a copy of a database restored from one is treated as a copy of the
original.

## The life of a copy

- **Private.** Dump files are readable by the developer alone, written
  to pgman's own directory by default, never into a synced folder
  (iCloud Drive, Dropbox, OneDrive, Google Drive, Nextcloud) for a raw
  copy, and excluded from Time Machine.
  pgman warns when the disk isn't encrypted (FileVault off). Encrypting
  dump files is planned, not built: until it is, a copy on disk is
  protected by the machine's own disk encryption.
- **Expiring.** Each copy has an expiry (14 days for a protected
  environment, unless configured); `pgman copies` flags those past it.
- **Deleted in one step.** `pgman copies rm` deletes the file and drops
  every database restored from it (only ones pgman itself restored, on
  the developer's machine).

## The journal

Every dump, restore and deletion is recorded in a journal on the
developer's machine (`~/.local/share/pgman/journal.jsonl`, readable by
them alone). `pgman copies report` prints it as an account — what was
taken, when, why, where it was restored, when it was deleted — for a
ticket, a review, or the end of an engagement.

**What it records:** the environment and database, whether the copy was
raw, masked or schema only, the reason (prompted as "a ticket id or a
few words, no names") and the ticket, the file's name and size, when it
expires, where it was restored, and when it was deleted; a random id for
the machine (not its name). **Not:** any value from the database,
connection details, credentials or secrets.

**In the client's account.** Where an environment names a CloudWatch Logs
group, each event is also sent there as it happens, with the IAM
identity of the developer who sent it, the file's name without its path,
and no server addresses. The developers' IAM permission is to add
events only, not to delete them; the group's retention (one to two years
is usual) and who may read it are the client's settings. Developers
should know their name (as their IAM identity) goes there.

The journal is a record kept by a tool used in good faith. The record no
developer can edit is the client's own: CloudTrail (credentials read)
and CloudWatch once sent.

## What pgman does not claim

- **It is not an access control.** Anyone who can read the credentials
  and reach the database can copy it without pgman. pgman makes the
  careful way the easy way, and records it.
- **A coding agent's queries.** pgman's MCP server lets an agent run
  read-only queries against a configured database, production included,
  if the developer has configured it so; masking applies to copies, not
  to those queries.
- **Anonymisation.** A masked copy is pseudonymised, not anonymous.
- **Encryption of dump files** is not built yet.

The design behind all of this, and what is planned, is in
[`docs/design/masked-dumps.md`](design/masked-dumps.md).
