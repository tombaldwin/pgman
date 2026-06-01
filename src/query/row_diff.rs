//! Result diff — compare two result sets row-by-row.
//!
//! Feeds the `Mode::ResultDiff` view: pin one grid as A, run
//! another query for B, and see what changed. The "did my
//! migration / batch update break anything?" workflow.
//!
//! Everything here is pure over `Vec<Vec<String>>` (the grid's
//! cell representation), so it's exhaustively unit-tested and
//! carries no dependency on the live DB or the schema cache.
//!
//! ## Keying
//!
//! A diff needs a notion of row identity. Two strategies:
//!
//! - [`RowKey::Columns`] — match rows by the values in a set of
//!   key columns (a primary key, or an inferred unique column).
//!   This is the strong mode: rows present in both A and B but
//!   with differing non-key cells are reported as **changed**,
//!   with per-cell old→new deltas.
//! - [`RowKey::FullRow`] — match rows by their entire content.
//!   Used when no usable key exists. A row that changed looks
//!   like one removed + one added (full-row identity can't tell
//!   a mutation from a delete+insert), so `changed` is always
//!   empty in this mode.
//!
//! [`infer_key_column`] picks a key without needing the catalog:
//! the leftmost column whose values are unique across *both*
//! result sets. A primary-key / `id` column satisfies this by
//! definition, so the common case ("same query re-run against a
//! table with an id") gets proper change-detection for free.

use std::collections::HashMap;

/// How to establish row identity for the diff. See the module
/// docs for the trade-off between the two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKey {
    /// Match on the values of these column indices (in this
    /// order). Non-key cell differences become `changed` rows.
    Columns(Vec<usize>),
    /// Match on the entire row. No `changed` detection.
    FullRow,
}

/// One cell that differs between the matched A and B rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellChange {
    /// Column index within the row.
    pub col: usize,
    pub old: String,
    pub new: String,
}

/// A row present in both A and B (same key) whose non-key cells
/// differ. Only produced under [`RowKey::Columns`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowChange {
    /// The key values that matched the two rows.
    pub key: Vec<String>,
    /// Index of the row in A.
    pub a_index: usize,
    /// Index of the row in B.
    pub b_index: usize,
    /// The cells that differ, in ascending column order.
    pub cells: Vec<CellChange>,
}

/// The outcome of [`diff_rows`]. Index vectors point into the
/// original A (`removed`) / B (`added`) row slices so the
/// renderer can pull the full row for display.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowDiff {
    /// Indices into B of rows that have no match in A.
    pub added: Vec<usize>,
    /// Indices into A of rows that have no match in B.
    pub removed: Vec<usize>,
    /// Rows matched by key whose non-key cells changed.
    pub changed: Vec<RowChange>,
    /// Count of rows present and identical in both.
    pub unchanged: usize,
}

impl RowDiff {
    /// `true` when nothing was added, removed, or changed — the
    /// "no differences" signal the renderer surfaces as a
    /// positive result.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// Extract the key for `row` under `key`. For [`RowKey::Columns`]
/// a missing column index contributes an empty string so the key
/// stays positionally stable even on a short row.
fn key_of(row: &[String], key: &RowKey) -> Vec<String> {
    match key {
        RowKey::Columns(cols) => cols
            .iter()
            .map(|&i| row.get(i).cloned().unwrap_or_default())
            .collect(),
        RowKey::FullRow => row.to_vec(),
    }
}

/// Diff two result sets. `rows_a` is the pinned baseline, `rows_b`
/// the current result. See the module docs for keying semantics.
///
/// Determinism: `added` / `removed` are sorted ascending by
/// index; `changed` is sorted by `a_index`. Duplicate keys (which
/// a real PK never produces, but a user-chosen column might) are
/// paired in first-seen order; surplus A rows fall to `removed`
/// and surplus B rows to `added`.
pub fn diff_rows(rows_a: &[Vec<String>], rows_b: &[Vec<String>], key: &RowKey) -> RowDiff {
    // Bucket B's row indices by key, preserving order so dup keys
    // pair deterministically.
    let mut b_by_key: HashMap<Vec<String>, Vec<usize>> = HashMap::new();
    for (i, row) in rows_b.iter().enumerate() {
        b_by_key.entry(key_of(row, key)).or_default().push(i);
    }

    let mut diff = RowDiff::default();
    // Track which B rows we've consumed so leftovers become adds.
    let mut b_consumed = vec![false; rows_b.len()];

    for (ai, a_row) in rows_a.iter().enumerate() {
        let k = key_of(a_row, key);
        // Pop the next unconsumed B row with this key.
        let matched_b = b_by_key
            .get_mut(&k)
            .and_then(|idxs| idxs.iter().find(|&&bi| !b_consumed[bi]).copied());
        match matched_b {
            Some(bi) => {
                b_consumed[bi] = true;
                let b_row = &rows_b[bi];
                if a_row == b_row {
                    diff.unchanged += 1;
                } else {
                    // Equal key, differing content. Under FullRow
                    // this branch is unreachable (equal key ⇒ equal
                    // row), so cell-level changes only arise with a
                    // Columns key.
                    let cells = cell_changes(a_row, b_row);
                    if cells.is_empty() {
                        // Differ only outside the compared range
                        // (ragged rows) — treat as unchanged rather
                        // than inventing a change with no cells.
                        diff.unchanged += 1;
                    } else {
                        diff.changed.push(RowChange {
                            key: k,
                            a_index: ai,
                            b_index: bi,
                            cells,
                        });
                    }
                }
            }
            None => diff.removed.push(ai),
        }
    }

    // Any B row never consumed is an addition.
    for (bi, consumed) in b_consumed.iter().enumerate() {
        if !consumed {
            diff.added.push(bi);
        }
    }

    diff.added.sort_unstable();
    diff.removed.sort_unstable();
    diff.changed.sort_by_key(|c| c.a_index);
    diff
}

/// Per-cell differences between two rows, ascending by column.
/// Compares positionally up to the shorter row's length.
fn cell_changes(a: &[String], b: &[String]) -> Vec<CellChange> {
    let n = a.len().min(b.len());
    (0..n)
        .filter(|&i| a[i] != b[i])
        .map(|i| CellChange {
            col: i,
            old: a[i].clone(),
            new: b[i].clone(),
        })
        .collect()
}

/// Infer a usable single-column key without consulting the
/// catalog: the leftmost column index whose values are **unique
/// within both** `rows_a` and `rows_b`. Returns `None` when no
/// column is unique on both sides (then the caller falls back to
/// [`RowKey::FullRow`]).
///
/// A primary-key / `id` column is unique by definition, so this
/// recovers the strong diff mode for the overwhelmingly common
/// "re-ran the same query" case. `n_cols` bounds the search to
/// the declared column count (rows are assumed rectangular).
///
/// An empty result on either side makes every column trivially
/// unique; the leftmost (`0`) is returned when `n_cols > 0`.
pub fn infer_key_column(
    rows_a: &[Vec<String>],
    rows_b: &[Vec<String>],
    n_cols: usize,
) -> Option<usize> {
    (0..n_cols).find(|&col| col_is_unique(rows_a, col) && col_is_unique(rows_b, col))
}

/// Whether column `col` holds distinct values across every row.
/// A row too short to have the column contributes an empty
/// string (so a ragged column collides with another empty and
/// reads as non-unique, which is the safe call).
fn col_is_unique(rows: &[Vec<String>], col: usize) -> bool {
    let mut seen = std::collections::HashSet::with_capacity(rows.len());
    rows.iter().all(|row| {
        let v = row.get(col).map(String::as_str).unwrap_or("");
        seen.insert(v)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn diff_identical_sets_is_all_unchanged() {
        let a = vec![r(&["1", "alice"]), r(&["2", "bob"])];
        let key = RowKey::Columns(vec![0]);
        let d = diff_rows(&a, &a, &key);
        assert!(d.is_empty());
        assert_eq!(d.unchanged, 2);
    }

    #[test]
    fn diff_detects_added_and_removed_by_key() {
        let a = vec![r(&["1", "alice"]), r(&["2", "bob"])];
        let b = vec![r(&["2", "bob"]), r(&["3", "carol"])];
        let d = diff_rows(&a, &b, &RowKey::Columns(vec![0]));
        // 1 removed (id=1 at a-index 0), 1 added (id=3 at b-index 1).
        assert_eq!(d.removed, vec![0]);
        assert_eq!(d.added, vec![1]);
        assert_eq!(d.unchanged, 1);
        assert!(d.changed.is_empty());
    }

    #[test]
    fn diff_detects_changed_row_with_cell_deltas() {
        let a = vec![r(&["1", "alice", "x"])];
        let b = vec![r(&["1", "ALICE", "x"])];
        let d = diff_rows(&a, &b, &RowKey::Columns(vec![0]));
        assert_eq!(d.changed.len(), 1);
        let c = &d.changed[0];
        assert_eq!(c.key, vec!["1".to_string()]);
        assert_eq!(c.a_index, 0);
        assert_eq!(c.b_index, 0);
        assert_eq!(
            c.cells,
            vec![CellChange {
                col: 1,
                old: "alice".into(),
                new: "ALICE".into()
            }]
        );
        assert_eq!(d.unchanged, 0);
    }

    #[test]
    fn diff_changed_reports_only_differing_cells() {
        let a = vec![r(&["1", "alice", "10", "keep"])];
        let b = vec![r(&["1", "alice", "20", "keep"])];
        let d = diff_rows(&a, &b, &RowKey::Columns(vec![0]));
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.changed[0].cells.len(), 1);
        assert_eq!(d.changed[0].cells[0].col, 2);
    }

    #[test]
    fn full_row_key_treats_mutation_as_remove_plus_add() {
        let a = vec![r(&["1", "alice"])];
        let b = vec![r(&["1", "ALICE"])];
        let d = diff_rows(&a, &b, &RowKey::FullRow);
        assert_eq!(d.removed, vec![0]);
        assert_eq!(d.added, vec![0]);
        assert!(d.changed.is_empty());
        assert_eq!(d.unchanged, 0);
    }

    #[test]
    fn full_row_key_matches_identical_rows() {
        let a = vec![r(&["1", "alice"]), r(&["2", "bob"])];
        let b = vec![r(&["2", "bob"]), r(&["9", "zed"])];
        let d = diff_rows(&a, &b, &RowKey::FullRow);
        assert_eq!(d.unchanged, 1); // bob
        assert_eq!(d.removed, vec![0]); // alice
        assert_eq!(d.added, vec![1]); // zed
    }

    #[test]
    fn diff_handles_duplicate_keys_by_pairing_in_order() {
        // Two rows share key "1". A has two, B has one → one of A's
        // is removed; the matched pair is unchanged.
        let a = vec![r(&["1", "x"]), r(&["1", "y"])];
        let b = vec![r(&["1", "x"])];
        let d = diff_rows(&a, &b, &RowKey::Columns(vec![0]));
        assert_eq!(d.unchanged, 1);
        assert_eq!(d.removed, vec![1]);
        assert!(d.added.is_empty());
    }

    #[test]
    fn diff_empty_a_makes_everything_added() {
        let b = vec![r(&["1"]), r(&["2"])];
        let d = diff_rows(&[], &b, &RowKey::Columns(vec![0]));
        assert_eq!(d.added, vec![0, 1]);
        assert!(d.removed.is_empty());
        assert_eq!(d.unchanged, 0);
    }

    #[test]
    fn diff_empty_b_makes_everything_removed() {
        let a = vec![r(&["1"]), r(&["2"])];
        let d = diff_rows(&a, &[], &RowKey::Columns(vec![0]));
        assert_eq!(d.removed, vec![0, 1]);
        assert!(d.added.is_empty());
    }

    #[test]
    fn diff_both_empty_is_empty() {
        let d = diff_rows(&[], &[], &RowKey::FullRow);
        assert!(d.is_empty());
        assert_eq!(d.unchanged, 0);
    }

    #[test]
    fn infer_key_picks_leftmost_unique_column() {
        // col 0 has a dup ("1","1"); col 1 is unique on both sides.
        let a = vec![r(&["1", "a"]), r(&["1", "b"])];
        let b = vec![r(&["1", "a"]), r(&["1", "c"])];
        assert_eq!(infer_key_column(&a, &b, 2), Some(1));
    }

    #[test]
    fn infer_key_prefers_id_column_when_unique() {
        let a = vec![r(&["1", "alice"]), r(&["2", "alice"])];
        let b = vec![r(&["1", "alice"]), r(&["2", "bob"])];
        // col 0 unique on both; col 1 not. Leftmost unique = 0.
        assert_eq!(infer_key_column(&a, &b, 2), Some(0));
    }

    #[test]
    fn infer_key_none_when_no_column_unique_on_both() {
        // Every column has a duplicate somewhere.
        let a = vec![r(&["1", "x"]), r(&["1", "y"])];
        let b = vec![r(&["2", "z"]), r(&["3", "z"])];
        // col 0: a has dup "1" → not unique in a. col 1: b has dup
        // "z" → not unique in b. So neither works.
        assert_eq!(infer_key_column(&a, &b, 2), None);
    }

    #[test]
    fn infer_key_empty_side_makes_first_column_unique() {
        let b = vec![r(&["1", "x"])];
        assert_eq!(infer_key_column(&[], &b, 2), Some(0));
    }

    #[test]
    fn infer_key_zero_columns_is_none() {
        assert_eq!(infer_key_column(&[], &[], 0), None);
    }
}
