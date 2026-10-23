//! Query reconstruction — turning logs and pasted code into runnable SQL.
//!
//! Four input sources, one output type (`reconstruct::ReconstructedQuery`):
//!
//! - [`hibernate`] — application-side Hibernate logs (`?` placeholders + binds).
//! - [`pglog`] — Postgres / RDS server logs (`$N` placeholders + `parameters:`),
//!   stderr format or `csvlog` / `jsonlog` ([`pglog_structured`]).
//! - [`proxylog`] — p6spy and datasource-proxy JDBC proxy logs.
//! - [`jdbc`] — pasted JDBC: SQL plus a typed parameter list.
//!
//! [`subst`] does the type-aware placeholder substitution they share.
//! [`nplus1`] clusters reconstructed queries to surface N+1 selects.

pub mod apply;
pub mod backslash;
pub mod clause;
pub mod complete;
pub mod ddl;
pub mod explain;
pub mod format;
pub mod from_parse;
pub mod hibernate;
pub mod highlight;
pub mod jdbc;
pub mod json_cell;
pub mod lint;
pub mod logdetect;
pub mod migrations;
pub mod nplus1;
pub mod paging;
pub mod params;
pub mod patches;
pub mod pglog;
pub mod pglog_structured;
pub mod preflight;
pub mod probe;
pub mod proxylog;
pub mod reconstruct;
pub mod row_diff;
pub mod schema;
pub mod select_list;
pub mod sessions;
pub mod slow_queries;
pub mod subst;
pub mod vocabulary;
