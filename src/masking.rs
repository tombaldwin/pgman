//! Masked dumps: a copy of a database with what looks personal replaced
//! as it is read, so the copy keeps the data's shape without the people
//! in it. See `docs/design/masked-dumps.md`.
//!
//! Pure here — the catalog model, the classifier and rules, the
//! transformers, the COPY text format, the archive's manifest; the
//! database side is [`crate::query::masking`].

pub mod catalog;
pub mod classify;
pub mod copytext;
pub mod rules;
pub mod transform;
