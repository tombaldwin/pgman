//! What each column of a masked dump becomes: the built-in default for
//! what its name and type say it holds, a framework's preset, the
//! project's rules file — and a foreign key's referenced column, so
//! joins still join. Decided from the catalog alone, so the same schema
//! masks the same way everywhere, CI included. Pure.
//!
//! Anything that looks personal and has no better rule is NULLed (or,
//! where NULL can't be stored, given the type's empty value): a dump is
//! never refused for want of a rule. See `docs/design/masked-dumps.md`,
//! section 2.

use super::catalog::{Catalog, Column, Table};
use super::rules::{Rows, RuleSet};
use std::collections::HashMap;

/// What a column becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    Keep,
    /// NULL, or the type's empty value where NULL can't be stored.
    Null,
    Fixed(String),
    /// The known test password, hashed as the value was.
    Password(String),
    /// A keyed hash of the value, of the column's type.
    Hash,
    Email,
    Name(NamePart),
    Phone,
    Ip,
    /// A postcode's district: `SW1A 1AA` becomes `SW1A`.
    Postcode,
    /// A date of birth's year, still a date.
    Year,
    /// Every shifted date of a row moved by the same number of days, up
    /// to `days` either way, keyed by the row's primary key.
    DateShift(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamePart {
    Full,
    First,
    Last,
}

impl Rule {
    /// The rule's name, as a rules file writes it.
    pub fn name(&self) -> &'static str {
        match self {
            Rule::Keep => "keep",
            Rule::Null => "null",
            Rule::Fixed(_) => "fixed",
            Rule::Password(_) => "password",
            Rule::Hash => "hash",
            Rule::Email => "email",
            Rule::Name(NamePart::Full) => "name",
            Rule::Name(NamePart::First) => "first_name",
            Rule::Name(NamePart::Last) => "last_name",
            Rule::Phone => "phone",
            Rule::Ip => "ip",
            Rule::Postcode => "postcode",
            Rule::Year => "year",
            Rule::DateShift(_) => "date.shift",
        }
    }

    /// Whether two values in, one value out, can happen: such a rule
    /// can't go on a column that must stay unique.
    pub fn lossy(&self) -> bool {
        matches!(
            self,
            Rule::Fixed(_) | Rule::Password(_) | Rule::Postcode | Rule::Year
        )
    }
}

/// Why a column has its rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// The built-in rules: what the name or type said.
    Default(String),
    /// A framework's table.
    Preset(String),
    /// The project's rules file.
    Rules,
    /// It references a column with a rule, so it takes the same one.
    Follows(String),
    /// Nothing about it looks personal.
    Plain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub column: String,
    pub rule: Rule,
    pub origin: Origin,
    /// The most characters a value may take: the smallest length of
    /// every column joined to this one, so they all come out the same.
    pub fit: Option<usize>,
    pub not_null: bool,
    pub unique: bool,
    /// The column's base type.
    pub base: String,
    /// An enum's first label, for an empty value.
    pub enum_first: Option<String>,
    pub array: bool,
}

/// One table's part of a masked dump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TablePlan {
    pub table: String,
    /// Its rows are left out: a session or token table, or the rules say.
    pub drop_rows: bool,
    /// Why, when they are.
    pub drop_why: Option<String>,
    /// Columns copied, in order (stored generated ones left out).
    pub columns: Vec<Decision>,
    /// The primary key, which keys a row's date shift.
    pub primary_key: Vec<String>,
}

/// A whole masked dump's plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub tables: Vec<TablePlan>,
    pub large_objects: bool,
    /// Why the dump can't go as planned: a rule that can't be stored, a
    /// rule that names nothing. `masking check` fails on these, and so
    /// does the dump.
    pub problems: Vec<String>,
}

impl Plan {
    pub fn table(&self, qualified: &str) -> Option<&TablePlan> {
        self.tables.iter().find(|t| t.table == qualified)
    }
}

/// A column name's words: split on `_`, `-`, between a lower-case letter
/// and a capital, and between letters and digits; lower-cased.
/// `customerEmail2` is `customer email 2`. Pure.
pub fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    for c in name.chars() {
        let split = match prev {
            _ if c == '_' || c == '-' || c == ' ' || c == '.' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                prev = None;
                continue;
            }
            Some(p) => {
                (p.is_lowercase() && c.is_uppercase())
                    || (p.is_alphabetic() && c.is_ascii_digit())
                    || (p.is_ascii_digit() && c.is_alphabetic())
            }
            None => false,
        };
        if split && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.extend(c.to_lowercase());
        prev = Some(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Whether `w` holds `phrase` (one word, or several in a row). Pure.
fn has(w: &[String], phrase: &str) -> bool {
    let p: Vec<&str> = phrase.split(' ').collect();
    w.windows(p.len())
        .any(|win| win.iter().zip(&p).all(|(a, b)| a == b))
}

fn any(w: &[String], phrases: &[&str]) -> bool {
    phrases.iter().any(|p| has(w, p))
}

/// Secrets: dropped whatever else the name says.
const SECRET: &[&str] = &[
    "token",
    "secret",
    "session",
    "cookie",
    "api key",
    "apikey",
    "mfa",
    "otp",
    "totp",
    "credential",
    "credentials",
    "salt",
    "nonce",
    "private key",
    "pin",
    "verification code",
    "reset code",
    "activation code",
    "auth code",
    "authorisation code",
    "authorization code",
    "recovery code",
    "one time",
];
const PASSWORD: &[&str] = &["password", "passwd", "pwd", "passphrase"];
/// Special-category and criminal-offence data (UK GDPR, Articles 9 and 10).
const SPECIAL: &[&str] = &[
    "gender",
    "sex",
    "sexuality",
    "sexual",
    "ethnicity",
    "ethnic",
    "race",
    "religion",
    "religious",
    "belief",
    "health",
    "medical",
    "diagnosis",
    "disability",
    "disabled",
    "union",
    "political",
    "politics",
    "biometric",
    "genetic",
    "criminal",
    "conviction",
    "convictions",
    "offence",
    "offense",
    "vulnerable",
    "pregnancy",
    "pregnant",
];
/// A last word that says the column is about the thing, not the thing:
/// `email_verified`, `phone_type`.
const FLAG_LAST: &[&str] = &[
    "type",
    "kind",
    "count",
    "verified",
    "enabled",
    "flag",
    "format",
    "method",
    "source",
    "consent",
    "opt",
    "optin",
    "valid",
    "confirmed",
];
/// A last word that is usually a code, not the thing — `currency_code`,
/// `order_status` — but can be the thing itself: `postal_code`,
/// `sort_code`, `marital_status`, `tax_id`.
const VALUE_LAST: &[&str] = &["code", "status", "state", "id"];
const EMAIL: &[&str] = &["email", "mail", "login"];
const PHONE: &[&str] = &[
    "phone",
    "mobile",
    "tel",
    "telephone",
    "fax",
    "msisdn",
    "cell",
];
const FIRST: &[&str] = &[
    "first name",
    "firstname",
    "given name",
    "forename",
    "forenames",
];
const LAST: &[&str] = &[
    "last name",
    "lastname",
    "surname",
    "family name",
    "maiden name",
];
const FULL: &[&str] = &[
    "full name",
    "fullname",
    "display name",
    "middle name",
    "nickname",
    "nick name",
    "user name",
    "username",
    "contact name",
    "customer name",
    "person name",
    "known as",
];
/// Tables whose bare `name` or `title` is a person's.
const PEOPLE: &[&str] = &[
    "user",
    "users",
    "customer",
    "customers",
    "person",
    "persons",
    "people",
    "member",
    "members",
    "employee",
    "employees",
    "staff",
    "contact",
    "contacts",
    "patient",
    "patients",
    "student",
    "students",
    "client",
    "clients",
    "account",
    "accounts",
    "applicant",
    "applicants",
    "candidate",
    "candidates",
    "driver",
    "drivers",
];
const IDENTIFIER: &[&str] = &[
    "ssn",
    "nino",
    "national insurance",
    "ni number",
    "nhs",
    "passport",
    "licence",
    "license",
    "iban",
    "bic",
    "sort code",
    "sortcode",
    "national id",
    "account number",
    "card number",
    "pan",
    "cvv",
    "salary",
    "income",
    "pay",
    "wage",
    "wages",
    "tax",
    "utr",
    "vat number",
];
const ADDRESS: &[&str] = &[
    "address", "street", "city", "town", "county", "line1", "line2", "line3", "house", "flat",
    "building", "locality",
];
const POSTCODE: &[&str] = &[
    "postcode",
    "post code",
    "postal code",
    "postalcode",
    "zip",
    "zipcode",
];
const BIRTH: &[&str] = &["dob", "birth", "birthday", "birthdate"];
const AGE: &[&str] = &["age"];
const NETWORK: &[&str] = &[
    "ip",
    "ipv4",
    "ipv6",
    "ip address",
    "ipaddress",
    "remote addr",
];
const DEVICE: &[&str] = &[
    "mac address",
    "user agent",
    "useragent",
    "device",
    "imei",
    "fingerprint",
];
const LOCATION: &[&str] = &[
    "lat",
    "lng",
    "lon",
    "latitude",
    "longitude",
    "location",
    "geo",
    "gps",
    "coordinates",
];
const IMAGE: &[&str] = &[
    "photo",
    "avatar",
    "signature",
    "selfie",
    "portrait",
    "picture",
];
const FAMILY: &[&str] = &[
    "marital",
    "nationality",
    "citizenship",
    "dependants",
    "dependents",
    "spouse",
];
const FREE_TEXT: &[&str] = &[
    "note",
    "notes",
    "comment",
    "comments",
    "description",
    "message",
    "messages",
    "body",
    "detail",
    "details",
    "remark",
    "remarks",
    "bio",
    "biography",
    "reason",
    "summary",
    "memo",
    "narrative",
    "feedback",
    "text",
    "content",
];
/// Who did it: a staff member's name or email in an audit column.
const ACTOR_LAST: &[&str] = &["by"];
const ACTOR: &[&str] = &["author", "actor", "owner", "assignee", "reporter"];
/// Names that are codes and references, whatever their type.
const NOT_PERSONAL: &[&str] = &[
    "currency",
    "locale",
    "language",
    "timezone",
    "time zone",
    "sku",
    "slug",
    "order number",
    "invoice number",
    "line number",
    "version",
    "checksum",
    "hash",
];
const PERSON_REF: &[&str] = &[
    "customer",
    "user",
    "member",
    "person",
    "account",
    "stripe",
    "paypal",
    "braintree",
    "payment",
    "subscriber",
    "client",
];
const REF_LAST: &[&str] = &["id", "ref", "reference", "number", "no"];

fn text_like(base: &str) -> bool {
    matches!(base, "text" | "varchar" | "bpchar" | "citext" | "name")
}

fn date_like(base: &str) -> bool {
    matches!(base, "date" | "timestamp" | "timestamptz")
}

/// Types pgman knows and keeps unless the name says otherwise.
fn plain_type(base: &str, category: &str) -> bool {
    matches!(
        base,
        "int2"
            | "int4"
            | "int8"
            | "numeric"
            | "float4"
            | "float8"
            | "money"
            | "bool"
            | "date"
            | "time"
            | "timetz"
            | "timestamp"
            | "timestamptz"
            | "interval"
            | "uuid"
            | "oid"
            | "varchar"
            | "bpchar"
            | "char"
            | "bit"
            | "varbit"
    ) || category == "E"
}

/// The default for `col` of `table` (named `table_name`, an Envers `_aud`
/// table's base), from its name and type. Pure.
pub fn default_rule(
    table_name: &str,
    col: &Column,
    extra_personal: &[String],
    extra_plain: &[String],
) -> (Rule, Origin) {
    let w = words(&col.name);
    let base = col.ty.base.as_str();
    let why = |s: &str| Origin::Default(s.to_string());
    let last = w.last().map(String::as_str).unwrap_or("");
    let text = text_like(base) || col.ty.array_of.is_some();
    // Specific before general: a token is a token whatever else it is.
    if any(&w, SECRET) {
        return (Rule::Null, why("a secret, by name"));
    }
    if any(&w, PASSWORD) {
        return if text {
            (Rule::Password(String::new()), why("a password, by name"))
        } else {
            (Rule::Null, why("a password, by name"))
        };
    }
    if last == "at"
        || matches!(
            col.name.as_str(),
            "created" | "updated" | "deleted" | "modified"
        )
    {
        return (Rule::Keep, Origin::Plain);
    }
    if any(&w, SPECIAL) {
        return (Rule::Null, why("special-category data, by name"));
    }
    let plain_listed = extra_plain.iter().any(|p| has(&w, &p.to_lowercase()));
    if plain_listed {
        return (
            Rule::Keep,
            Origin::Default("not personal, by the rules file's [classify]".into()),
        );
    }
    if extra_personal.iter().any(|p| has(&w, &p.to_lowercase())) {
        return (Rule::Null, why("personal, by the rules file's [classify]"));
    }
    // A code-like last word keeps the column unless what comes before it
    // is personal whatever it's called: a postcode, an identifier, a
    // device's, family details. (An integer `*_id` is a key, kept.)
    let itself = (last != "id" || text)
        && (any(&w, IDENTIFIER) || any(&w, POSTCODE) || any(&w, DEVICE) || any(&w, FAMILY));
    if FLAG_LAST.contains(&last)
        || (VALUE_LAST.contains(&last) && !itself && !(last == "id" && text && any(&w, PERSON_REF)))
    {
        return (Rule::Keep, Origin::Plain);
    }
    if any(&w, EMAIL) && text {
        return (Rule::Email, why("an email address, by name"));
    }
    if any(&w, PHONE) && text {
        return (Rule::Phone, why("a phone number, by name"));
    }
    if text {
        if any(&w, FIRST) {
            return (Rule::Name(NamePart::First), why("a first name, by name"));
        }
        if any(&w, LAST) {
            return (Rule::Name(NamePart::Last), why("a last name, by name"));
        }
        if any(&w, FULL)
            || (matches!(col.name.as_str(), "name" | "title") && PEOPLE.contains(&table_name))
        {
            return (Rule::Name(NamePart::Full), why("a person's name, by name"));
        }
    }
    if any(&w, IDENTIFIER) {
        return (Rule::Null, why("an identifier or pay, by name"));
    }
    // An IP or MAC address before a postal one.
    if any(&w, NETWORK) {
        return if text || matches!(base, "inet" | "cidr") {
            (Rule::Ip, why("an IP address, by name"))
        } else {
            (Rule::Null, why("an IP address, by name"))
        };
    }
    if any(&w, DEVICE) {
        return (Rule::Null, why("about a person's device, by name"));
    }
    if any(&w, POSTCODE) {
        return if text {
            (Rule::Postcode, why("a postcode, by name"))
        } else {
            (Rule::Null, why("a postcode, by name"))
        };
    }
    if any(&w, ADDRESS) {
        return (Rule::Null, why("an address, by name"));
    }
    if any(&w, BIRTH) {
        return if date_like(base) {
            (Rule::Year, why("a date of birth, by name"))
        } else {
            (Rule::Null, why("a date of birth, by name"))
        };
    }
    if any(&w, AGE) {
        return (Rule::Null, why("an age, by name"));
    }
    if any(&w, LOCATION) || any(&w, IMAGE) || any(&w, FAMILY) {
        return (Rule::Null, why("about a person, by name"));
    }
    if ACTOR_LAST.contains(&last) || any(&w, ACTOR) {
        return (Rule::Null, why("who did it, by name"));
    }
    if any(&w, FREE_TEXT) {
        return (Rule::Null, why("free text, by name"));
    }
    if REF_LAST.contains(&last) && any(&w, PERSON_REF) && text {
        return (Rule::Hash, why("someone's id elsewhere, by name"));
    }
    if any(&w, NOT_PERSONAL) {
        return (Rule::Keep, Origin::Plain);
    }
    // By type: what can hold anything, and what pgman doesn't know.
    match base {
        "text" | "citext" | "json" | "jsonb" | "xml" | "bytea" | "tsvector" => {
            return (Rule::Null, why(&format!("{base}: anything can be in it")))
        }
        "varchar" if col.ty.max_len.is_none() => {
            return (
                Rule::Null,
                why("varchar with no length: anything can be in it"),
            )
        }
        "inet" | "cidr" => return (Rule::Ip, why("an address, by type")),
        "macaddr" | "macaddr8" => return (Rule::Null, why("a device's address, by type")),
        _ => {}
    }
    if let Some(elem) = &col.ty.array_of {
        if !plain_type(elem, "") {
            return (Rule::Null, why("an array of text"));
        }
        return (Rule::Keep, Origin::Plain);
    }
    if !plain_type(base, &col.ty.category) {
        return (
            Rule::Null,
            why(&format!("{base}: a type pgman doesn't know")),
        );
    }
    (Rule::Keep, Origin::Plain)
}

/// A framework table's preset, if `table` is one: (kept whole, or rows
/// dropped, and the framework). Pure.
fn preset(name: &str) -> Option<(bool, &'static str)> {
    match name {
        "flyway_schema_history" => Some((true, "Flyway's history")),
        "databasechangelog" | "databasechangeloglock" => Some((true, "Liquibase's history")),
        "revinfo" => Some((true, "Envers' revisions")),
        "spring_session" | "spring_session_attributes" => Some((false, "Spring Session")),
        "persistent_logins" => Some((false, "Spring Security's remember-me")),
        "oauth2_authorized_client"
        | "oauth_access_token"
        | "oauth_refresh_token"
        | "oauth_code"
        | "oauth2_authorization"
        | "oauth2_authorization_consent" => Some((false, "OAuth tokens")),
        _ => None,
    }
}

/// Union-find over (table, column).
struct Groups {
    parent: HashMap<(String, String), (String, String)>,
}

impl Groups {
    fn find(&mut self, k: &(String, String)) -> (String, String) {
        let p = self.parent.get(k).cloned().unwrap_or_else(|| k.clone());
        if &p == k {
            return p;
        }
        let root = self.find(&p);
        self.parent.insert(k.clone(), root.clone());
        root
    }

    fn join(&mut self, a: (String, String), b: (String, String)) {
        let (ra, rb) = (self.find(&a), self.find(&b));
        if ra != rb {
            self.parent.insert(ra, rb);
        }
    }
}

/// The plan for `catalog` under `rules`. Pure.
pub fn plan(catalog: &Catalog, rules: &RuleSet) -> Plan {
    let mut problems = Vec::new();
    let mut decided: HashMap<(String, String), (Rule, Origin)> = HashMap::new();
    let mut drops: HashMap<String, String> = HashMap::new();
    for problem in rules.unknown(catalog) {
        problems.push(problem);
    }
    for t in catalog.tables.iter().filter(|t| t.has_rows()) {
        let q = t.qualified();
        let table_rules = rules.table(t);
        let preset = preset(&t.name);
        if let Some((false, what)) = preset {
            drops.insert(q.clone(), format!("{what}: no rows"));
        }
        if let Some(tr) = table_rules {
            if tr.rows == Some(Rows::Drop) {
                drops.insert(q.clone(), "the rules file says rows = \"drop\"".into());
            }
        }
        // An Envers audit table is masked as its base table is.
        let base_name = t.name.strip_suffix("_aud").unwrap_or(&t.name);
        let base = catalog
            .table(&format!("{}.{base_name}", t.schema))
            .filter(|_| base_name != t.name);
        for c in t.columns.iter().filter(|c| !c.generated) {
            let mut d = match preset {
                Some((true, what)) => (Rule::Keep, Origin::Preset(what.to_string())),
                _ => {
                    let from = base.and_then(|b| b.column(&c.name)).unwrap_or(c);
                    let mut d = default_rule(base_name, from, &rules.personal, &rules.not_personal);
                    if base.is_some() && !std::ptr::eq(from, c) {
                        if let Origin::Default(why) = &d.1 {
                            d.1 = Origin::Default(format!("{why} (as in {base_name})"));
                        }
                    }
                    d
                }
            };
            if let Some(r) = table_rules.and_then(|tr| tr.rule_for(&c.name)) {
                // Keeping what the defaults would mask is a decision a
                // reviewer reads: it says why.
                if r.rule == Rule::Keep && d.0 != Rule::Keep && r.reason.is_none() {
                    problems.push(format!(
                        "{q}.{}: keep, where the default is {} ({}), needs reason = \"…\"",
                        c.name,
                        d.0.name(),
                        match &d.1 {
                            Origin::Default(why) | Origin::Preset(why) => why.clone(),
                            _ => "by default".to_string(),
                        }
                    ));
                }
                d = (r.rule.clone(), Origin::Rules);
            }
            decided.insert((q.clone(), c.name.clone()), d);
        }
    }
    // Joins: a referencing column takes its referenced column's rule, and
    // every column in a joined group fits the shortest of them.
    let mut groups = Groups {
        parent: HashMap::new(),
    };
    let mut edges = Vec::new();
    for t in catalog.tables.iter().filter(|t| t.has_rows()) {
        for fk in &t.foreign_keys {
            for (c, r) in fk.columns.iter().zip(&fk.ref_columns) {
                let from = (t.qualified(), c.clone());
                let to = (fk.ref_table.clone(), r.clone());
                groups.join(from.clone(), to.clone());
                edges.push((from, to));
            }
        }
    }
    // Follow to a fixpoint: a chain of references takes the head's rule.
    for _ in 0..32 {
        let mut changed = false;
        for (from, to) in &edges {
            let Some((rule, origin)) = decided.get(to).cloned() else {
                continue;
            };
            let Some(own) = decided.get(from).cloned() else {
                continue;
            };
            if own.0 == rule {
                // The same rule already: say it follows the key, once.
                if rule != Rule::Keep && !matches!(own.1, Origin::Rules | Origin::Follows(_)) {
                    decided.insert(
                        from.clone(),
                        (rule, Origin::Follows(format!("{}.{}", to.0, to.1))),
                    );
                    changed = true;
                }
                continue;
            }
            if own.1 == Origin::Rules && origin != Origin::Rules {
                // The rules file named the referencing side: it wins up
                // the chain too, or the join breaks.
                decided.insert(
                    to.clone(),
                    (
                        own.0.clone(),
                        Origin::Follows(format!("{}.{}", from.0, from.1)),
                    ),
                );
                changed = true;
                continue;
            }
            if own.1 == Origin::Rules && origin == Origin::Rules {
                problems.push(format!(
                    "{}.{} references {}.{}, and the rules give them different rules ({} and {}): \
                     give the rule on {}.{} alone",
                    from.0,
                    from.1,
                    to.0,
                    to.1,
                    own.0.name(),
                    rule.name(),
                    to.0,
                    to.1
                ));
                continue;
            }
            decided.insert(
                from.clone(),
                (rule, Origin::Follows(format!("{}.{}", to.0, to.1))),
            );
            changed = true;
        }
        if !changed {
            break;
        }
    }
    let mut fit_of: HashMap<(String, String), usize> = HashMap::new();
    for t in &catalog.tables {
        for c in &t.columns {
            if let Some(n) = c.ty.max_len {
                let root = groups.find(&(t.qualified(), c.name.clone()));
                let e = fit_of.entry(root).or_insert(n);
                *e = (*e).min(n);
            }
        }
    }
    let mut tables = Vec::new();
    for t in catalog.tables.iter().filter(|t| t.has_rows()) {
        let q = t.qualified();
        let mut columns = Vec::new();
        for c in t.columns.iter().filter(|c| !c.generated) {
            let key = (q.clone(), c.name.clone());
            let (mut rule, origin) = decided
                .get(&key)
                .cloned()
                .unwrap_or((Rule::Keep, Origin::Plain));
            let fit = fit_of.get(&groups.find(&key)).copied();
            check(t, c, &rule, &origin, &mut problems);
            // NULL where it must stay unique and can't be NULL: a hash, so
            // every row still differs.
            if rule == Rule::Null && c.unique && c.ty.not_null {
                rule = Rule::Hash;
            }
            columns.push(Decision {
                column: c.name.clone(),
                rule,
                origin,
                fit,
                not_null: c.ty.not_null,
                unique: c.unique,
                base: c.ty.base.clone(),
                enum_first: c.ty.enum_first.clone(),
                array: c.ty.array_of.is_some(),
            });
        }
        tables.push(TablePlan {
            table: q.clone(),
            drop_rows: drops.contains_key(&q),
            drop_why: drops.get(&q).cloned(),
            columns,
            primary_key: t.primary_key.clone(),
        });
    }
    Plan {
        tables,
        large_objects: rules.large_objects_kept,
        problems,
    }
}

/// What `rule` can't do to `c` of `t`, added to `problems`. Pure.
fn check(t: &Table, c: &Column, rule: &Rule, origin: &Origin, problems: &mut Vec<String>) {
    let at = format!("{}.{}", t.qualified(), c.name);
    let base = c.ty.base.as_str();
    if c.unique && rule.lossy() {
        problems.push(format!(
            "{at} must stay unique, and {} can make two values one: use hash, or keep",
            rule.name()
        ));
    }
    if matches!(rule, Rule::Year | Rule::DateShift(_)) {
        if !date_like(base) {
            problems.push(format!(
                "{at} is {}, not a date: {} can't apply",
                c.ty.display,
                rule.name()
            ));
        }
        if t.partition_key.contains(&c.name) {
            problems.push(format!(
                "{at} is what {} is partitioned by: moving it would move rows between partitions. \
                 Keep it, or null it",
                t.qualified()
            ));
        }
    }
    if matches!(rule, Rule::DateShift(_)) && t.primary_key.is_empty() {
        problems.push(format!(
            "{at}: date.shift keys each row's shift by its primary key, and {} has none",
            t.qualified()
        ));
    }
    let needs_text = matches!(
        rule,
        Rule::Email | Rule::Name(_) | Rule::Phone | Rule::Postcode | Rule::Password(_)
    );
    if needs_text && !text_like(base) {
        problems.push(format!(
            "{at} is {}, not text: {} can't apply",
            c.ty.display,
            rule.name()
        ));
    }
    if *rule == Rule::Ip && !(text_like(base) || matches!(base, "inet" | "cidr")) {
        problems.push(format!("{at} is {}: ip can't apply", c.ty.display));
    }
    if *rule == Rule::Hash
        && !(text_like(base)
            || matches!(
                base,
                "int2" | "int4" | "int8" | "uuid" | "numeric" | "bytea"
            ))
    {
        problems.push(format!("{at} is {}: hash can't apply", c.ty.display));
    }
    if *rule == Rule::Null && c.ty.not_null && origin == &Origin::Rules && c.unique {
        // Becomes a hash; fine.
    }
    if *rule == Rule::Null
        && c.ty.not_null
        && empty_value(base, c.ty.enum_first.as_deref(), c.ty.array_of.is_some()).is_none()
    {
        problems.push(format!(
            "{at} is {} NOT NULL, and pgman has no empty value for it: give it a rule \
             (fixed, or keep with a reason)",
            c.ty.display
        ));
    }
    if let Rule::Fixed(v) = rule {
        if let Some(n) = c.ty.max_len {
            if v.chars().count() > n {
                problems.push(format!(
                    "{at} takes {n} characters; the fixed value is longer"
                ));
            }
        }
    }
}

/// The value standing in for NULL where NULL can't be stored, in COPY
/// text: `''` for text, `{}` for JSON and arrays, and so on. `None` when
/// pgman has none for the type. Pure.
pub fn empty_value(base: &str, enum_first: Option<&str>, array: bool) -> Option<String> {
    if array {
        return Some("{}".into());
    }
    if let Some(label) = enum_first {
        return Some(label.to_string());
    }
    Some(
        match base {
            "text" | "varchar" | "bpchar" | "citext" | "name" | "xml" => "",
            "json" | "jsonb" => "{}",
            "bytea" => "\\x",
            "bool" => "f",
            "int2" | "int4" | "int8" | "numeric" | "float4" | "float8" | "money" => "0",
            "inet" | "cidr" => "192.0.2.0",
            "macaddr" => "00:00:5e:00:53:00",
            "macaddr8" => "00:00:5e:ff:fe:00:53:00",
            "date" => "1970-01-01",
            "timestamp" | "timestamptz" => "1970-01-01 00:00:00",
            "time" | "timetz" => "00:00:00",
            "interval" => "0",
            "uuid" => "00000000-0000-4000-8000-000000000000",
            "tsvector" => "",
            _ => return None,
        }
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::super::catalog::{ColumnType, ForeignKey};
    use super::super::rules;
    use super::*;

    fn ty(base: &str, max_len: Option<usize>, not_null: bool) -> ColumnType {
        ColumnType {
            display: base.into(),
            base: base.into(),
            base_schema: "pg_catalog".into(),
            category: match base {
                "text" | "varchar" => "S".into(),
                _ => "".into(),
            },
            enum_first: None,
            array_of: None,
            max_len,
            not_null,
        }
    }

    fn c(name: &str, base: &str, max_len: Option<usize>) -> Column {
        Column {
            num: 0,
            name: name.into(),
            ty: ty(base, max_len, false),
            generated: false,
            unique: false,
        }
    }

    fn rule(table: &str, name: &str, base: &str, max_len: Option<usize>) -> Rule {
        default_rule(table, &c(name, base, max_len), &[], &[]).0
    }

    #[test]
    fn names_split_into_words() {
        assert_eq!(words("customer_email"), ["customer", "email"]);
        assert_eq!(words("customerEmail2"), ["customer", "email", "2"]);
        assert_eq!(words("IPAddress"), ["ipaddress"]);
        assert_eq!(words("addressLine1"), ["address", "line", "1"]);
        assert_eq!(words("__x__"), ["x"]);
    }

    #[test]
    fn whole_words_only() {
        for (name, want) in [
            ("stage", Rule::Keep),
            ("message", Rule::Null),
            ("age", Rule::Null),
            ("age_band", Rule::Null),
            ("zip", Rule::Postcode),
            ("recipient", Rule::Keep),
            ("payload_kind", Rule::Keep),
            ("ip_address", Rule::Ip),
            ("payment_id", Rule::Hash),
        ] {
            assert_eq!(rule("t", name, "varchar", Some(64)), want, "{name}");
        }
    }

    #[test]
    fn hibernate_varchar_255_is_judged_by_name_alone() {
        for name in [
            "currency_code",
            "status",
            "sku",
            "locale",
            "order_number",
            "installed_by_type",
            "kind",
        ] {
            assert_eq!(
                rule("orders", name, "varchar", Some(255)),
                Rule::Keep,
                "{name}"
            );
        }
        for (name, want) in [
            ("email", Rule::Email),
            ("contact_email", Rule::Email),
            ("mobile", Rule::Phone),
            ("first_name", Rule::Name(NamePart::First)),
            ("surname", Rule::Name(NamePart::Last)),
            ("display_name", Rule::Name(NamePart::Full)),
            ("postcode", Rule::Postcode),
            ("notes", Rule::Null),
            ("description", Rule::Null),
            ("created_by", Rule::Null),
            ("nino", Rule::Null),
            ("stripe_customer_id", Rule::Hash),
            ("address_line_1", Rule::Null),
        ] {
            assert_eq!(rule("orders", name, "varchar", Some(255)), want, "{name}");
        }
    }

    #[test]
    fn specific_words_win_over_general_ones() {
        assert_eq!(
            rule("u", "password_reset_token", "varchar", Some(64)),
            Rule::Null,
            "a token"
        );
        assert_eq!(
            rule("u", "password_hash", "varchar", Some(60)),
            Rule::Password(String::new())
        );
        assert_eq!(
            rule("u", "health_check_at", "timestamptz", None),
            Rule::Keep,
            "a timestamp"
        );
        assert_eq!(
            rule("u", "gender_code", "varchar", Some(2)),
            Rule::Null,
            "special beats code"
        );
        assert_eq!(rule("u", "email_verified", "bool", None), Rule::Keep);
        assert_eq!(rule("u", "email_type", "varchar", Some(16)), Rule::Keep);
        assert_eq!(rule("u", "is_vulnerable", "bool", None), Rule::Null);
        assert_eq!(rule("u", "api_key", "varchar", Some(64)), Rule::Null);
        assert_eq!(rule("u", "date_of_birth", "date", None), Rule::Year);
        assert_eq!(rule("u", "birth_year", "int4", None), Rule::Null);
    }

    #[test]
    fn a_code_like_last_word_doesnt_hide_the_thing_itself() {
        for (name, want) in [
            ("postal_code", Rule::Postcode),
            ("zip_code", Rule::Postcode),
            ("post_code", Rule::Postcode),
            ("postalCode", Rule::Postcode),
            ("sort_code", Rule::Null),
            ("sortcode", Rule::Null),
            ("tax_id", Rule::Null),
            ("tax_code", Rule::Null),
            ("national_id", Rule::Null),
            ("device_id", Rule::Null),
            ("marital_status", Rule::Null),
            ("verification_code", Rule::Null),
            ("reset_code", Rule::Null),
            ("pin_code", Rule::Null),
        ] {
            assert_eq!(rule("u", name, "varchar", Some(40)), want, "{name}");
        }
        // Codes and flags about them are still kept, and an integer id is
        // a key.
        for name in [
            "currency_code",
            "country_code",
            "order_status",
            "device_type",
            "postcode_valid",
        ] {
            assert_eq!(rule("u", name, "varchar", Some(40)), Rule::Keep, "{name}");
        }
        assert_eq!(rule("u", "device_id", "int8", None), Rule::Keep);
    }

    #[test]
    fn a_bare_name_is_a_persons_only_on_a_peoples_table() {
        assert_eq!(
            rule("customers", "name", "varchar", Some(255)),
            Rule::Name(NamePart::Full)
        );
        assert_eq!(rule("products", "name", "varchar", Some(255)), Rule::Keep);
        assert_eq!(
            rule("staff", "title", "varchar", Some(20)),
            Rule::Name(NamePart::Full)
        );
        assert_eq!(rule("books", "title", "varchar", Some(200)), Rule::Keep);
    }

    #[test]
    fn types_that_hold_anything_are_nulled() {
        for base in [
            "text", "jsonb", "json", "bytea", "xml", "citext", "tsvector", "geometry",
        ] {
            assert_eq!(rule("t", "payload_x", base, None), Rule::Null, "{base}");
        }
        assert_eq!(rule("t", "unbounded", "varchar", None), Rule::Null);
        assert_eq!(
            rule("t", "status", "text", None),
            Rule::Keep,
            "a status is a status"
        );
        assert_eq!(rule("t", "amount", "numeric", None), Rule::Keep);
        assert_eq!(rule("t", "src", "inet", None), Rule::Ip);
        let mut tags = c("tags", "_text", None);
        tags.ty.array_of = Some("text".into());
        assert_eq!(default_rule("t", &tags, &[], &[]).0, Rule::Null);
    }

    #[test]
    fn the_rules_files_classify_lists_extend_the_words() {
        let col = c("handle", "varchar", Some(40));
        assert_eq!(default_rule("t", &col, &[], &[]).0, Rule::Keep);
        assert_eq!(
            default_rule("t", &col, &["handle".into()], &[]).0,
            Rule::Null
        );
        let notes = c("notes", "text", None);
        assert_eq!(
            default_rule("t", &notes, &[], &["notes".into()]).0,
            Rule::Keep
        );
    }

    fn table(name: &str, cols: Vec<Column>) -> Table {
        Table {
            schema: "public".into(),
            name: name.into(),
            kind: "r".into(),
            columns: cols,
            primary_key: vec!["id".into()],
            foreign_keys: vec![],
            partition_key: vec![],
        }
    }

    fn parse(text: &str) -> RuleSet {
        rules::parse(text).unwrap()
    }

    #[test]
    fn joins_take_the_referenced_columns_rule_and_its_length() {
        let mut customers = table(
            "customers",
            vec![c("id", "int4", None), c("email", "varchar", Some(100))],
        );
        customers.columns[1].unique = true;
        let mut orders = table(
            "orders",
            vec![
                c("id", "int4", None),
                c("customer_email", "varchar", Some(50)),
                c("ref_code", "varchar", Some(10)),
            ],
        );
        orders.foreign_keys.push(ForeignKey {
            columns: vec!["customer_email".into()],
            ref_table: "public.customers".into(),
            ref_columns: vec!["email".into()],
        });
        let cat = Catalog {
            tables: vec![customers, orders],
            large_objects: 0,
        };
        let p = plan(&cat, &RuleSet::default());
        let email = &p.table("public.customers").unwrap().columns[1];
        let follow = &p.table("public.orders").unwrap().columns[1];
        assert_eq!(email.rule, Rule::Email);
        assert_eq!(follow.rule, Rule::Email);
        assert_eq!(
            follow.origin,
            Origin::Follows("public.customers.email".into())
        );
        assert_eq!(
            (email.fit, follow.fit),
            (Some(50), Some(50)),
            "both fit the shorter"
        );
        // The rules file on the referencing side carries up the chain.
        let p = plan(
            &cat,
            &parse("version = 1\n[tables.orders]\ncustomer_email = \"hash\"\n"),
        );
        assert_eq!(
            p.table("public.customers").unwrap().columns[1].rule,
            Rule::Hash
        );
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        let p = plan(&cat, &parse("version = 1\n[tables.orders]\ncustomer_email = \"hash\"\n[tables.customers]\nemail = \"email\"\n"));
        assert!(
            p.problems.iter().any(|m| m.contains("different rules")),
            "{:?}",
            p.problems
        );
    }

    #[test]
    fn presets_and_audit_tables() {
        let cat = Catalog {
            tables: vec![
                table(
                    "flyway_schema_history",
                    vec![c("installed_by", "varchar", Some(100))],
                ),
                table(
                    "spring_session",
                    vec![c("principal_name", "varchar", Some(100))],
                ),
                table(
                    "customers",
                    vec![c("id", "int4", None), c("name", "varchar", Some(100))],
                ),
                table(
                    "customers_aud",
                    vec![
                        c("id", "int4", None),
                        c("name", "varchar", Some(100)),
                        c("rev", "int4", None),
                    ],
                ),
            ],
            large_objects: 0,
        };
        let p = plan(&cat, &RuleSet::default());
        let fly = p.table("public.flyway_schema_history").unwrap();
        assert_eq!(fly.columns[0].rule, Rule::Keep);
        assert!(matches!(fly.columns[0].origin, Origin::Preset(_)));
        assert!(p.table("public.spring_session").unwrap().drop_rows);
        let aud = p.table("public.customers_aud").unwrap();
        assert_eq!(
            aud.columns[1].rule,
            Rule::Name(NamePart::Full),
            "as in customers"
        );
        assert_eq!(aud.columns[2].rule, Rule::Keep);
    }

    #[test]
    fn what_cant_be_stored_is_a_problem() {
        let mut uniq = c("postcode", "varchar", Some(10));
        uniq.unique = true;
        let mut part = c("dob", "date", None);
        part.ty.not_null = true;
        let mut t = table(
            "people",
            vec![
                c("id", "int4", None),
                uniq,
                part,
                c("code", "varchar", Some(3)),
            ],
        );
        t.partition_key = vec!["dob".into()];
        let mut geo = c("shape", "geometry", None);
        geo.ty.not_null = true;
        let mut g = table("places", vec![geo]);
        g.primary_key.clear();
        let cat = Catalog {
            tables: vec![t, g],
            large_objects: 0,
        };
        let p = plan(
            &cat,
            &parse(
                "version = 1\n[tables.people]\ncode = { rule = \"fixed\", value = \"toolong\" }\n",
            ),
        );
        let all = p.problems.join("\n");
        assert!(
            all.contains("public.people.postcode must stay unique"),
            "{all}"
        );
        assert!(
            all.contains("public.people.dob is what public.people is partitioned by"),
            "{all}"
        );
        assert!(
            all.contains("public.places.shape is geometry NOT NULL, and pgman has no empty value"),
            "{all}"
        );
        assert!(
            all.contains("public.people.code takes 3 characters"),
            "{all}"
        );
    }

    #[test]
    fn a_nulled_unique_not_null_column_is_hashed_instead() {
        let mut col = c("notes_key", "text", None);
        col.unique = true;
        col.ty.not_null = true;
        let cat = Catalog {
            tables: vec![table("t", vec![col])],
            large_objects: 0,
        };
        let p = plan(&cat, &RuleSet::default());
        assert_eq!(p.tables[0].columns[0].rule, Rule::Hash);
    }

    #[test]
    fn empty_values_fit_their_types() {
        assert_eq!(empty_value("text", None, false).as_deref(), Some(""));
        assert_eq!(empty_value("jsonb", None, false).as_deref(), Some("{}"));
        assert_eq!(empty_value("int4", None, true).as_deref(), Some("{}"));
        assert_eq!(
            empty_value("mood", Some("ok"), false).as_deref(),
            Some("ok")
        );
        assert_eq!(empty_value("bool", None, false).as_deref(), Some("f"));
        assert_eq!(empty_value("geometry", None, false), None);
    }
}
