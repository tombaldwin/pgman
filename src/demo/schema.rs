//! The `--demo` database: `ledger`, the billing database of a B2B
//! subscriptions product — customers on plans, invoices and their
//! lines, payments, refunds and credit notes, metered usage partitioned
//! by month, outgoing webhooks, an audit log, and a `reporting` schema
//! of views over it all.
//!
//! Everything is declared once, in [`RELATIONS`], [`FOREIGN_KEYS`],
//! [`INDEXES`] and [`CHECKS`]; the schema cache, the DDL copy, the
//! EXPLAIN estimates and the generated rows are all read from those
//! tables, so they can't disagree with one another. The values are
//! made up: every company, person and domain is fictional, and every
//! address is under the reserved `.example` TLD.

use std::collections::HashMap;

use crate::query::schema::{
    ColumnMeta, ConstraintMeta, FkEdge, IndexMeta, SchemaCache, TableMeta, TableSize,
};

/// What kind of relation, as far as the demo needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Table,
    /// A partitioned table and its partition key.
    Partitioned(&'static str),
    /// A partition of `parent`, with its bound as `pg_get_expr` writes it.
    Partition {
        parent: &'static str,
        bound: &'static str,
    },
    /// A view, with its definition as `pg_get_viewdef` writes it.
    View {
        definition: &'static str,
        materialized: bool,
    },
}

/// `(name, format_type, not null)`.
type Col = (&'static str, &'static str, bool);

/// One relation of the demo database.
pub(super) struct Rel {
    pub schema: &'static str,
    pub name: &'static str,
    pub kind: Kind,
    pub columns: &'static [Col],
    /// The primary key's columns, when it has one.
    pub pk: Option<&'static str>,
    /// The planner's row estimate (`reltuples`).
    pub rows: u64,
    /// `(table, total)` bytes: the heap, then heap + indexes + TOAST.
    pub bytes: (u64, u64),
}

const KB: u64 = 1024;
const MB: u64 = 1024 * KB;
const GB: u64 = 1024 * MB;

const TS: &str = "timestamp with time zone";
const MONEY: &str = "numeric(12,2)";
const CURRENCY: &str = "character(3)";

const USAGE_EVENTS: &[Col] = &[
    ("id", "bigint", true),
    ("subscription_item_id", "bigint", true),
    ("metric", "text", true),
    ("quantity", "bigint", true),
    ("idempotency_key", "uuid", true),
    ("occurred_at", TS, true),
];

pub(super) const RELATIONS: &[Rel] = &[
    Rel {
        schema: "public",
        name: "accounts",
        kind: Kind::Table,
        columns: &[
            ("id", "bigint", true),
            ("customer_id", "bigint", true),
            ("balance", MONEY, true),
            ("currency", CURRENCY, true),
            ("updated_at", TS, true),
        ],
        pk: Some("id"),
        rows: 3_412,
        bytes: (280 * KB, 512 * KB),
    },
    Rel {
        schema: "public",
        name: "audit_log",
        kind: Kind::Table,
        columns: &[
            ("id", "bigint", true),
            ("actor_user_id", "bigint", false),
            ("action", "text", true),
            ("entity", "text", true),
            ("entity_id", "bigint", true),
            ("changes", "jsonb", true),
            ("at", TS, true),
        ],
        pk: Some("id"),
        rows: 9_812_334,
        bytes: (3 * GB + 140 * MB, 3 * GB + 610 * MB),
    },
    Rel {
        schema: "public",
        name: "coupons",
        kind: Kind::Table,
        columns: &[
            ("id", "bigint", true),
            ("code", "text", true),
            ("percent_off", "numeric(5,2)", false),
            ("amount_off", MONEY, false),
            ("valid_until", TS, false),
            ("redemptions", "integer", true),
        ],
        pk: Some("id"),
        rows: 412,
        bytes: (64 * KB, 128 * KB),
    },
    Rel {
        schema: "public",
        name: "credit_notes",
        kind: Kind::Table,
        columns: &[
            ("id", "bigint", true),
            ("invoice_id", "bigint", true),
            ("number", "text", true),
            ("amount", MONEY, true),
            ("reason", "text", true),
            ("issued_at", TS, true),
        ],
        pk: Some("id"),
        rows: 20_310,
        bytes: (2 * MB + 400 * KB, 3 * MB + 900 * KB),
    },
    Rel {
        schema: "public",
        name: "customers",
        kind: Kind::Table,
        columns: &[
            ("id", "bigint", true),
            ("public_id", "uuid", true),
            ("name", "text", true),
            ("billing_email", "citext", true),
            ("country", "character(2)", true),
            ("currency", CURRENCY, true),
            ("tax_rate_id", "bigint", false),
            ("status", "text", true),
            ("created_at", TS, true),
            ("metadata", "jsonb", true),
        ],
        pk: Some("id"),
        rows: 3_412,
        bytes: (MB + 900 * KB, 3 * MB + 400 * KB),
    },
    Rel {
        schema: "public",
        name: "flyway_schema_history",
        kind: Kind::Table,
        columns: &[
            ("installed_rank", "integer", true),
            ("version", "character varying(50)", false),
            ("description", "character varying(200)", true),
            ("type", "character varying(20)", true),
            ("script", "character varying(1000)", true),
            ("checksum", "integer", false),
            ("installed_by", "character varying(100)", true),
            ("installed_on", "timestamp without time zone", true),
            ("execution_time", "integer", true),
            ("success", "boolean", true),
        ],
        pk: Some("installed_rank"),
        rows: 212,
        bytes: (48 * KB, 96 * KB),
    },
    Rel {
        schema: "public",
        name: "invoice_lines",
        kind: Kind::Table,
        columns: &[
            ("id", "bigint", true),
            ("invoice_id", "bigint", true),
            ("description", "text", true),
            ("quantity", "integer", true),
            ("unit_amount", MONEY, true),
            ("amount", MONEY, true),
            ("tax_rate_id", "bigint", false),
        ],
        pk: Some("id"),
        rows: 6_904_118,
        bytes: (GB + 920 * MB, 3 * GB + 210 * MB),
    },
    Rel {
        schema: "public",
        name: "invoices",
        kind: Kind::Table,
        columns: &[
            ("id", "bigint", true),
            ("number", "text", true),
            ("customer_id", "bigint", true),
            ("subscription_id", "bigint", false),
            ("status", "text", true),
            ("currency", CURRENCY, true),
            ("subtotal", MONEY, true),
            ("tax", MONEY, true),
            ("total", MONEY, true),
            ("issued_at", TS, false),
            ("due_at", TS, false),
            ("paid_at", TS, false),
        ],
        pk: Some("id"),
        rows: 1_812_440,
        bytes: (642 * MB, GB + 120 * MB),
    },
    Rel {
        schema: "public",
        name: "payments",
        kind: Kind::Table,
        columns: &[
            ("id", "bigint", true),
            ("invoice_id", "bigint", true),
            ("amount", MONEY, true),
            ("currency", CURRENCY, true),
            ("method", "text", true),
            ("status", "text", true),
            ("provider_ref", "text", true),
            ("created_at", TS, true),
        ],
        pk: Some("id"),
        rows: 1_731_002,
        bytes: (410 * MB, 690 * MB),
    },
    Rel {
        schema: "public",
        name: "plan_prices",
        kind: Kind::Table,
        columns: &[
            ("id", "bigint", true),
            ("plan_id", "bigint", true),
            ("currency", CURRENCY, true),
            ("unit_amount", MONEY, true),
            ("valid_from", TS, true),
            ("valid_to", TS, false),
        ],
        pk: Some("id"),
        rows: 61,
        bytes: (16 * KB, 48 * KB),
    },
    Rel {
        schema: "public",
        name: "plans",
        kind: Kind::Table,
        columns: &[
            ("id", "bigint", true),
            ("code", "text", true),
            ("name", "text", true),
            ("billing_interval", "text", true),
            ("active", "boolean", true),
            ("created_at", TS, true),
        ],
        pk: Some("id"),
        rows: 14,
        bytes: (8 * KB, 32 * KB),
    },
    Rel {
        schema: "public",
        name: "refunds",
        kind: Kind::Table,
        columns: &[
            ("id", "bigint", true),
            ("payment_id", "bigint", true),
            ("amount", MONEY, true),
            ("reason", "text", true),
            ("created_at", TS, true),
        ],
        pk: Some("id"),
        rows: 12_551,
        bytes: (MB + 100 * KB, MB + 700 * KB),
    },
    Rel {
        schema: "public",
        name: "schema_version",
        kind: Kind::Table,
        columns: &[("id", "integer", true), ("applied_at", TS, true)],
        pk: Some("id"),
        rows: 318,
        bytes: (16 * KB, 40 * KB),
    },
    Rel {
        schema: "public",
        name: "subscription_items",
        kind: Kind::Table,
        columns: &[
            ("id", "bigint", true),
            ("subscription_id", "bigint", true),
            ("plan_price_id", "bigint", true),
            ("quantity", "integer", true),
            ("created_at", TS, true),
        ],
        pk: Some("id"),
        rows: 9_877,
        bytes: (720 * KB, MB + 300 * KB),
    },
    Rel {
        schema: "public",
        name: "subscriptions",
        kind: Kind::Table,
        columns: &[
            ("id", "bigint", true),
            ("customer_id", "bigint", true),
            ("plan_id", "bigint", true),
            ("coupon_id", "bigint", false),
            ("status", "text", true),
            ("current_period_start", TS, true),
            ("current_period_end", TS, true),
            ("cancel_at", TS, false),
            ("created_at", TS, true),
        ],
        pk: Some("id"),
        rows: 4_206,
        bytes: (520 * KB, MB + 100 * KB),
    },
    Rel {
        schema: "public",
        name: "tax_rates",
        kind: Kind::Table,
        columns: &[
            ("id", "bigint", true),
            ("country", "character(2)", true),
            ("name", "text", true),
            ("percentage", "numeric(5,2)", true),
            ("inclusive", "boolean", true),
        ],
        pk: Some("id"),
        rows: 38,
        bytes: (8 * KB, 24 * KB),
    },
    Rel {
        schema: "public",
        name: "usage_events",
        kind: Kind::Partitioned("RANGE (occurred_at)"),
        columns: USAGE_EVENTS,
        pk: Some("id, occurred_at"),
        rows: 113_940_211,
        bytes: (0, 0),
    },
    Rel {
        schema: "public",
        name: "usage_events_2026_08",
        kind: Kind::Partition {
            parent: "usage_events",
            bound: "FOR VALUES FROM ('2026-08-01 00:00:00+00') TO ('2026-09-01 00:00:00+00')",
        },
        columns: USAGE_EVENTS,
        pk: Some("id, occurred_at"),
        rows: 41_210_554,
        bytes: (6 * GB + 120 * MB, 8 * GB + 930 * MB),
    },
    Rel {
        schema: "public",
        name: "usage_events_2026_09",
        kind: Kind::Partition {
            parent: "usage_events",
            bound: "FOR VALUES FROM ('2026-09-01 00:00:00+00') TO ('2026-10-01 00:00:00+00')",
        },
        columns: USAGE_EVENTS,
        pk: Some("id, occurred_at"),
        rows: 43_617_020,
        bytes: (6 * GB + 480 * MB, 9 * GB + 410 * MB),
    },
    Rel {
        schema: "public",
        name: "usage_events_2026_10",
        kind: Kind::Partition {
            parent: "usage_events",
            bound: "FOR VALUES FROM ('2026-10-01 00:00:00+00') TO ('2026-11-01 00:00:00+00')",
        },
        columns: USAGE_EVENTS,
        pk: Some("id, occurred_at"),
        rows: 29_112_637,
        bytes: (4 * GB + 330 * MB, 6 * GB + 270 * MB),
    },
    Rel {
        schema: "public",
        name: "users",
        kind: Kind::Table,
        columns: &[
            ("id", "bigint", true),
            ("customer_id", "bigint", true),
            ("email", "citext", true),
            ("full_name", "text", true),
            ("role", "text", true),
            ("last_login_at", TS, false),
            ("created_at", TS, true),
        ],
        pk: Some("id"),
        rows: 18_930,
        bytes: (2 * MB + 600 * KB, 4 * MB + 800 * KB),
    },
    Rel {
        schema: "public",
        name: "webhook_deliveries",
        kind: Kind::Table,
        columns: &[
            ("id", "bigint", true),
            ("webhook_id", "bigint", true),
            ("event", "text", true),
            ("attempt", "integer", true),
            ("status_code", "integer", false),
            ("payload", "jsonb", true),
            ("created_at", TS, true),
            ("delivered_at", TS, false),
        ],
        pk: Some("id"),
        rows: 22_418_903,
        bytes: (7 * GB + 210 * MB, 9 * GB + 840 * MB),
    },
    Rel {
        schema: "public",
        name: "webhooks",
        kind: Kind::Table,
        columns: &[
            ("id", "bigint", true),
            ("customer_id", "bigint", true),
            ("url", "text", true),
            ("events", "text[]", true),
            ("secret_hint", "text", true),
            ("active", "boolean", true),
        ],
        pk: Some("id"),
        rows: 2_904,
        bytes: (440 * KB, 720 * KB),
    },
    Rel {
        schema: "reporting",
        name: "mrr_by_month",
        kind: Kind::View {
            definition:
                " SELECT date_trunc('month'::text, s.current_period_start)::date AS month,\n    \
                 c.currency,\n    \
                 sum(pp.unit_amount * si.quantity::numeric) AS mrr,\n    \
                 count(DISTINCT s.customer_id) AS customers\n   \
                 FROM subscriptions s\n     \
                 JOIN customers c ON c.id = s.customer_id\n     \
                 JOIN subscription_items si ON si.subscription_id = s.id\n     \
                 JOIN plan_prices pp ON pp.id = si.plan_price_id\n  \
                 WHERE s.status = ANY (ARRAY['active'::text, 'past_due'::text])\n  \
                 GROUP BY 1, 2",
            materialized: false,
        },
        columns: &[
            ("month", "date", false),
            ("currency", CURRENCY, false),
            ("mrr", "numeric", false),
            ("customers", "bigint", false),
        ],
        pk: None,
        rows: 0,
        bytes: (0, 0),
    },
    Rel {
        schema: "reporting",
        name: "overdue_invoices",
        kind: Kind::View {
            definition: " SELECT i.number,\n    \
                 c.name AS customer,\n    \
                 i.currency,\n    \
                 i.total,\n    \
                 i.due_at,\n    \
                 now() - i.due_at AS overdue_by\n   \
                 FROM invoices i\n     \
                 JOIN customers c ON c.id = i.customer_id\n  \
                 WHERE i.status = 'open'::text AND i.due_at < now()",
            materialized: false,
        },
        columns: &[
            ("number", "text", false),
            ("customer", "text", false),
            ("currency", CURRENCY, false),
            ("total", MONEY, false),
            ("due_at", TS, false),
            ("overdue_by", "interval", false),
        ],
        pk: None,
        rows: 0,
        bytes: (0, 0),
    },
    Rel {
        schema: "reporting",
        name: "revenue_daily",
        kind: Kind::View {
            definition: " SELECT date_trunc('day'::text, p.created_at)::date AS day,\n    \
                 p.currency,\n    \
                 sum(p.amount) AS gross,\n    \
                 COALESCE(sum(r.amount), 0::numeric) AS refunded,\n    \
                 sum(p.amount) - COALESCE(sum(r.amount), 0::numeric) AS net\n   \
                 FROM payments p\n     \
                 LEFT JOIN refunds r ON r.payment_id = p.id\n  \
                 WHERE p.status = 'succeeded'::text\n  \
                 GROUP BY 1, 2",
            materialized: true,
        },
        columns: &[
            ("day", "date", false),
            ("currency", CURRENCY, false),
            ("gross", "numeric", false),
            ("refunded", "numeric", false),
            ("net", "numeric", false),
        ],
        pk: None,
        rows: 2_190,
        bytes: (184 * KB, 248 * KB),
    },
];

/// `(child table, child column, parent table)`: every key references
/// the parent's `id`, all in `public`.
pub(super) const FOREIGN_KEYS: &[(&str, &str, &str)] = &[
    ("accounts", "customer_id", "customers"),
    ("audit_log", "actor_user_id", "users"),
    ("credit_notes", "invoice_id", "invoices"),
    ("customers", "tax_rate_id", "tax_rates"),
    ("invoice_lines", "invoice_id", "invoices"),
    ("invoice_lines", "tax_rate_id", "tax_rates"),
    ("invoices", "customer_id", "customers"),
    ("invoices", "subscription_id", "subscriptions"),
    ("payments", "invoice_id", "invoices"),
    ("plan_prices", "plan_id", "plans"),
    ("refunds", "payment_id", "payments"),
    ("subscription_items", "plan_price_id", "plan_prices"),
    ("subscription_items", "subscription_id", "subscriptions"),
    ("subscriptions", "coupon_id", "coupons"),
    ("subscriptions", "customer_id", "customers"),
    ("subscriptions", "plan_id", "plans"),
    ("usage_events", "subscription_item_id", "subscription_items"),
    ("users", "customer_id", "customers"),
    ("webhook_deliveries", "webhook_id", "webhooks"),
    ("webhooks", "customer_id", "customers"),
];

/// A secondary index: `(table, name, unique, what follows USING btree)`.
/// A unique one is a `UNIQUE` constraint's, so the DDL copy writes the
/// constraint rather than the index. Some foreign keys are deliberately
/// left without one (`refunds.payment_id`, `credit_notes.invoice_id`,
/// `subscription_items.plan_price_id`), as they so often are.
pub(super) const INDEXES: &[(&str, &str, bool, &str)] = &[
    (
        "audit_log",
        "audit_log_entity_idx",
        false,
        "(entity, entity_id)",
    ),
    ("coupons", "coupons_code_key", true, "(code)"),
    ("customers", "customers_public_id_key", true, "(public_id)"),
    (
        "invoice_lines",
        "invoice_lines_invoice_id_idx",
        false,
        "(invoice_id)",
    ),
    (
        "invoices",
        "invoices_customer_id_issued_at_idx",
        false,
        "(customer_id, issued_at DESC)",
    ),
    ("invoices", "invoices_number_key", true, "(number)"),
    (
        "invoices",
        "invoices_open_due_at_idx",
        false,
        "(due_at) WHERE (status = 'open'::text)",
    ),
    ("payments", "payments_invoice_id_idx", false, "(invoice_id)"),
    (
        "payments",
        "payments_provider_ref_key",
        true,
        "(provider_ref)",
    ),
    ("plan_prices", "plan_prices_plan_id_idx", false, "(plan_id)"),
    ("plans", "plans_code_key", true, "(code)"),
    (
        "subscriptions",
        "subscriptions_customer_id_idx",
        false,
        "(customer_id)",
    ),
    (
        "subscriptions",
        "subscriptions_renewal_idx",
        false,
        "(current_period_end) WHERE (status = ANY (ARRAY['active'::text, 'trialing'::text]))",
    ),
    (
        "usage_events",
        "usage_events_subscription_item_id_occurred_at_idx",
        false,
        "(subscription_item_id, occurred_at)",
    ),
    ("users", "users_customer_id_idx", false, "(customer_id)"),
    ("users", "users_email_key", true, "(email)"),
    (
        "webhook_deliveries",
        "webhook_deliveries_undelivered_idx",
        false,
        "(created_at) WHERE (delivered_at IS NULL)",
    ),
    (
        "webhook_deliveries",
        "webhook_deliveries_webhook_id_created_at_idx",
        false,
        "(webhook_id, created_at DESC)",
    ),
];

/// `(table, constraint name, pg_get_constraintdef)`.
pub(super) const CHECKS: &[(&str, &str, &str)] = &[
    (
        "coupons",
        "coupons_discount_check",
        "CHECK (percent_off IS NOT NULL OR amount_off IS NOT NULL)",
    ),
    (
        "invoices",
        "invoices_total_check",
        "CHECK (total >= 0::numeric)",
    ),
    (
        "subscription_items",
        "subscription_items_quantity_check",
        "CHECK (quantity > 0)",
    ),
];

/// The relation called `name` in `schema`.
pub(super) fn rel(schema: &str, name: &str) -> Option<&'static Rel> {
    RELATIONS
        .iter()
        .find(|r| r.schema == schema && r.name == name)
}

/// The partitions of `parent` (in `public`).
pub(super) fn partitions_of(parent: &str) -> impl Iterator<Item = &'static Rel> + '_ {
    RELATIONS
        .iter()
        .filter(move |r| matches!(r.kind, Kind::Partition { parent: p, .. } if p == parent))
}

/// A partition's copy of its parent's index `name`, named the way
/// Postgres names the ones it creates: the partition's name in place of
/// the parent's.
fn partition_index_name(parent: &str, partition: &str, name: &str) -> String {
    match name.strip_prefix(parent) {
        Some(rest) => format!("{partition}{rest}"),
        None => format!("{partition}_{name}"),
    }
}

/// Every index on `rel` as the schema browser lists it: the primary
/// key's, then its own, then — for a partition — its copies of the
/// parent's. Sorted by name, as the catalog query sorts them.
pub(super) fn indexes_of(rel: &Rel) -> Vec<IndexMeta> {
    if matches!(rel.kind, Kind::View { .. }) {
        return Vec::new();
    }
    let def = |name: &str, unique: bool, tail: &str| IndexMeta {
        name: name.to_string(),
        definition: format!(
            "CREATE {}INDEX {name} ON {}.{} USING btree {tail}",
            if unique { "UNIQUE " } else { "" },
            rel.schema,
            rel.name
        ),
        unique,
        primary: false,
    };
    let mut out = Vec::new();
    if let Some(pk) = rel.pk {
        out.push(IndexMeta {
            primary: true,
            ..def(&format!("{}_pkey", rel.name), true, &format!("({pk})"))
        });
    }
    let own_table = match rel.kind {
        Kind::Partition { parent, .. } => parent,
        _ => rel.name,
    };
    for (table, name, unique, tail) in INDEXES {
        if *table != own_table {
            continue;
        }
        let name = if own_table == rel.name {
            name.to_string()
        } else {
            partition_index_name(own_table, rel.name, name)
        };
        out.push(def(&name, *unique, tail));
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The demo's [`SchemaCache`], as the live catalog fetch would build it
/// for [`RELATIONS`].
pub(super) fn schema_cache() -> SchemaCache {
    let key = |r: &Rel| (r.schema.to_string(), r.name.to_string());
    let columns_meta_by_table: HashMap<(String, String), Vec<ColumnMeta>> = RELATIONS
        .iter()
        .map(|r| {
            let cols = r
                .columns
                .iter()
                .map(|(name, ty, not_null)| ColumnMeta {
                    name: name.to_string(),
                    type_name: ty.to_string(),
                    not_null: *not_null,
                })
                .collect();
            (key(r), cols)
        })
        .collect();
    let columns_by_table = columns_meta_by_table
        .iter()
        .map(|(k, v)| (k.clone(), v.iter().map(|c| c.name.clone()).collect()))
        .collect();
    let mut tables: Vec<TableMeta> = RELATIONS
        .iter()
        .map(|r| TableMeta {
            schema: r.schema.into(),
            name: r.name.into(),
        })
        .collect();
    tables.sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
    let table_sizes = RELATIONS
        .iter()
        .filter(|r| r.bytes.1 > 0)
        .map(|r| {
            (
                key(r),
                TableSize {
                    table_bytes: r.bytes.0,
                    total_bytes: r.bytes.1,
                },
            )
        })
        .collect();
    let partition_parent = RELATIONS
        .iter()
        .filter_map(|r| match r.kind {
            Kind::Partition { parent, .. } => {
                Some((key(r), (r.schema.to_string(), parent.to_string())))
            }
            _ => None,
        })
        .collect();
    let indexes_by_table: HashMap<(String, String), Vec<IndexMeta>> = RELATIONS
        .iter()
        .map(|r| (key(r), indexes_of(r)))
        .filter(|(_, ixs)| !ixs.is_empty())
        .collect();
    let mut indexes: Vec<TableMeta> = indexes_by_table
        .iter()
        .flat_map(|((schema, _), ixs)| {
            ixs.iter().map(|ix| TableMeta {
                schema: schema.clone(),
                name: ix.name.clone(),
            })
        })
        .collect();
    indexes.sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
    // The unique and primary-key constraints, the kinds `ON CONFLICT ON
    // CONSTRAINT` can name.
    let mut constraints: Vec<ConstraintMeta> = indexes_by_table
        .iter()
        .flat_map(|((schema, table), ixs)| {
            ixs.iter().filter(|ix| ix.unique).map(|ix| ConstraintMeta {
                schema: schema.clone(),
                table: table.clone(),
                name: ix.name.clone(),
            })
        })
        .collect();
    constraints.sort_by(|a, b| (&a.table, &a.name).cmp(&(&b.table, &b.name)));
    let fk_edges = FOREIGN_KEYS
        .iter()
        .map(|(child, column, parent)| FkEdge {
            child_schema: "public".into(),
            child_table: child.to_string(),
            child_column: column.to_string(),
            parent_schema: "public".into(),
            parent_table: parent.to_string(),
            parent_column: "id".into(),
        })
        .collect();
    let sequences = RELATIONS
        .iter()
        .filter(|r| r.kind == Kind::Table && r.pk == Some("id"))
        .map(|r| TableMeta {
            schema: r.schema.into(),
            name: format!("{}_id_seq", r.name),
        })
        .collect();
    SchemaCache {
        schemas: vec!["public".into(), "reporting".into()],
        tables,
        sequences,
        indexes,
        columns_by_table,
        columns_meta_by_table,
        table_sizes,
        fk_edges,
        constraints,
        partition_parent,
        indexes_by_table,
    }
}

/// The schema browser's DDL copy in `--demo`, as the catalog would
/// answer for [`RELATIONS`]: `id` an identity column, the primary key,
/// unique, check and foreign-key constraints, a partition's bound, a
/// view's definition, and the indexes no constraint owns. `None` for a
/// relation the demo doesn't have.
pub fn table_ddl(schema: &str, table: &str) -> Option<crate::query::ddl::TableDdl> {
    use crate::app::quote_ident;
    use crate::query::ddl::{DdlColumn, DdlKind, TableDdl};
    let r = rel(schema, table)?;
    let qualified_name = format!("{}.{}", quote_ident(schema), quote_ident(table));
    if let Kind::View {
        definition,
        materialized,
    } = r.kind
    {
        return Some(TableDdl {
            qualified_name,
            kind: DdlKind::View {
                definition: definition.to_string(),
                materialized,
            },
            columns: Vec::new(),
            constraints: Vec::new(),
            partition_key: None,
            indexes: Vec::new(),
        });
    }
    let columns = r
        .columns
        .iter()
        .map(|(name, ty, not_null)| DdlColumn {
            name: quote_ident(name),
            type_name: ty.to_string(),
            not_null: *not_null,
            identity: (*name == "id" && r.pk.is_some()).then_some('d'),
            default: match *name {
                "created_at" | "updated_at" | "applied_at" | "at" => Some("now()".into()),
                "metadata" => Some("'{}'::jsonb".into()),
                "attempt" => Some("1".into()),
                _ => None,
            },
            ..DdlColumn::default()
        })
        .collect();
    let own = |t: &str| t == table;
    let mut constraints = Vec::new();
    if let Some(pk) = r.pk {
        constraints.push((
            quote_ident(&format!("{table}_pkey")),
            format!("PRIMARY KEY ({pk})"),
        ));
    }
    let base = match r.kind {
        Kind::Partition { parent, .. } => parent,
        _ => table,
    };
    // A partition's foreign keys, checks and unique keys are inherited:
    // the partition statement lists only its primary key, as live.
    if base == table {
        for (t, name, unique, tail) in INDEXES {
            if *unique && own(t) {
                let cols = tail.split(')').next().unwrap_or("").trim_start_matches('(');
                constraints.push((quote_ident(name), format!("UNIQUE ({cols})")));
            }
        }
        for (t, name, def) in CHECKS {
            if own(t) {
                constraints.push((quote_ident(name), def.to_string()));
            }
        }
        for (child, column, parent) in FOREIGN_KEYS {
            if own(child) {
                constraints.push((
                    quote_ident(&format!("{table}_{column}_fkey")),
                    format!(
                        "FOREIGN KEY ({}) REFERENCES {}(id)",
                        quote_ident(column),
                        quote_ident(parent)
                    ),
                ));
            }
        }
    }
    let indexes = indexes_of(r)
        .into_iter()
        .filter(|ix| !ix.unique)
        .map(|ix| ix.definition)
        .collect();
    let (kind, partition_key) = match r.kind {
        Kind::Partitioned(key) => (DdlKind::Table, Some(key.to_string())),
        Kind::Partition { parent, bound } => (
            DdlKind::Partition {
                parent: format!("public.{}", quote_ident(parent)),
                bound: bound.to_string(),
            },
            None,
        ),
        _ => (DdlKind::Table, None),
    };
    Some(TableDdl {
        qualified_name,
        kind,
        columns,
        constraints,
        partition_key,
        indexes,
    })
}

/// `\df`'s rows in `--demo`: `[schema, name, result, arguments, type]`.
pub(super) const FUNCTIONS: &[[&str; 5]] = &[
    ["pg_catalog", "now", "timestamp with time zone", "", "func"],
    [
        "public",
        "close_billing_period",
        "",
        "p_period date",
        "proc",
    ],
    [
        "public",
        "invoice_balance",
        "numeric",
        "p_invoice_id bigint",
        "func",
    ],
    [
        "public",
        "next_invoice_number",
        "text",
        "p_year integer",
        "func",
    ],
    ["public", "set_updated_at", "trigger", "", "func"],
    [
        "reporting",
        "mrr_for_month",
        "numeric",
        "p_month date, p_currency character",
        "func",
    ],
    ["reporting", "refresh_revenue_daily", "", "", "proc"],
];

// ---------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------

/// Fictional companies: `(name, domain, country, currency)`.
const COMPANIES: &[(&str, &str, &str, &str)] = &[
    ("Northwind Analytics", "northwind.example", "GB", "GBP"),
    ("Bluefin Logistics", "bluefin.example", "DE", "EUR"),
    ("Halcyon Health", "halcyonhealth.example", "US", "USD"),
    ("Brightline Media", "brightline.example", "GB", "GBP"),
    ("Copperleaf Foods", "copperleaf.example", "NL", "EUR"),
    ("Juniper Robotics", "juniper-robotics.example", "US", "USD"),
    ("Meridian Legal", "meridianlegal.example", "GB", "GBP"),
    ("Orchard Labs", "orchardlabs.example", "IE", "EUR"),
    ("Pinecrest Energy", "pinecrest.example", "US", "USD"),
    ("Quayside Retail", "quayside.example", "FR", "EUR"),
    ("Redwood Insurance", "redwood-ins.example", "GB", "GBP"),
    ("Sablewood Games", "sablewood.example", "DE", "EUR"),
];

/// Fictional people: `(first, last)`.
const PEOPLE: &[(&str, &str)] = &[
    ("Amara", "Okafor"),
    ("Tomasz", "Nowak"),
    ("Priya", "Raman"),
    ("Lucas", "Moreau"),
    ("Hannah", "Becker"),
    ("Kenji", "Watanabe"),
    ("Sofia", "Lindqvist"),
    ("Mateo", "Alvarez"),
    ("Grace", "Adeyemi"),
    ("Oliver", "Byrne"),
    ("Ines", "Carvalho"),
    ("Daniel", "Kowalski"),
];

/// `tax_rates`, in id order from 1: `(country, name, percentage)`.
const TAX_RATES: &[(&str, &str, &str)] = &[
    ("GB", "UK VAT", "20.00"),
    ("DE", "DE USt", "19.00"),
    ("US", "US exempt", "0.00"),
    ("NL", "NL BTW", "21.00"),
    ("FR", "FR TVA", "20.00"),
    ("IE", "IE VAT", "23.00"),
    ("GB", "UK VAT reduced", "5.00"),
    ("DE", "DE USt reduced", "7.00"),
];

/// The tax rate a customer in `country` is billed at: its standard
/// rate's id.
fn tax_rate_id(country: &str) -> usize {
    1 + TAX_RATES
        .iter()
        .position(|(c, ..)| *c == country)
        .unwrap_or(0)
}

/// The first customer's id: the demo's companies are customers
/// 2101..2112.
pub(super) const FIRST_CUSTOMER: usize = 2101;
/// The first invoice's id.
pub(super) const FIRST_INVOICE: usize = 48_201;

/// Where a table's ids start, so ids read like a database that has
/// been in use for a while rather than a fresh one.
fn first_id(table: &str) -> usize {
    match table {
        "customers" | "accounts" => FIRST_CUSTOMER,
        "invoices" => FIRST_INVOICE,
        "invoice_lines" => 190_552,
        "payments" => 39_871,
        "refunds" => 1_204,
        "credit_notes" => 3_310,
        "subscriptions" => 5_512,
        "subscription_items" => 12_090,
        "users" => 7_731,
        "webhooks" => 880,
        "webhook_deliveries" => 22_418_120,
        "audit_log" => 9_812_001,
        "plans" | "tax_rates" | "plan_prices" | "coupons" => 1,
        "flyway_schema_history" => 201,
        "schema_version" => 309,
        t if t.starts_with("usage_events") => 113_939_400,
        _ => 1,
    }
}

/// A deterministic 64-bit mix of `parts`, for values that should look
/// random but never change (FNV-1a).
fn mix(parts: &[&str], n: usize) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts
        .iter()
        .map(|p| p.as_bytes())
        .chain([&n.to_le_bytes()[..]])
    {
        for b in p {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// A version-4-shaped UUID that is a pure function of its inputs.
fn uuid(parts: &[&str], n: usize) -> String {
    let a = mix(parts, n);
    let b = mix(parts, n + 7_919);
    format!(
        "{:08x}-{:04x}-4{:03x}-{:x}{:03x}-{:012x}",
        a >> 32,
        (a >> 16) & 0xffff,
        a & 0xfff,
        8 + (b >> 62),
        (b >> 48) & 0xfff,
        b & 0xffff_ffff_ffff
    )
}

/// A timestamp in the weeks before the demo's "today" (2026-10-21),
/// spread over the day the way a busy database's are.
fn timestamp(i: usize, days_back: usize) -> String {
    let day = 21usize.saturating_sub((days_back + i * 3) % 20).max(1);
    format!(
        "2026-10-{day:02} {:02}:{:02}:{:02}+00",
        7 + (i * 5) % 12,
        (i * 17 + 4) % 60,
        (i * 29 + 11) % 60
    )
}

fn pick<T: Copy>(pool: &[T], i: usize) -> T {
    pool[i % pool.len()]
}

/// One generated cell for `column` (of type `ty`) at 0-indexed `i` in
/// `table`. Values follow the column's meaning first — statuses that
/// exist, money with two decimals, foreign keys that point at real
/// parents — and its type second. An empty string is NULL.
pub(super) fn synth_cell(table: &str, column: &str, ty: &str, i: usize) -> String {
    let base = table.split("_2026_").next().unwrap_or(table);
    let id = first_id(table) + i;
    let company = pick(COMPANIES, i);
    let person = pick(PEOPLE, i * 5 + 3);
    let cell = |s: &str| s.to_string();
    if column == "id" || (column == "installed_rank" && base == "flyway_schema_history") {
        return id.to_string();
    }
    if let Some((_, _, parent)) = FOREIGN_KEYS
        .iter()
        .find(|(c, col, _)| *c == base && *col == column)
    {
        // Lines come three to an invoice; everything else spreads over
        // the first few of its parents.
        return match (base, *parent) {
            ("invoice_lines", "invoices") => (first_id(parent) + i / 3).to_string(),
            ("audit_log", _) if i % 4 == 3 => String::new(),
            ("subscriptions", "coupons") if i % 3 != 1 => String::new(),
            ("customers", "tax_rates") => tax_rate_id(company.2).to_string(),
            ("invoice_lines", "tax_rates") => tax_rate_id(pick(COMPANIES, i / 3).2).to_string(),
            _ => (first_id(parent) + (i * 7) % 12).to_string(),
        };
    }
    match (base, column) {
        ("customers", "name") => cell(company.0),
        ("customers", "billing_email") => format!("billing@{}", company.1),
        ("customers", "country") => cell(company.2),
        ("tax_rates", "country") => cell(pick(TAX_RATES, i).0),
        ("tax_rates", "name") => cell(pick(TAX_RATES, i).1),
        ("tax_rates", "percentage") => cell(pick(TAX_RATES, i).2),
        ("tax_rates", "inclusive") => cell("f"),
        ("customers", "status") => cell(pick(
            &[
                "active", "active", "past_due", "active", "active", "churned",
            ],
            i,
        )),
        ("customers", "metadata") => format!(
            "{{\"segment\": \"{}\", \"crm_id\": \"CRM-{:05}\"}}",
            pick(&["mid-market", "enterprise", "smb", "mid-market"], i),
            40_211 + i * 37
        ),
        (_, "currency") => cell(company.3),
        ("users", "email") => format!(
            "{}.{}@{}",
            person.0.to_lowercase(),
            person.1.to_lowercase(),
            company.1
        ),
        ("users", "full_name") => format!("{} {}", person.0, person.1),
        ("users", "role") => cell(pick(&["owner", "admin", "billing", "member", "member"], i)),
        ("users", "last_login_at") if i % 5 == 4 => String::new(),
        ("plans", "code") => cell(pick(&["starter", "growth", "scale", "enterprise"], i)),
        ("plans", "name") => cell(pick(&["Starter", "Growth", "Scale", "Enterprise"], i)),
        ("plans", "billing_interval") => cell(pick(&["month", "month", "month", "year"], i)),
        ("plan_prices", "unit_amount") => cell(pick(&["49.00", "249.00", "1290.00", "4800.00"], i)),
        ("plan_prices", "valid_to") => String::new(),
        ("subscriptions", "status") => cell(pick(
            &[
                "active", "active", "trialing", "past_due", "active", "canceled",
            ],
            i,
        )),
        ("subscriptions", "cancel_at") if i % 6 != 5 => String::new(),
        ("subscription_items", "quantity") => pick(&[1, 5, 1, 12, 1, 3], i).to_string(),
        ("invoice_lines", "quantity") => pick(&[1, 5, 1, 1, 1, 1], i).to_string(),
        ("invoices", "number") => format!("INV-{:06}", 4_821 + i),
        ("invoices", "status") => cell(pick(&["paid", "open", "paid", "void", "paid"], i)),
        ("invoices", "paid_at") if matches!(i % 5, 1 | 3) => String::new(),
        ("invoice_lines", "description") => cell(pick(
            &[
                "Growth plan · Oct 2026",
                "Additional seats × 5",
                "API calls overage (1.2M)",
                "Scale plan · Oct 2026",
                "Priority support",
                "Storage overage (40 GB)",
            ],
            i,
        )),
        ("invoice_lines", "unit_amount") => cell(pick(
            &["1290.00", "29.00", "480.00", "4800.00", "250.00", "10.00"],
            i,
        )),
        ("invoice_lines", "amount") => cell(pick(
            &["1290.00", "145.00", "480.00", "4800.00", "250.00", "10.00"],
            i,
        )),
        ("payments", "method") => cell(pick(&["card", "sepa_debit", "card", "bank_transfer"], i)),
        ("payments", "status") => cell(pick(
            &["succeeded", "succeeded", "failed", "succeeded", "pending"],
            i,
        )),
        ("payments", "provider_ref") => format!("psp_{:016x}", mix(&[table, column], i)),
        ("refunds", "reason") => cell(pick(
            &["duplicate", "requested_by_customer", "service_outage"],
            i,
        )),
        ("credit_notes", "number") => format!("CN-2026-{:05}", 3_310 + i),
        ("credit_notes", "reason") => cell(pick(&["proration", "goodwill", "pricing_error"], i)),
        ("coupons", "code") => cell(pick(
            &["LAUNCH20", "PARTNER15", "NONPROFIT50", "Q4ANNUAL"],
            i,
        )),
        ("coupons", "percent_off") => cell(pick(&["20.00", "15.00", "50.00", ""], i)),
        ("coupons", "amount_off") => cell(pick(&["", "", "", "500.00"], i)),
        ("coupons", "redemptions") => pick(&[312, 41, 18, 7], i).to_string(),
        ("usage_events", "metric") => cell(pick(
            &["api_calls", "seats", "storage_gb", "emails_sent"],
            i,
        )),
        ("usage_events", "quantity") => pick(&[1200, 5, 40, 830, 2400, 12], i).to_string(),
        ("usage_events", "idempotency_key") => uuid(&[table], i),
        ("webhooks", "url") => format!("https://hooks.{}/billing", company.1),
        ("webhooks", "events") => cell(pick(
            &[
                "{invoice.paid,invoice.payment_failed}",
                "{*}",
                "{subscription.updated}",
            ],
            i,
        )),
        ("webhooks", "secret_hint") => format!("whsec_…{:04x}", mix(&[table], i) & 0xffff),
        ("webhook_deliveries", "event") => cell(pick(
            &[
                "invoice.paid",
                "invoice.payment_failed",
                "subscription.updated",
                "invoice.paid",
            ],
            i,
        )),
        ("webhook_deliveries", "attempt") => pick(&[1, 1, 3, 1, 2], i).to_string(),
        ("webhook_deliveries", "status_code") => cell(pick(&["200", "200", "503", "200", ""], i)),
        ("webhook_deliveries", "delivered_at") if i % 5 == 2 || i % 5 == 4 => String::new(),
        ("webhook_deliveries", "payload") => format!(
            "{{\"type\": \"invoice.paid\", \"invoice\": \"INV-{:06}\"}}",
            4_821 + i
        ),
        ("audit_log", "action") => cell(pick(
            &[
                "invoice.finalized",
                "subscription.updated",
                "payment.refunded",
                "user.invited",
            ],
            i,
        )),
        ("audit_log", "entity") => cell(pick(&["invoice", "subscription", "payment", "user"], i)),
        ("audit_log", "entity_id") => (FIRST_INVOICE + i * 3).to_string(),
        ("audit_log", "changes") => cell(pick(
            &[
                "{\"status\": [\"draft\", \"open\"]}",
                "{\"plan_id\": [2, 3]}",
                "{\"amount\": \"49.00\"}",
                "{\"role\": \"billing\"}",
            ],
            i,
        )),
        ("flyway_schema_history", "version") => format!("1.{}", 1 + i),
        ("flyway_schema_history", "description") => cell(pick(
            &[
                "add invoice indexes",
                "usage events partitions",
                "webhook retries",
            ],
            i,
        )),
        ("flyway_schema_history", "type") => cell("SQL"),
        ("flyway_schema_history", "script") => format!("V1_{}__migration.sql", 1 + i),
        ("flyway_schema_history", "installed_by") => cell("ledger_owner"),
        ("flyway_schema_history", "installed_on") => {
            format!("2023-0{}-1{} 10:00:00", 1 + i % 9, i % 10)
        }
        ("mrr_by_month", "month") => format!("2026-{:02}-01", 10 - (i % 10)),
        ("mrr_by_month", "mrr") => format!("{:.2}", 1_204_310.0 - i as f64 * 18_640.5),
        ("mrr_by_month", "customers") => (3_120 - i * 21).to_string(),
        ("revenue_daily", "day") => format!("2026-10-{:02}", 20 - (i % 19)),
        ("revenue_daily", "gross") => format!("{:.2}", 41_208.4 + (i * 1_877 % 9_000) as f64),
        ("revenue_daily", "refunded") => cell(pick(&["0.00", "249.00", "0.00", "1290.00"], i)),
        ("revenue_daily", "net") => format!("{:.2}", 41_208.4 + (i * 1_877 % 9_000) as f64 - 120.0),
        ("overdue_invoices", "customer") => cell(company.0),
        ("overdue_invoices", "number") => format!("INV-{:06}", 4_821 + i * 2),
        ("overdue_invoices", "overdue_by") => format!("{} days {:02}:14:09", 3 + i * 4, i % 24),
        (_, "public_id") => uuid(&[table], i),
        _ if ty == "uuid" => uuid(&[table, column], i),
        _ if ty.starts_with("numeric") => {
            cell(pick(&["1548.00", "249.00", "5712.00", "117.81"], i))
        }
        _ if ty == "boolean" => cell(if i % 5 == 3 { "f" } else { "t" }),
        _ if ty == "jsonb" => cell("{}"),
        _ if ty == "date" => format!("2026-10-{:02}", 1 + i),
        _ if ty.starts_with("timestamp") => {
            let back = match column {
                "created_at" | "issued_at" | "valid_from" | "current_period_start" => 12,
                _ => 2,
            };
            timestamp(i, back)
        }
        _ if ty == "bigint" || ty == "integer" || ty == "smallint" => (i + 1).to_string(),
        _ => format!("{column}-{}", i + 1),
    }
}

/// `invoices`, written out: the demo's opening query reads it, so every
/// row is chosen — five open (four overdue on the demo's 2026-10-21),
/// paid, a void, a draft and one written off.
pub(super) fn invoices() -> (Vec<String>, Vec<Vec<String>>) {
    const ROWS: &[[&str; 12]] = &[
        [
            "48201",
            "INV-004821",
            "2101",
            "5512",
            "paid",
            "GBP",
            "1290.00",
            "258.00",
            "1548.00",
            "2026-09-01 09:00:00+00",
            "2026-09-15 09:00:00+00",
            "2026-09-09 14:22:51+00",
        ],
        [
            "48202",
            "INV-004822",
            "2102",
            "5513",
            "open",
            "EUR",
            "4800.00",
            "912.00",
            "5712.00",
            "2026-09-01 09:00:00+00",
            "2026-10-01 09:00:00+00",
            "",
        ],
        [
            "48203",
            "INV-004823",
            "2103",
            "5519",
            "paid",
            "USD",
            "249.00",
            "0.00",
            "249.00",
            "2026-09-01 09:00:00+00",
            "2026-09-15 09:00:00+00",
            "2026-09-02 08:01:13+00",
        ],
        [
            "48204",
            "INV-004824",
            "2104",
            "5520",
            "open",
            "GBP",
            "640.00",
            "128.00",
            "768.00",
            "2026-09-20 09:00:00+00",
            "2026-10-04 09:00:00+00",
            "",
        ],
        [
            "48205",
            "INV-004825",
            "2105",
            "5524",
            "void",
            "EUR",
            "99.00",
            "20.79",
            "119.79",
            "2026-09-15 09:00:00+00",
            "2026-09-29 09:00:00+00",
            "",
        ],
        [
            "48206",
            "INV-004826",
            "2106",
            "5527",
            "uncollectible",
            "USD",
            "2150.00",
            "0.00",
            "2150.00",
            "2026-08-01 09:00:00+00",
            "2026-08-31 09:00:00+00",
            "",
        ],
        [
            "48207",
            "INV-004827",
            "2107",
            "5531",
            "open",
            "GBP",
            "18450.00",
            "3690.00",
            "22140.00",
            "2026-10-01 09:00:00+00",
            "2026-10-08 09:00:00+00",
            "",
        ],
        [
            "48208",
            "INV-004828",
            "2108",
            "5533",
            "paid",
            "EUR",
            "1290.00",
            "296.70",
            "1586.70",
            "2026-10-01 09:00:00+00",
            "2026-10-15 09:00:00+00",
            "2026-10-03 11:47:09+00",
        ],
        [
            "48209",
            "INV-004829",
            "2109",
            "5536",
            "open",
            "USD",
            "980.00",
            "0.00",
            "980.00",
            "2026-10-01 09:00:00+00",
            "2026-10-11 09:00:00+00",
            "",
        ],
        [
            "48210",
            "INV-004830",
            "2110",
            "5540",
            "draft",
            "EUR",
            "4800.00",
            "960.00",
            "5760.00",
            "",
            "",
            "",
        ],
        [
            "48211",
            "INV-004831",
            "2111",
            "5541",
            "open",
            "GBP",
            "249.00",
            "49.80",
            "298.80",
            "2026-10-14 09:00:00+00",
            "2026-10-28 09:00:00+00",
            "",
        ],
        [
            "48212",
            "INV-004832",
            "2112",
            "5544",
            "paid",
            "EUR",
            "49.00",
            "9.31",
            "58.31",
            "2026-10-15 09:00:00+00",
            "2026-10-29 09:00:00+00",
            "2026-10-15 09:00:41+00",
        ],
    ];
    canned("invoices", ROWS)
}

/// `customers`, from the company list, so the invoices' `customer_id`s
/// point at them.
pub(super) fn customers() -> (Vec<String>, Vec<Vec<String>>) {
    let rel = rel("public", "customers").expect("customers");
    let cols = rel.columns.iter().map(|c| c.0.to_string()).collect();
    let rows = (0..COMPANIES.len())
        .map(|i| {
            rel.columns
                .iter()
                .map(|(name, ty, _)| synth_cell("customers", name, ty, i))
                .collect()
        })
        .collect();
    (cols, rows)
}

fn canned<const N: usize>(table: &str, rows: &[[&str; N]]) -> (Vec<String>, Vec<Vec<String>>) {
    let rel = rel("public", table).expect("a demo table");
    (
        rel.columns.iter().map(|c| c.0.to_string()).collect(),
        rows.iter()
            .map(|r| r.iter().map(|s| s.to_string()).collect())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_foreign_key_and_index_names_a_real_table_and_column() {
        let has = |t: &str, c: &str| {
            rel("public", t).is_some_and(|r| r.columns.iter().any(|(n, ..)| *n == c))
        };
        for (child, column, parent) in FOREIGN_KEYS {
            assert!(has(child, column), "{child}.{column}");
            assert!(has(parent, "id"), "{parent}.id");
        }
        for (table, name, _, tail) in INDEXES {
            assert!(rel("public", table).is_some(), "{name} on {table}");
            assert!(name.starts_with(table), "{name} is named for {table}");
            let first = tail
                .trim_start_matches('(')
                .split([',', ')', ' '])
                .next()
                .unwrap();
            assert!(has(table, first), "{name}'s first column {first}");
        }
        for (table, ..) in CHECKS {
            assert!(rel("public", table).is_some(), "{table}");
        }
    }

    #[test]
    fn the_cache_folds_partitions_under_their_table_and_sizes_them() {
        let c = schema_cache();
        assert_eq!(c.tables.len(), RELATIONS.len());
        assert_eq!(
            c.partition_parent
                .get(&("public".into(), "usage_events_2026_09".into())),
            Some(&("public".into(), "usage_events".into()))
        );
        assert_eq!(c.partition_parent.len(), 3);
        assert!(c.schemas.contains(&"reporting".to_string()));
        let size = &c.table_sizes[&("public".into(), "invoice_lines".into())];
        assert!(size.total_bytes > size.table_bytes && size.table_bytes > GB);
        // Plain views have no storage; the materialized one does.
        assert!(!c
            .table_sizes
            .contains_key(&("reporting".into(), "mrr_by_month".into())));
        assert!(c
            .table_sizes
            .contains_key(&("reporting".into(), "revenue_daily".into())));
    }

    #[test]
    fn a_partition_carries_its_parents_indexes_under_its_own_names() {
        let p = rel("public", "usage_events_2026_10").unwrap();
        let names: Vec<String> = indexes_of(p).into_iter().map(|i| i.name).collect();
        assert_eq!(
            names,
            [
                "usage_events_2026_10_pkey",
                "usage_events_2026_10_subscription_item_id_occurred_at_idx"
            ]
        );
        assert!(indexes_of(rel("reporting", "overdue_invoices").unwrap()).is_empty());
    }

    #[test]
    fn partition_index_name_swaps_the_parents_prefix() {
        assert_eq!(
            partition_index_name("usage_events", "usage_events_2026_08", "usage_events_x_idx"),
            "usage_events_2026_08_x_idx"
        );
        assert_eq!(
            partition_index_name("t", "t_1", "other_idx"),
            "t_1_other_idx"
        );
    }

    #[test]
    fn unique_indexes_are_constraints_in_the_cache_and_the_ddl() {
        let c = schema_cache();
        let named = |n: &str| c.constraints.iter().any(|k| k.name == n);
        assert!(named("invoices_pkey") && named("invoices_number_key"));
        assert!(!named("invoices_open_due_at_idx"));
        let ddl = crate::query::ddl::create_statement(&table_ddl("public", "invoices").unwrap());
        assert!(
            ddl.contains("CONSTRAINT invoices_number_key UNIQUE (number)"),
            "{ddl}"
        );
        assert!(
            ddl.contains("CONSTRAINT invoices_total_check CHECK"),
            "{ddl}"
        );
        assert!(!ddl.contains("CREATE UNIQUE INDEX"), "{ddl}");
        assert!(ddl.contains("WHERE (status = 'open'::text)"), "{ddl}");
    }

    #[test]
    fn generated_values_are_deterministic_and_point_at_real_parents() {
        assert_eq!(
            synth_cell("payments", "provider_ref", "text", 3),
            synth_cell("payments", "provider_ref", "text", 3)
        );
        let u = uuid(&["customers"], 0);
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4", "{u}");
        // Three lines to an invoice, starting at the first.
        let inv = |i| synth_cell("invoice_lines", "invoice_id", "bigint", i);
        assert_eq!(inv(0), FIRST_INVOICE.to_string());
        assert_eq!(inv(2), FIRST_INVOICE.to_string());
        assert_eq!(inv(3), (FIRST_INVOICE + 1).to_string());
        let cust: usize = synth_cell("invoices", "customer_id", "bigint", 5)
            .parse()
            .unwrap();
        assert!((FIRST_CUSTOMER..FIRST_CUSTOMER + COMPANIES.len()).contains(&cust));
        // A partition's rows are its parent's shape.
        assert_eq!(
            synth_cell("usage_events_2026_09", "metric", "text", 0),
            "api_calls"
        );
    }

    #[test]
    fn a_customer_is_taxed_at_its_own_countrys_rate() {
        for i in 0..COMPANIES.len() {
            let country = synth_cell("customers", "country", "character(2)", i);
            let id: usize = synth_cell("customers", "tax_rate_id", "bigint", i)
                .parse()
                .unwrap();
            let rate_country = synth_cell("tax_rates", "country", "character(2)", id - 1);
            assert_eq!(rate_country, country, "customer {i}");
        }
    }

    #[test]
    fn mix_differs_by_every_part() {
        assert_ne!(mix(&["a"], 0), mix(&["a"], 1));
        assert_ne!(mix(&["a", "b"], 0), mix(&["ab"], 0));
    }
}
