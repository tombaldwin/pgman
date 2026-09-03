//! N+1 detection — cluster reconstructed queries by shape.
//!
//! A loop that issues the same statement many times (a missing `JOIN FETCH`,
//! a lazy collection walked in application code) shows up as one query *shape*
//! repeated in a tight burst. `fingerprint` reduces a statement to that shape;
//! `detect` groups by it.
//!
//! Future refinement (BACKLOG.md): a time-window heuristic once
//! `ReconstructedQuery` carries timestamps — repetition across a whole day
//! isn't an N+1, repetition within 100ms is.

use crate::query::reconstruct::ReconstructedQuery;
use std::collections::HashMap;

/// Reduce a statement to a shape fingerprint: lowercased, whitespace collapsed,
/// and literals / placeholders replaced with `?`. Two statements with the same
/// fingerprint are the "same query" for N+1 purposes.
pub fn fingerprint(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    // Last source char seen — distinguishes a numeric literal (`= 1`) from a
    // digit inside an identifier (`col1`).
    let mut prev = ' ';
    let mut pending_space = false;

    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            pending_space = true;
            prev = ' ';
            continue;
        }
        if pending_space && !out.is_empty() {
            out.push(' ');
        }
        pending_space = false;

        if c == '\'' {
            // String literal — skip to the closing quote ('' escapes).
            while let Some(cc) = chars.next() {
                if cc == '\'' {
                    if chars.peek() == Some(&'\'') {
                        chars.next();
                    } else {
                        break;
                    }
                }
            }
            out.push('?');
            prev = '?';
            continue;
        }
        if c == '$' && chars.peek().is_some_and(|d| d.is_ascii_digit()) {
            while chars.peek().is_some_and(|d| d.is_ascii_digit()) {
                chars.next();
            }
            out.push('?');
            prev = '?';
            continue;
        }
        if c == '?' {
            out.push('?');
            prev = '?';
            continue;
        }
        if c.is_ascii_digit() && !(prev.is_ascii_alphanumeric() || prev == '_') {
            // Numeric literal — collapse digits and dots to one `?`.
            while chars
                .peek()
                .is_some_and(|d| d.is_ascii_digit() || *d == '.')
            {
                chars.next();
            }
            out.push('?');
            prev = '?';
            continue;
        }
        for lc in c.to_lowercase() {
            out.push(lc);
        }
        prev = c;
    }
    out
}

/// A group of reconstructed queries sharing a fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cluster {
    pub fingerprint: String,
    pub count: usize,
    /// One representative statement from the cluster — the raw,
    /// unsubstituted shape (`… order_id=?`), which is what the cluster
    /// groups by and what the cluster view shows. Not runnable when
    /// the log used placeholders; see [`runnable_member`].
    pub example: String,
    /// Indices, into the slice `detect` was given, of every query in
    /// the cluster — in log order. `members.len() == count`.
    pub members: Vec<usize>,
}

/// Does `sql` still carry an unbound placeholder — a `?` or `$N`
/// outside a string literal? A query reconstructed from a log with no
/// bind-parameter lines keeps its template form, which cannot run.
pub fn has_unbound_placeholder(sql: &str) -> bool {
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                // String literal — skip to the closing quote ('' escapes).
                while let Some(cc) = chars.next() {
                    if cc == '\'' {
                        if chars.peek() == Some(&'\'') {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                }
            }
            '?' => return true,
            '$' if chars.peek().is_some_and(|d| d.is_ascii_digit()) => return true,
            _ => {}
        }
    }
    false
}

/// The member of `cluster` to load when the operator picks it: the
/// first, in log order, whose `runnable_sql` has every value bound.
/// `None` when the log bound no values for this shape at all — the
/// caller falls back to the template and says so.
pub fn runnable_member<'a>(
    cluster: &Cluster,
    queries: &'a [ReconstructedQuery],
) -> Option<&'a ReconstructedQuery> {
    cluster
        .members
        .iter()
        .filter_map(|&i| queries.get(i))
        .find(|q| !has_unbound_placeholder(&q.runnable_sql))
}

/// A one-line triage view of an imported log: how many queries
/// total, how many distinct N+1 clusters, how many of the queries
/// are part of any cluster, and a representative slow leader.
/// Timing-independent — adding per-query durations is a separate
/// backlog item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub total_queries: usize,
    /// Number of distinct fingerprints that fired 2+ times.
    pub cluster_count: usize,
    /// Sum of `cluster.count` over all clusters — i.e. how many of
    /// the queries are repeats. Always `<= total_queries`.
    pub repeated_queries: usize,
    /// The most-repeated cluster, if any. `None` when no
    /// fingerprint fired more than once.
    pub top_cluster: Option<Cluster>,
}

impl SessionSummary {
    /// Compact summary suited for a one-line header above the log
    /// picker. Mentions the top cluster only when one exists.
    pub fn one_line(&self) -> String {
        if self.total_queries == 0 {
            return "no queries imported".to_string();
        }
        let mut s = format!(
            "{} {}",
            self.total_queries,
            if self.total_queries == 1 {
                "query"
            } else {
                "queries"
            }
        );
        if self.cluster_count > 0 {
            s.push_str(&format!(
                " · {} N+1 cluster{} ({} of {} repeated)",
                self.cluster_count,
                if self.cluster_count == 1 { "" } else { "s" },
                self.repeated_queries,
                self.total_queries,
            ));
        }
        s
    }
}

/// Build a one-line triage summary over the imported reconstructed
/// queries.
pub fn summarize(queries: &[ReconstructedQuery]) -> SessionSummary {
    let clusters = detect(queries);
    let repeated_queries = clusters.iter().map(|c| c.count).sum();
    let top_cluster = clusters.first().cloned();
    SessionSummary {
        total_queries: queries.len(),
        cluster_count: clusters.len(),
        repeated_queries,
        top_cluster,
    }
}

/// Cluster reconstructed queries by fingerprint. Only clusters seen 2+ times
/// are returned, most-repeated first — a repeated shape is the N+1 signature.
pub fn detect(queries: &[ReconstructedQuery]) -> Vec<Cluster> {
    let mut groups: HashMap<String, (String, Vec<usize>)> = HashMap::new();
    for (i, q) in queries.iter().enumerate() {
        let fp = fingerprint(&q.raw_sql);
        let entry = groups
            .entry(fp)
            .or_insert_with(|| (q.raw_sql.clone(), Vec::new()));
        entry.1.push(i);
    }
    let mut clusters: Vec<Cluster> = groups
        .into_iter()
        .filter(|(_, (_, members))| members.len() >= 2)
        .map(|(fingerprint, (example, members))| Cluster {
            fingerprint,
            count: members.len(),
            example,
            members,
        })
        .collect();
    // Most-repeated first; fingerprint as a stable tiebreak for deterministic
    // output.
    clusters.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.fingerprint.cmp(&b.fingerprint))
    });
    clusters
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::reconstruct::Source;

    fn rq(raw_sql: &str) -> ReconstructedQuery {
        ReconstructedQuery {
            raw_sql: raw_sql.to_string(),
            params: Vec::new(),
            runnable_sql: raw_sql.to_string(),
            source: Source::HibernateLog,
            src_line: 0,
        }
    }

    #[test]
    fn fingerprint_normalises_whitespace_and_case() {
        assert_eq!(
            fingerprint("SELECT  *  FROM   Users"),
            fingerprint("select * from users")
        );
    }

    #[test]
    fn fingerprint_collapses_literals_and_placeholders() {
        let a = fingerprint("select * from t where id = 5 and name = 'alice'");
        let b = fingerprint("select * from t where id = 99 and name = 'bob'");
        let c = fingerprint("select * from t where id = $1 and name = ?");
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    #[test]
    fn fingerprint_keeps_digits_inside_identifiers() {
        // `col1` is an identifier, not `col` followed by a literal.
        let fp = fingerprint("select col1 from t2");
        assert!(fp.contains("col1"), "got {fp:?}");
        assert!(fp.contains("t2"), "got {fp:?}");
    }

    #[test]
    fn detect_flags_repeated_shapes() {
        let queries = vec![
            rq("select * from item where order_id = 1"),
            rq("select * from item where order_id = 2"),
            rq("select * from item where order_id = 3"),
            rq("select * from orders where id = 1"),
        ];
        let clusters = detect(&queries);
        // The order-line lookup repeats 3×; the single orders lookup does not.
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].count, 3);
        assert!(clusters[0].fingerprint.contains("from item"));
    }

    #[test]
    fn detect_records_each_member_in_log_order() {
        let queries = vec![
            rq("select * from item where order_id = 1"),
            rq("select * from orders where id = 1"),
            rq("select * from item where order_id = 2"),
            rq("select * from item where order_id = 3"),
        ];
        let clusters = detect(&queries);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].members, vec![0, 2, 3]);
        assert_eq!(clusters[0].count, clusters[0].members.len());
    }

    #[test]
    fn has_unbound_placeholder_sees_question_marks_and_dollar_numbers_outside_literals() {
        assert!(has_unbound_placeholder(
            "select * from item where order_id=?"
        ));
        assert!(has_unbound_placeholder(
            "select * from item where order_id=$1"
        ));
        assert!(!has_unbound_placeholder(
            "select * from item where order_id=7"
        ));
        // Inside a string literal a `?` is data, not a placeholder —
        // and a doubled quote does not end the literal early.
        assert!(!has_unbound_placeholder(
            "select * from faq where q = 'why?' and a = 'it''s ?'"
        ));
        assert!(!has_unbound_placeholder("select '$' || 1"));
        assert!(has_unbound_placeholder("select 'ok' from t where x = ?"));
    }

    /// A member with its values bound, and one without (the log had no
    /// bind lines for it): `runnable_sql` keeps the template.
    fn rq_bound(raw_sql: &str, runnable_sql: &str) -> ReconstructedQuery {
        ReconstructedQuery {
            raw_sql: raw_sql.to_string(),
            params: Vec::new(),
            runnable_sql: runnable_sql.to_string(),
            source: Source::HibernateLog,
            src_line: 0,
        }
    }

    #[test]
    fn runnable_member_is_the_first_fully_substituted_one() {
        let queries = vec![
            rq_bound(
                "select * from item where order_id=?",
                "select * from item where order_id=?",
            ),
            rq_bound(
                "select * from item where order_id=?",
                "select * from item where order_id=7",
            ),
            rq_bound(
                "select * from item where order_id=?",
                "select * from item where order_id=8",
            ),
        ];
        let clusters = detect(&queries);
        assert_eq!(clusters.len(), 1);
        let m = runnable_member(&clusters[0], &queries).expect("one member is bound");
        assert_eq!(m.runnable_sql, "select * from item where order_id=7");
    }

    #[test]
    fn runnable_member_is_none_when_no_member_is_bound() {
        let queries = vec![
            rq_bound(
                "select * from item where order_id=?",
                "select * from item where order_id=?",
            ),
            rq_bound(
                "select * from item where order_id=?",
                "select * from item where order_id=?",
            ),
        ];
        let clusters = detect(&queries);
        assert_eq!(clusters.len(), 1);
        assert!(runnable_member(&clusters[0], &queries).is_none());
    }

    #[test]
    fn detect_ignores_one_off_queries() {
        let queries = vec![rq("select 1"), rq("select 2 from t")];
        assert!(detect(&queries).is_empty());
    }

    #[test]
    fn summarize_empty_log_reports_no_queries() {
        let s = summarize(&[]);
        assert_eq!(s.total_queries, 0);
        assert_eq!(s.cluster_count, 0);
        assert!(s.top_cluster.is_none());
        assert_eq!(s.one_line(), "no queries imported");
    }

    #[test]
    fn summarize_counts_clusters_and_repeats() {
        let queries = vec![
            rq("select * from item where order_id = 1"),
            rq("select * from item where order_id = 2"),
            rq("select * from item where order_id = 3"),
            rq("select * from orders where id = 1"),
            // A second cluster of 2 — same shape twice.
            rq("select * from product where id = 1"),
            rq("select * from product where id = 2"),
        ];
        let s = summarize(&queries);
        assert_eq!(s.total_queries, 6);
        assert_eq!(s.cluster_count, 2);
        assert_eq!(s.repeated_queries, 5); // 3 + 2
        let top = s.top_cluster.as_ref().unwrap();
        assert_eq!(top.count, 3);
        assert!(top.fingerprint.contains("from item"));
    }

    #[test]
    fn summarize_one_line_singular_plural_forms() {
        // Singular "query" for n=1.
        let queries = vec![rq("select 1")];
        let s = summarize(&queries);
        assert_eq!(s.one_line(), "1 query");
        // Plural and no clusters → no cluster suffix. Use two
        // structurally distinct shapes so fingerprints differ.
        let queries = vec![rq("select 1"), rq("select * from t")];
        let s = summarize(&queries);
        assert_eq!(s.one_line(), "2 queries");
        // Single cluster → "cluster" (singular) in the suffix.
        let queries = vec![rq("select 1"), rq("select 1")];
        let s = summarize(&queries);
        assert!(s.one_line().contains("1 N+1 cluster ("));
    }
}
