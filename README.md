# pgman

A k9s-style Postgres TUI aimed at Java / AWS shops. Sibling project to
[`ebman`](https://github.com/tombaldwin/ebman).

## The wedge

Point it at a Postgres database, then turn logs and pasted code into runnable SQL:

- **Hibernate logs → runnable SQL.** Reconstruct executable statements from
  `org.hibernate.SQL` lines plus the separately-logged bind parameters.
- **Postgres / RDS server logs → runnable SQL.** Reconstruct from `log_statement`
  output plus `DETAIL: parameters: $1 = …` lines (the more reliable source — it
  needs no application redeploy).
- **Pasted JDBC → runnable SQL.** Substitute `?` placeholders with bound values.
- **N+1 detection.** Cluster reconstructed queries by shape to surface
  loop-driven selects.

Run it inside a Spring project and it picks up `spring.datasource.*` to connect.

## Safety

pgman connects to production databases. It opens read-only by default, enforces
a `statement_timeout`, classifies every statement, and applies **per-database
guard rails** (`safety.rs`) — e.g. block `DROP`, confirm `TRUNCATE` /
unqualified `DELETE`, and wrap DML in a transaction you can roll back.

## Status

Pre-v1 scaffold. See `BACKLOG.md` for the milestone plan.
