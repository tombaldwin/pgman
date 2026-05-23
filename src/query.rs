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

pub mod clause;
pub mod complete;
pub mod from_parse;
pub mod hibernate;
pub mod jdbc;
pub mod nplus1;
pub mod pglog;
pub mod reconstruct;
pub mod schema;
pub mod subst;
