//! What masking needs to know about a database, read from its catalog
//! alone — no rows: each table whose rows are copied, its columns with
//! their types resolved through domains, and the keys that tie columns
//! together (primary keys, unique indexes, foreign keys, partition
//! keys). [`crate::query::masking::catalog`] reads it; everything here is
//! pure.

use std::collections::HashMap;

/// One row of the tables query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTable {
    pub oid: i64,
    pub schema: String,
    pub name: String,
    /// `r` table (a partition or inheritance child included), `p`
    /// partitioned parent, `f` foreign, `m` materialised view.
    pub kind: String,
    /// The partitioned table at the root, for a partition.
    pub root: Option<i64>,
    /// The table it inherits from or is a partition of, if any.
    pub parent: Option<i64>,
}

/// One row of the columns query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawColumn {
    pub table: i64,
    pub num: i16,
    pub name: String,
    pub type_oid: i64,
    pub typmod: i32,
    pub not_null: bool,
    /// `s` a stored generated column, else empty.
    pub generated: String,
    /// `format_type`'s text: `character varying(255)`.
    pub display: String,
}

/// One row of the types query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawType {
    pub oid: i64,
    pub name: String,
    pub schema: String,
    /// `b` base, `d` domain, `e` enum, `c` composite, `r` range, `m`
    /// multirange, `p` pseudo.
    pub kind: String,
    /// `S` string, `N` numeric, `D` date/time, `B` boolean, `A` array,
    /// `I` network, `U` user-defined, …
    pub category: String,
    /// A domain's base type.
    pub base: i64,
    pub typmod: i32,
    pub not_null: bool,
    /// An array's element type.
    pub elem: i64,
    /// An enum's first label.
    pub first_label: Option<String>,
}

/// One key: `p` primary, `u` unique (any unique index, expression and
/// partial ones included), `f` foreign.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawKey {
    pub table: i64,
    pub kind: String,
    pub columns: Vec<i16>,
    pub ref_table: Option<i64>,
    pub ref_columns: Vec<i16>,
}

/// What a column holds, resolved through domains to the base type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnType {
    /// As declared: `character varying(255)`, `email_address`.
    pub display: String,
    /// The base type's name: `varchar`, `text`, `int8`, `geometry`.
    pub base: String,
    pub base_schema: String,
    pub category: String,
    /// The base is an enum, its first label.
    pub enum_first: Option<String>,
    /// An array, and its element's base name.
    pub array_of: Option<String>,
    /// The most characters it takes (`varchar(n)`, `char(n)`).
    pub max_len: Option<usize>,
    /// NULL can't be stored: the column's NOT NULL or a domain's.
    pub not_null: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub num: i16,
    pub name: String,
    pub ty: ColumnType,
    /// Stored generated: never copied, the restore computes it.
    pub generated: bool,
    /// Under a unique index (alone or with others).
    pub unique: bool,
}

/// A foreign key: `columns` of this table reference `ref_columns` of
/// `ref_table`, position by position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKey {
    pub columns: Vec<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub schema: String,
    pub name: String,
    pub kind: String,
    pub columns: Vec<Column>,
    pub primary_key: Vec<String>,
    pub foreign_keys: Vec<ForeignKey>,
    /// Columns its partitioned root is partitioned by.
    pub partition_key: Vec<String>,
    /// `schema.name` of the table it inherits from or is a partition of.
    pub parent: Option<String>,
}

impl Table {
    /// `schema.name`, as rules and messages name it.
    pub fn qualified(&self) -> String {
        format!("{}.{}", self.schema, self.name)
    }

    /// Whether its rows are copied: a plain table, a partition or an
    /// inheritance child. A partitioned parent has none of its own, a
    /// foreign table's are elsewhere, a materialised view is refreshed.
    pub fn has_rows(&self) -> bool {
        self.kind == "r"
    }

    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    pub tables: Vec<Table>,
    pub large_objects: i64,
}

impl Catalog {
    pub fn table(&self, qualified: &str) -> Option<&Table> {
        self.tables.iter().find(|t| t.qualified() == qualified)
    }
}

/// The characters a `varchar(n)` / `char(n)` typmod allows. Pure.
fn length_of(base: &str, typmod: i32) -> Option<usize> {
    match base {
        "varchar" | "bpchar" if typmod >= 4 => Some((typmod - 4) as usize),
        _ => None,
    }
}

/// `type_oid` (with the column's `typmod`) resolved through domains:
/// the base, its length, and whether a domain forbids NULL. Pure.
fn resolve(
    types: &HashMap<i64, &RawType>,
    type_oid: i64,
    typmod: i32,
    display: &str,
) -> ColumnType {
    let mut oid = type_oid;
    let mut typmod = typmod;
    let mut not_null = false;
    // A domain over a domain over a base: follow it, a few deep at most.
    for _ in 0..16 {
        match types.get(&oid) {
            Some(t) if t.kind == "d" => {
                not_null |= t.not_null;
                if t.typmod >= 0 {
                    typmod = t.typmod;
                }
                oid = t.base;
            }
            _ => break,
        }
    }
    let t = types.get(&oid);
    let base = t
        .map(|t| t.name.clone())
        .unwrap_or_else(|| "unknown".into());
    let array_of = t
        .filter(|t| t.category == "A")
        .and_then(|t| types.get(&t.elem).map(|e| e.name.clone()));
    ColumnType {
        display: display.to_string(),
        max_len: length_of(&base, typmod),
        base_schema: t.map(|t| t.schema.clone()).unwrap_or_default(),
        category: t.map(|t| t.category.clone()).unwrap_or_default(),
        enum_first: t
            .filter(|t| t.kind == "e")
            .and_then(|t| t.first_label.clone()),
        array_of,
        base,
        not_null,
    }
}

/// The catalog from the queries' rows. Pure.
pub fn build(
    tables: &[RawTable],
    columns: &[RawColumn],
    types: &[RawType],
    keys: &[RawKey],
    partition_keys: &[(i64, Vec<i16>)],
    large_objects: i64,
) -> Catalog {
    let types: HashMap<i64, &RawType> = types.iter().map(|t| (t.oid, t)).collect();
    let name_of = |table: i64, num: i16| -> Option<String> {
        columns
            .iter()
            .find(|c| c.table == table && c.num == num)
            .map(|c| c.name.clone())
    };
    let qualified: HashMap<i64, String> = tables
        .iter()
        .map(|t| (t.oid, format!("{}.{}", t.schema, t.name)))
        .collect();
    let mut out = Vec::new();
    for t in tables {
        let own_keys: Vec<&RawKey> = keys.iter().filter(|k| k.table == t.oid).collect();
        let unique: Vec<i16> = own_keys
            .iter()
            .filter(|k| k.kind == "u" || k.kind == "p")
            .flat_map(|k| k.columns.iter().copied())
            .collect();
        let mut cols: Vec<Column> = columns
            .iter()
            .filter(|c| c.table == t.oid)
            .map(|c| Column {
                num: c.num,
                name: c.name.clone(),
                ty: {
                    let mut ty = resolve(&types, c.type_oid, c.typmod, &c.display);
                    ty.not_null |= c.not_null;
                    ty
                },
                generated: c.generated == "s",
                unique: unique.contains(&c.num),
            })
            .collect();
        cols.sort_by_key(|c| c.num);
        let primary_key = own_keys
            .iter()
            .find(|k| k.kind == "p")
            .map(|k| {
                k.columns
                    .iter()
                    .filter_map(|n| name_of(t.oid, *n))
                    .collect()
            })
            .unwrap_or_default();
        let foreign_keys = own_keys
            .iter()
            .filter(|k| k.kind == "f")
            .filter_map(|k| {
                let r = k.ref_table?;
                Some(ForeignKey {
                    columns: k
                        .columns
                        .iter()
                        .filter_map(|n| name_of(t.oid, *n))
                        .collect(),
                    ref_table: qualified.get(&r)?.clone(),
                    ref_columns: k
                        .ref_columns
                        .iter()
                        .filter_map(|n| name_of(r, *n))
                        .collect(),
                })
            })
            .collect();
        let root = t.root.unwrap_or(t.oid);
        let partition_key = partition_keys
            .iter()
            .find(|(rel, _)| *rel == root)
            .map(|(_, nums)| nums.iter().filter_map(|n| name_of(root, *n)).collect())
            .unwrap_or_default();
        out.push(Table {
            schema: t.schema.clone(),
            name: t.name.clone(),
            kind: t.kind.clone(),
            columns: cols,
            primary_key,
            foreign_keys,
            partition_key,
            parent: t.parent.and_then(|p| qualified.get(&p).cloned()),
        });
    }
    Catalog {
        tables: out,
        large_objects,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn ty(oid: i64, name: &str, kind: &str, category: &str) -> RawType {
        RawType {
            oid,
            name: name.into(),
            schema: "pg_catalog".into(),
            kind: kind.into(),
            category: category.into(),
            base: 0,
            typmod: -1,
            not_null: false,
            elem: 0,
            first_label: None,
        }
    }

    pub(crate) fn col(
        table: i64,
        num: i16,
        name: &str,
        type_oid: i64,
        typmod: i32,
        not_null: bool,
    ) -> RawColumn {
        RawColumn {
            table,
            num,
            name: name.into(),
            type_oid,
            typmod,
            not_null,
            generated: String::new(),
            display: name.into(),
        }
    }

    #[test]
    fn types_resolve_through_domains_with_their_length_and_not_null() {
        let mut email = ty(9000, "email_address", "d", "S");
        email.base = 9001;
        email.not_null = true;
        let mut inner = ty(9001, "short_text", "d", "S");
        inner.base = 1043;
        inner.typmod = 54;
        let types = vec![
            ty(1043, "varchar", "b", "S"),
            ty(25, "text", "b", "S"),
            email,
            inner,
            {
                let mut a = ty(1009, "_text", "b", "A");
                a.elem = 25;
                a
            },
            {
                let mut e = ty(9100, "gender", "e", "E");
                e.schema = "public".into();
                e.first_label = Some("unspecified".into());
                e
            },
        ];
        let tables = vec![RawTable {
            oid: 1,
            schema: "public".into(),
            name: "customers".into(),
            kind: "r".into(),
            root: None,
            parent: None,
        }];
        let columns = vec![
            col(1, 1, "id", 1043, 40, true),
            col(1, 2, "email", 9000, -1, false),
            col(1, 3, "tags", 1009, -1, false),
            col(1, 4, "gender", 9100, -1, true),
        ];
        let keys = vec![RawKey {
            table: 1,
            kind: "p".into(),
            columns: vec![1],
            ref_table: None,
            ref_columns: vec![],
        }];
        let c = build(&tables, &columns, &types, &keys, &[], 0);
        let t = &c.tables[0];
        assert_eq!(t.primary_key, ["id"]);
        let id = t.column("id").unwrap();
        assert_eq!(
            (id.ty.base.as_str(), id.ty.max_len, id.unique),
            ("varchar", Some(36), true)
        );
        let email = t.column("email").unwrap();
        assert_eq!(email.ty.base, "varchar");
        assert_eq!(email.ty.max_len, Some(50), "the inner domain's length");
        assert!(email.ty.not_null, "the outer domain's NOT NULL");
        assert_eq!(
            t.column("tags").unwrap().ty.array_of.as_deref(),
            Some("text")
        );
        assert_eq!(
            t.column("gender").unwrap().ty.enum_first.as_deref(),
            Some("unspecified")
        );
    }

    #[test]
    fn foreign_and_partition_keys_are_named() {
        let types = vec![ty(23, "int4", "b", "N"), ty(1082, "date", "b", "D")];
        let table = |oid, name: &str, kind: &str, root| RawTable {
            oid,
            schema: "public".into(),
            name: name.into(),
            kind: kind.into(),
            root,
            parent: root,
        };
        let tables = vec![
            table(1, "customers", "r", None),
            table(2, "orders", "p", None),
            table(3, "orders_2026", "r", Some(2)),
        ];
        let columns = vec![
            col(1, 1, "id", 23, -1, true),
            col(2, 1, "id", 23, -1, true),
            col(2, 2, "customer_id", 23, -1, true),
            col(2, 3, "placed_on", 1082, -1, true),
            col(3, 1, "id", 23, -1, true),
            col(3, 2, "customer_id", 23, -1, true),
            col(3, 3, "placed_on", 1082, -1, true),
        ];
        let keys = vec![RawKey {
            table: 3,
            kind: "f".into(),
            columns: vec![2],
            ref_table: Some(1),
            ref_columns: vec![1],
        }];
        let c = build(&tables, &columns, &types, &keys, &[(2, vec![3])], 4);
        let leaf = c.table("public.orders_2026").unwrap();
        assert!(leaf.has_rows());
        assert!(!c.table("public.orders").unwrap().has_rows());
        assert_eq!(leaf.partition_key, ["placed_on"]);
        assert_eq!(
            leaf.foreign_keys,
            [ForeignKey {
                columns: vec!["customer_id".into()],
                ref_table: "public.customers".into(),
                ref_columns: vec!["id".into()],
            }]
        );
        assert_eq!(c.large_objects, 4);
    }
}
