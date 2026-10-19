//! Credential resolution.
//!
//! Spring config (`creds::spring`) is a *source of references*: it yields a
//! datasource URL and possibly unresolved `${...}` placeholders.
//! `creds::aws_secrets` fetches a connection's password from AWS Secrets
//! Manager for the MCP server's personal connections.
//!
//! Hard rule: resolved credentials must never reach `tracing` or the UI. Show
//! *provenance* instead (see CLAUDE.md).

pub mod aws_secrets;
pub mod intellij;
pub mod spring;
