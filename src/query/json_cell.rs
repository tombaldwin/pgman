//! Tree-shaped view of a JSONB cell for the CellDetail navigator.
//!
//! Pure: takes a `&str` (the rendered cell value), parses it with
//! `serde_json`, and emits a `Vec<JsonRow>` covering every value
//! in depth-first order. The key handler + renderer in
//! `app.rs` / `ui.rs` consume the same row list, so test coverage
//! of this module covers the navigator's spine.

use serde_json::Value;

/// One visible row in the flattened JSON tree.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonRow {
    /// jq-style path to this node: `.foo[0].bar`. The root is the
    /// empty string `""`; the operator pressing `y` yanks this.
    pub path: String,
    /// Depth from the root (0 for the root itself). Drives indent.
    pub depth: usize,
    /// What to render before the value: for object members, the
    /// quoted key; for array members, the bracket-index. Empty for
    /// the root.
    pub key: String,
    /// What to render as the value:
    ///   - `Scalar(text)` for `null` / bool / number / string.
    ///   - `Container { kind, len, expanded }` for `{}` / `[]`.
    pub display: JsonDisplay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonDisplay {
    Scalar(String),
    Container {
        kind: ContainerKind,
        len: usize,
        expanded: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerKind {
    Object,
    Array,
}

/// Try to parse `s` as JSON; return the parsed value when it's an
/// object or array (the cases worth a tree view). Scalar JSON
/// (just a bare number / string / null) returns None — the
/// existing wrapped-text rendering already serves those fine.
pub fn parse_jsonb_cell(s: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(s.trim()).ok()?;
    match v {
        Value::Object(_) | Value::Array(_) => Some(v),
        _ => None,
    }
}

/// Flatten the JSON tree into a row list honouring the
/// `collapsed` paths. Stable order: object keys preserved as
/// `serde_json` parsed them (insertion order via `preserve_order`
/// is off by default, so they're alphabetical from the BTreeMap
/// `serde_json` uses by default — which is fine, predictable).
/// Arrays preserve element order.
pub fn flatten(value: &Value, collapsed: &std::collections::HashSet<String>) -> Vec<JsonRow> {
    let mut out = Vec::new();
    walk(value, "", String::new(), 0, collapsed, &mut out);
    out
}

fn walk(
    value: &Value,
    key: &str,
    path: String,
    depth: usize,
    collapsed: &std::collections::HashSet<String>,
    out: &mut Vec<JsonRow>,
) {
    let is_collapsed = collapsed.contains(&path);
    match value {
        Value::Object(map) => {
            out.push(JsonRow {
                path: path.clone(),
                depth,
                key: key.to_string(),
                display: JsonDisplay::Container {
                    kind: ContainerKind::Object,
                    len: map.len(),
                    expanded: !is_collapsed,
                },
            });
            if is_collapsed {
                return;
            }
            for (k, v) in map {
                let child_path = format!("{path}.{k}");
                let child_key = format!("\"{k}\"");
                walk(v, &child_key, child_path, depth + 1, collapsed, out);
            }
        }
        Value::Array(arr) => {
            out.push(JsonRow {
                path: path.clone(),
                depth,
                key: key.to_string(),
                display: JsonDisplay::Container {
                    kind: ContainerKind::Array,
                    len: arr.len(),
                    expanded: !is_collapsed,
                },
            });
            if is_collapsed {
                return;
            }
            for (i, v) in arr.iter().enumerate() {
                let child_path = format!("{path}[{i}]");
                let child_key = format!("[{i}]");
                walk(v, &child_key, child_path, depth + 1, collapsed, out);
            }
        }
        Value::Null => out.push(JsonRow {
            path,
            depth,
            key: key.to_string(),
            display: JsonDisplay::Scalar("null".into()),
        }),
        Value::Bool(b) => out.push(JsonRow {
            path,
            depth,
            key: key.to_string(),
            display: JsonDisplay::Scalar(b.to_string()),
        }),
        Value::Number(n) => out.push(JsonRow {
            path,
            depth,
            key: key.to_string(),
            display: JsonDisplay::Scalar(n.to_string()),
        }),
        Value::String(s) => out.push(JsonRow {
            path,
            depth,
            key: key.to_string(),
            display: JsonDisplay::Scalar(format!("\"{}\"", s.replace('"', "\\\""))),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn parse_jsonb_cell_returns_object_or_array_only() {
        assert!(parse_jsonb_cell("{}").is_some());
        assert!(parse_jsonb_cell("[]").is_some());
        assert!(parse_jsonb_cell(r#"{"a": 1}"#).is_some());
        assert!(parse_jsonb_cell("[1, 2]").is_some());
        // Scalars → None (use the wrapped-text renderer).
        assert!(parse_jsonb_cell("42").is_none());
        assert!(parse_jsonb_cell(r#""hello""#).is_none());
        assert!(parse_jsonb_cell("null").is_none());
        // Not JSON.
        assert!(parse_jsonb_cell("not json").is_none());
    }

    #[test]
    fn flatten_emits_object_and_children_in_order() {
        let v = parse_jsonb_cell(r#"{"id":1,"name":"alice"}"#).unwrap();
        let rows = flatten(&v, &HashSet::new());
        // Root + 2 members.
        assert_eq!(rows.len(), 3);
        assert!(matches!(
            rows[0].display,
            JsonDisplay::Container {
                kind: ContainerKind::Object,
                len: 2,
                expanded: true,
            }
        ));
        // serde_json maps are sorted by default (BTreeMap-backed).
        assert_eq!(rows[1].path, ".id");
        assert_eq!(rows[2].path, ".name");
    }

    #[test]
    fn flatten_array_elements_use_bracket_index_paths() {
        let v = parse_jsonb_cell("[10, 20, 30]").unwrap();
        let rows = flatten(&v, &HashSet::new());
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[1].path, "[0]");
        assert_eq!(rows[2].path, "[1]");
        assert_eq!(rows[3].path, "[2]");
        assert!(matches!(&rows[1].display, JsonDisplay::Scalar(s) if s == "10"));
    }

    #[test]
    fn flatten_skips_children_of_collapsed_containers() {
        let v = parse_jsonb_cell(r#"{"a":{"b":1,"c":2},"d":3}"#).unwrap();
        // Collapse the .a object.
        let mut collapsed = HashSet::new();
        collapsed.insert(".a".to_string());
        let rows = flatten(&v, &collapsed);
        // Root + .a (collapsed, no children) + .d.
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].path, ".a");
        assert!(matches!(
            rows[1].display,
            JsonDisplay::Container {
                expanded: false,
                ..
            }
        ));
        assert_eq!(rows[2].path, ".d");
    }

    #[test]
    fn nested_paths_use_jq_dot_bracket_style() {
        let v = parse_jsonb_cell(r#"{"users":[{"name":"alice"}]}"#).unwrap();
        let rows = flatten(&v, &HashSet::new());
        // Find the deepest scalar — should be `.users[0].name`.
        let leaf = rows
            .iter()
            .find(|r| matches!(r.display, JsonDisplay::Scalar(_)))
            .expect("scalar leaf");
        assert_eq!(leaf.path, ".users[0].name");
    }
}
