//! Query reconstruction — turning logs and pasted code into runnable SQL.
//!
//! Three input sources, one output type (`reconstruct::ReconstructedQuery`):
//!
//! - [`hibernate`] — application-side Hibernate logs (`?` placeholders + binds).
//! - [`pglog`] — Postgres / RDS server logs (`$N` placeholders + `parameters:`).
//! - [`jdbc`] — pasted JDBC: SQL plus a typed parameter list.
//!
//! [`subst`] does the type-aware placeholder substitution shared by all three.
//! [`nplus1`] clusters reconstructed queries to surface N+1 selects.

pub mod backslash;
pub mod clause;
pub mod complete;
pub mod explain;
pub mod from_parse;
pub mod hibernate;
pub mod highlight;
pub mod jdbc;
pub mod json_cell;
pub mod lint;
pub mod nplus1;
pub mod params;
pub mod pglog;
pub mod reconstruct;
pub mod row_diff;
pub mod schema;
pub mod select_list;
pub mod sessions;
pub mod slow_queries;
pub mod subst;
pub mod vocabulary;
