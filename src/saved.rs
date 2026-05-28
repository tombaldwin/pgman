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
