# Changelog

All notable changes to pgman are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims
to follow [Semantic Versioning](https://semver.org/) once it reaches 1.0.

## [Unreleased]

pgman is pre-1.0; until the first tagged release, `main` is the moving target.
Highlights so far (see `BACKLOG.md` for the full record):

- **Connect & browse** — auto-discovery of datasources from Spring
  `application*.yml`/`.properties` (incl. profile overlays), IntelliJ
  `.idea/dataSources.xml`, and `.pgman/pgman.toml`; schema browser; results
  grid with filter / find / sort / bookmarks; result diff.
- **Query reconstruction** — Hibernate logs, Postgres/RDS server logs, and
  pasted JDBC turned into runnable SQL; N+1 detection.
- **Safety** — read-only-by-default connections, `statement_timeout`,
  per-database guard rails classifying every statement.
- **Editor** — syntax highlighting, `pg_format`, history, saved queries (with
  `:param` prompts, rename, search), DBUnit fixture apply (`Ctrl-D`) and
  capture (`\fixture`) with per-database clean strategy.
- **Performance / DBA** — slow-query and active-session panels, EXPLAIN tree,
  schema-lint wizard.
- **JDBC tap** — live app-side query observability (TCP / UDP / OTLP ingest)
  with hotspots, per-caller and per-pool rollups, transaction view, baseline
  diff, and live N+1 detection; Markdown / HTML report export.

[Unreleased]: https://github.com/tombaldwin/pgman/commits/main
