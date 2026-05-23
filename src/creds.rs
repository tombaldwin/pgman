//! Credential resolution.
//!
//! Spring config (`creds::spring`) is a *source of references*: it yields a
//! datasource URL and possibly unresolved `${...}` placeholders. The resolvers
//! that turn those placeholders into values — AWS SSM Parameter Store, AWS
//! Secrets Manager, 1Password — are v2 (see BACKLOG.md).
//!
//! Hard rule: resolved credentials must never reach `tracing` or the UI. Show
//! *provenance* instead (see CLAUDE.md).

pub mod intellij;
pub mod spring;
