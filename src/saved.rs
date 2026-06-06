//! Saved queries — named SQL bodies persisted across pgman
//! runs. Disk format: TOML file at
//! `util::data_dir()/saved.toml`. Pure data + load/save helpers
//! live here; the UI / key handlers live in `app.rs` / `ui.rs`.
//!
//! Out of scope for v1: `:param`-style substitution prompts on
//! load, deletion from the panel, tagging. Keep adding to this
//! module as the feature grows.
//!
//! Format example:
//!
//! ```toml
//! [[entries]]
//! name = "active-users-last-week"
//! body = "SELECT id, email FROM users WHERE last_seen > now() - interval '7 days';"
//!
//! [[entries]]
//! name = "today's-revenue"
//! body = "SELECT sum(amount) FROM orders WHERE created_at::date = current_date;"
//! ```

use serde::{Deserialize, Serialize};
use std::path::Path;

/// One named SQL snippet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedQuery {
    pub name: String,
    pub body: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedQueries {
    #[serde(default)]
    pub entries: Vec<SavedQuery>,
}

impl SavedQueries {
    /// Add (or REPLACE on name conflict) `q`. Returns true when
    /// an existing entry was replaced, false when this is a new
    /// addition. Names are matched exactly (case-sensitive).
    pub fn upsert(&mut self, q: SavedQuery) -> bool {
        if let Some(slot) = self.entries.iter_mut().find(|e| e.name == q.name) {
            *slot = q;
            true
        } else {
            self.entries.push(q);
            false
        }
    }

    /// Remove the entry named `name`. Returns the removed entry
    /// when there was one; `None` otherwise.
    pub fn remove(&mut self, name: &str) -> Option<SavedQuery> {
        let pos = self.entries.iter().position(|e| e.name == name)?;
        Some(self.entries.remove(pos))
    }

    /// Look up by exact name.
    pub fn get(&self, name: &str) -> Option<&SavedQuery> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Rename the entry `from` → `to`, **preserving its list
    /// position** (unlike remove + upsert, which would move it to
    /// the end). Outcomes:
    /// - `Ok(true)` — renamed.
    /// - `Ok(false)` — no entry named `from` (nothing changed).
    /// - `Err(RenameError::Exists)` — a *different* entry is
    ///   already named `to`; refused so a rename can't silently
    ///   clobber another saved query.
    ///
    /// `from == to` is a no-op success (`Ok(true)`).
    pub fn rename(&mut self, from: &str, to: &str) -> Result<bool, RenameError> {
        if from == to {
            return Ok(self.get(from).is_some());
        }
        if self.entries.iter().any(|e| e.name == to) {
            return Err(RenameError::Exists);
        }
        match self.entries.iter_mut().find(|e| e.name == from) {
            Some(e) => {
                e.name = to.to_string();
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

/// Why a [`SavedQueries::rename`] was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameError {
    /// The target name is already taken by another entry.
    Exists,
}

/// Best-effort load. A missing / unreadable / malformed file
/// returns an empty `SavedQueries` — operators never see a
/// startup error from this, just an empty list.
pub fn load_from(path: &Path) -> SavedQueries {
    let Ok(text) = std::fs::read_to_string(path) else {
        return SavedQueries::default();
    };
    toml::from_str(&text).unwrap_or_else(|e| {
        tracing::warn!("saved queries load failed: {e}; starting empty");
        SavedQueries::default()
    })
}

/// Path-parameterised persist. Atomic via the shared
/// `tui_common::util::write_atomic` helper.
pub fn save_to(path: &Path, q: &SavedQueries) -> std::io::Result<()> {
    let text = toml::to_string_pretty(q)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    tui_common::util::write_atomic(path, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_adds_then_replaces_by_name() {
        let mut q = SavedQueries::default();
        assert!(!q.upsert(SavedQuery {
            name: "a".into(),
            body: "select 1".into(),
        }));
        assert!(q.upsert(SavedQuery {
            name: "a".into(),
            body: "select 2".into(),
        }));
        assert_eq!(q.entries.len(), 1);
        assert_eq!(q.entries[0].body, "select 2");
    }

    #[test]
    fn remove_returns_old_entry_or_none() {
        let mut q = SavedQueries::default();
        q.upsert(SavedQuery {
            name: "a".into(),
            body: "select 1".into(),
        });
        let removed = q.remove("a").expect("found");
        assert_eq!(removed.body, "select 1");
        assert!(q.entries.is_empty());
        assert!(q.remove("a").is_none());
    }

    #[test]
    fn rename_preserves_position_and_body() {
        let mut q = SavedQueries::default();
        q.upsert(SavedQuery {
            name: "a".into(),
            body: "one".into(),
        });
        q.upsert(SavedQuery {
            name: "b".into(),
            body: "two".into(),
        });
        q.upsert(SavedQuery {
            name: "c".into(),
            body: "three".into(),
        });
        assert_eq!(q.rename("b", "bee"), Ok(true));
        // Position 1 preserved (not moved to the end).
        assert_eq!(q.entries[1].name, "bee");
        assert_eq!(q.entries[1].body, "two");
        assert_eq!(
            q.entries
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "bee", "c"]
        );
    }

    #[test]
    fn rename_missing_source_is_ok_false() {
        let mut q = SavedQueries::default();
        q.upsert(SavedQuery {
            name: "a".into(),
            body: "one".into(),
        });
        assert_eq!(q.rename("nope", "x"), Ok(false));
        assert_eq!(q.entries.len(), 1);
        assert_eq!(q.entries[0].name, "a");
    }

    #[test]
    fn rename_to_existing_name_is_refused() {
        let mut q = SavedQueries::default();
        q.upsert(SavedQuery {
            name: "a".into(),
            body: "one".into(),
        });
        q.upsert(SavedQuery {
            name: "b".into(),
            body: "two".into(),
        });
        assert_eq!(q.rename("a", "b"), Err(RenameError::Exists));
        // Both entries untouched.
        assert_eq!(q.entries[0].name, "a");
        assert_eq!(q.entries[1].name, "b");
    }

    #[test]
    fn rename_to_same_name_is_noop_success() {
        let mut q = SavedQueries::default();
        q.upsert(SavedQuery {
            name: "a".into(),
            body: "one".into(),
        });
        assert_eq!(q.rename("a", "a"), Ok(true));
        assert_eq!(q.entries.len(), 1);
        // Renaming a missing entry to itself reports "not found".
        assert_eq!(q.rename("ghost", "ghost"), Ok(false));
    }

    #[test]
    fn round_trip_via_temp_file() {
        let dir = std::env::temp_dir().join(format!("pgman-saved-rt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("saved.toml");
        let mut q = SavedQueries::default();
        q.upsert(SavedQuery {
            name: "active users".into(),
            body: "SELECT * FROM users\nWHERE active = true;".into(),
        });
        q.upsert(SavedQuery {
            name: "revenue".into(),
            body: "SELECT sum(amount) FROM orders;".into(),
        });
        save_to(&path, &q).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded, q);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_from_missing_file_returns_empty() {
        let path = std::env::temp_dir().join("definitely-not-a-saved-file");
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_from(&path), SavedQueries::default());
    }

    #[test]
    fn load_from_malformed_file_returns_empty_not_panic() {
        let dir = std::env::temp_dir().join(format!("pgman-saved-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("saved.toml");
        std::fs::write(&path, "}}}not toml{{").unwrap();
        assert_eq!(load_from(&path), SavedQueries::default());
        let _ = std::fs::remove_file(&path);
    }
}
