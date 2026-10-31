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
use super::rules::{Rows, RuleSet, TableRules};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

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
/// and a capital, and between letters and digits; lower-cased; and a word
/// that runs known ones together pulled apart ([`run_together`]).
/// `customerEmail2` is `customer email 2`, `dateofbirth` is `date of
/// birth`. Pure.
pub fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for w in parts(name) {
        match run_together(&w) {
            Some(split) => out.extend(split),
            None => out.push(w),
        }
    }
    out
}

/// [`words`] without pulling run-together words apart.
fn parts(name: &str) -> Vec<String> {
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

/// Words that join the phrases below when Postgres has run them together:
/// an unquoted `dateOfBirth` is folded to `dateofbirth`.
const JOINING: &[&str] = &[
    "of",
    "to",
    "by",
    "from",
    "next",
    "date",
    "number",
    "name",
    "social",
    "security",
    "bank",
    "card",
    "emergency",
    "contact",
    "account",
    "sort",
    "code",
    "personal",
    "place",
    "time",
    "society",
];

/// Words found inside a longer one even where the rest of it is no word
/// pgman knows: `homemobile`. Long and unambiguous, so `cancellation`
/// holds no `cell` and `warehouse` no `house`.
const INSIDE: &[&str] = &[
    "email",
    "phone",
    "telephone",
    "mobile",
    "firstname",
    "lastname",
    "surname",
    "forename",
    "forenames",
    "birth",
    "birthday",
    "birthdate",
    "passport",
    "postcode",
    "postalcode",
    "zipcode",
    "address",
    "sortcode",
    "accountnumber",
    "cardnumber",
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "credential",
    "credentials",
    "apikey",
    "sexual",
    "sexuality",
    "ethnicity",
    "ethnic",
    "religion",
    "religious",
    "gender",
    "health",
    "medical",
    "diagnosis",
    "disability",
    "criminal",
    "conviction",
    "convictions",
    "offence",
    "offense",
    "pregnancy",
    "pregnant",
    "biometric",
    "genetic",
    "political",
    "nationality",
    "citizenship",
    "marital",
    "salary",
    "income",
    "latitude",
    "longitude",
    "photo",
    "avatar",
    "signature",
    "selfie",
    "portrait",
    "nickname",
    "username",
    "fullname",
    "displayname",
    "middlename",
    "social",
    "security",
    "number",
    "account",
    "contact",
    "emergency",
    "personal",
    "enabled",
    "verified",
    "confirmed",
    "consent",
    "optin",
    "comment",
    "comments",
    "description",
    "remark",
    "remarks",
    "narrative",
    "feedback",
];

/// Every word the lists name.
fn known(word: &str) -> bool {
    static KNOWN: OnceLock<HashSet<&'static str>> = OnceLock::new();
    KNOWN
        .get_or_init(|| {
            [
                SECRET,
                PASSWORD,
                SPECIAL,
                FLAG_LAST,
                VALUE_LAST,
                EMAIL,
                PHONE,
                FIRST,
                LAST,
                FULL,
                PEOPLE,
                HOLDER,
                IDENTIFIER,
                ADDRESS,
                POSTCODE,
                BIRTH,
                AGE,
                NETWORK,
                DEVICE,
                LOCATION,
                IMAGE,
                FAMILY,
                FREE_TEXT,
                ACTOR_LAST,
                ACTOR,
                NOT_PERSONAL,
                PERSON_REF,
                REF_LAST,
                JOINING,
                INSIDE,
            ]
            .iter()
            .flat_map(|list| list.iter().flat_map(|p| p.split(' ')))
            .collect()
        })
        .contains(word)
}

/// A lower-case word that runs others together, pulled apart: into words
/// pgman knows wholly (`dateofbirth` is `date of birth`, `banksortcode`
/// `bank sort code`), or else around the [`INSIDE`] words in it
/// (`sentmobilenumber` is `sent mobile number`). `None` when it is a word
/// pgman knows, or holds none. Pure.
pub fn run_together(word: &str) -> Option<Vec<String>> {
    if word.len() < 5 || !word.bytes().all(|b| b.is_ascii_lowercase()) || known(word) {
        return None;
    }
    let n = word.len();
    // Wholly: the fewest known words that make it up.
    let mut best: Vec<Option<(usize, usize)>> = vec![None; n + 1];
    best[0] = Some((0, 0));
    for j in 1..=n {
        best[j] = (0..j)
            .filter(|&i| known(&word[i..j]))
            .filter_map(|i| best[i].map(|(k, _)| (k + 1, i)))
            .min();
    }
    if best[n].is_some() {
        let mut out = Vec::new();
        let mut j = n;
        while j > 0 {
            let (_, i) = best[j].expect("reached from the end");
            out.push(word[i..j].to_string());
            j = i;
        }
        out.reverse();
        return Some(out);
    }
    // Around INSIDE words: the fewest letters left over, then the fewest
    // words. A step is one leftover letter or one INSIDE word.
    let mut cost: Vec<(usize, usize, usize, bool)> = vec![(usize::MAX, 0, 0, false); n + 1];
    cost[0] = (0, 0, 0, false);
    for i in 0..n {
        let (left, count, _, _) = cost[i];
        if left == usize::MAX {
            continue;
        }
        let letter = (left + 1, count, i, false);
        if (letter.0, letter.1) < (cost[i + 1].0, cost[i + 1].1) {
            cost[i + 1] = letter;
        }
        for w in INSIDE {
            let j = i + w.len();
            if j <= n && &word[i..j] == *w && (left, count + 1) < (cost[j].0, cost[j].1) {
                cost[j] = (left, count + 1, i, true);
            }
        }
    }
    let mut out: Vec<String> = Vec::new();
    let mut found = false;
    let mut leftover = String::new();
    let mut j = n;
    while j > 0 {
        let (_, _, i, is_word) = cost[j];
        if is_word {
            if !leftover.is_empty() {
                out.push(leftover.chars().rev().collect());
                leftover.clear();
            }
            out.push(word[i..j].to_string());
            found = true;
        } else {
            leftover.push_str(&word[i..j]);
        }
        j = i;
    }
    if !leftover.is_empty() {
        out.push(leftover.chars().rev().collect());
    }
    out.reverse();
    found.then_some(out)
}

/// Whether `w` holds `phrase` (one word, or several in a row, or those
/// run together as one: `accountnumber`). Pure.
fn has(w: &[String], phrase: &str) -> bool {
    let p: Vec<&str> = phrase.split(' ').collect();
    w.windows(p.len())
        .any(|win| win.iter().zip(&p).all(|(a, b)| a == b))
        || (p.len() > 1 && w.iter().any(|x| *x == p.concat()))
        // A word split from its digits: ipv4, last4.
        || (p.len() == 1
            && w.windows(2).any(|pair| {
                pair[1].bytes().all(|b| b.is_ascii_digit()) && format!("{}{}", pair[0], pair[1]) == p[0]
            }))
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
    "user code",
    "device code",
    "access key",
    "apikeys",
    "approval code",
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
    "sick",
    "pronouns",
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
    "theme",
    "color",
    "colour",
    "layout",
    "template",
    "style",
    "mode",
];
/// Whose colour is a person's.
const BODY: &[&str] = &["eye", "eyes", "skin", "hair", "complexion"];
/// A last word that is usually a code, not the thing — `currency_code`,
/// `order_status` — but can be the thing itself: `postal_code`,
/// `sort_code`, `marital_status`, `tax_id`.
const VALUE_LAST: &[&str] = &["code", "status", "state", "id"];
const EMAIL: &[&str] = &["email", "emails", "mail", "login", "reply to"];
const PHONE: &[&str] = &[
    "phone",
    "phones",
    "mobile",
    "tel",
    "telephone",
    "fax",
    "msisdn",
    "cell",
    "mob",
    "sms number",
];
const FIRST: &[&str] = &[
    "first name",
    "firstname",
    "given name",
    "forename",
    "forenames",
    "fname",
    "givenname",
    "given names",
];
const LAST: &[&str] = &[
    "last name",
    "lastname",
    "surname",
    "family name",
    "maiden name",
    "lname",
];
const FULL: &[&str] = &[
    "full name",
    "fullname",
    "display name",
    "middle name",
    "middlename",
    "initials",
    "nickname",
    "nick name",
    "user name",
    "username",
    "contact name",
    "account name",
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
    "profile",
    "profiles",
    "partner",
    "partners",
    "attendee",
    "attendees",
    "guest",
    "guests",
];
/// Whose name a `<word>_name` column holds, beside [`PEOPLE`]:
/// `account_holder_name`, `recipient_name`, `billing_name`.
const HOLDER: &[&str] = &[
    "holder",
    "cardholder",
    "payee",
    "payer",
    "recipient",
    "sender",
    "guest",
    "passenger",
    "beneficiary",
    "tenant",
    "owner",
    "signatory",
    "guardian",
    "parent",
    "spouse",
    "partner",
    "kin",
    "witness",
    "referee",
    "billing",
    "shipping",
    "delivery",
    "reviewer",
    "attendee",
];
const IDENTIFIER: &[&str] = &[
    "ssn",
    "social security",
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
    "building society",
    "roll number",
    "identification",
    "visa",
    "work permit",
    "acc number",
    "bonus",
    "commission",
    "last digits",
    "last four",
    "last4",
    "linkedin",
    "twitter",
    "discord",
    "mastodon",
    "bluesky",
    "orcid",
    "skype",
];
const ADDRESS: &[&str] = &[
    "address", "street", "city", "town", "county", "line 1", "line 2", "line 3", "house", "flat",
    "building", "locality", "addr",
];
const POSTCODE: &[&str] = &[
    "postcode",
    "post code",
    "postal code",
    "postalcode",
    "zip",
    "zipcode",
];
const BIRTH: &[&str] = &["dob", "birth", "birthday", "birthdate", "born"];
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
    "browser",
    "ja3",
    "ja4",
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
    "next of kin",
    "emergency contact",
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
    "payload",
    "subject",
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
    "login",
    "extern",
    "external",
];
const REF_LAST: &[&str] = &["id", "ref", "reference", "number", "no", "uid"];

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
    let (rule, origin) = by_name_and_type(table_name, col, extra_personal, extra_plain);
    // A transformer makes one value, not an array of them.
    if col.ty.array_of.is_some() && !matches!(rule, Rule::Keep | Rule::Null) {
        let why = match origin {
            Origin::Default(w) => format!("{w}, in an array"),
            _ => "in an array".to_string(),
        };
        return (Rule::Null, Origin::Default(why));
    }
    (rule, origin)
}

fn by_name_and_type(
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
    // When a password changed, whether a token was used: a date or a flag
    // is about the secret, not the secret.
    let about = col.ty.array_of.is_none()
        && matches!(
            base,
            "date" | "timestamp" | "timestamptz" | "time" | "timetz" | "interval" | "bool"
        );
    // Specific before general: a token is a token whatever else it is.
    if !about && any(&w, SECRET) {
        return (Rule::Null, why("a secret, by name"));
    }
    if !about && any(&w, PASSWORD) {
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
    // Something switched off isn't a disability: emails_disabled. A bare
    // disabled, or registered_disabled, is the monitoring answer.
    let switched_off = w.len() > 1
        && last == "disabled"
        && w[w.len() - 2] != "registered"
        && !any(&w[..w.len() - 1], SPECIAL);
    if any(&w, SPECIAL) && !switched_off {
        return (Rule::Null, why("special-category data, by name"));
    }
    // The rules file's words match the name as written, too: `bankdetails`.
    let raw = parts(&col.name);
    let listed = |list: &[String]| {
        list.iter().any(|p| {
            let p = p.to_lowercase();
            has(&w, &p) || has(&raw, &p)
        })
    };
    let plain_listed = listed(extra_plain);
    if plain_listed {
        return (
            Rule::Keep,
            Origin::Default("not personal, by the rules file's [classify]".into()),
        );
    }
    if listed(extra_personal) {
        return (Rule::Null, why("personal, by the rules file's [classify]"));
    }
    // A hash of something personal and guessable, a card's last digits or
    // a holder's name, is the thing: hash it again, so joins still join.
    // (A password's hash is a password, above.)
    if matches!(last, "hash" | "digest") && w.len() > 1 && (text || base == "bytea") {
        // Judged as the bounded text that was hashed.
        let mut of = Column {
            name: w[..w.len() - 1].join("_"),
            ..col.clone()
        };
        of.ty.base = "varchar".into();
        of.ty.max_len = Some(255);
        of.ty.array_of = None;
        if let (r, Origin::Default(what)) =
            by_name_and_type(table_name, &of, extra_personal, extra_plain)
        {
            // Free text hashes to no guess; anything else is a hash, kept.
            if r != Rule::Keep && !what.starts_with("free text") {
                return (Rule::Hash, Origin::Default(format!("a hash of {what}")));
            }
        }
        // A digest is as often a summary sent out as a hash.
        if last == "hash" {
            return (Rule::Keep, Origin::Plain);
        }
    }
    // A code-like last word keeps the column unless what comes before it
    // is personal whatever it's called: a postcode, an identifier, a
    // device's, family details. (An integer `*_id` is a key, kept.)
    let itself = (last != "id" || text)
        && (any(&w, IDENTIFIER) || any(&w, POSTCODE) || any(&w, DEVICE) || any(&w, FAMILY));
    // A colour is a setting, but not a person's: eye_colour, skin_tone.
    if matches!(last, "color" | "colour" | "tone") && any(&w, BODY) {
        return (Rule::Null, why("a person's appearance, by name"));
    }
    if FLAG_LAST.contains(&last)
        || (VALUE_LAST.contains(&last) && !itself && !(last == "id" && text && any(&w, PERSON_REF)))
    {
        return (Rule::Keep, Origin::Plain);
    }
    // An email's subject or body is free text, below.
    let about_text = FREE_TEXT.contains(&last);
    if any(&w, EMAIL) && text && !about_text {
        return (Rule::Email, why("an email address, by name"));
    }
    if any(&w, PHONE) && text && !about_text {
        return (Rule::Phone, why("a phone number, by name"));
    }
    if text {
        if any(&w, FIRST) {
            return (Rule::Name(NamePart::First), why("a first name, by name"));
        }
        if any(&w, LAST) {
            return (Rule::Name(NamePart::Last), why("a last name, by name"));
        }
        // A people's table by its last word too: app_user, tbl_customer.
        let table_words = words(table_name);
        let peoples = PEOPLE.contains(&table_name)
            || table_words
                .last()
                .is_some_and(|t| PEOPLE.contains(&t.as_str()));
        let whose = last == "name"
            && w.len() >= 2
            && (PEOPLE.contains(&w[w.len() - 2].as_str())
                || HOLDER.contains(&w[w.len() - 2].as_str()));
        if any(&w, FULL) || whose || (matches!(col.name.as_str(), "name" | "title") && peoples) {
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
    // A flag can't be a photo: require_signature.
    if any(&w, LOCATION) || (any(&w, IMAGE) && base != "bool") || any(&w, FAMILY) {
        return (Rule::Null, why("about a person, by name"));
    }
    if ACTOR_LAST.contains(&last) || any(&w, ACTOR) {
        return (Rule::Null, why("who did it, by name"));
    }
    // A number, date or flag can't hold free text: note_events, a
    // failure_reason enum's code.
    if any(&w, FREE_TEXT) && (text || !plain_type(base, &col.ty.category)) {
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
        "schema_migrations" => Some((true, "the migration history")),
        "ar_internal_metadata" => Some((true, "Rails' metadata")),
        "django_migrations" | "django_content_type" => Some((true, "Django's own")),
        "alembic_version" => Some((true, "Alembic's version")),
        "knex_migrations" | "knex_migrations_lock" => Some((true, "Knex's history")),
        "_prisma_migrations" => Some((true, "Prisma's history")),
        "django_session" => Some((false, "Django's sessions")),
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

/// The rules that apply to `t`, nearest first: its own block, then the
/// blocks of the tables it is a partition or inheritance child of, then —
/// for an Envers `_aud` table — its base table's. A column takes the
/// first rule it finds. Pure.
fn rules_chain<'a>(catalog: &Catalog, rules: &'a RuleSet, t: &Table) -> Vec<&'a TableRules> {
    let mut chain = Vec::new();
    let mut at = Some(t.clone());
    for _ in 0..16 {
        let Some(table) = at.take() else { break };
        chain.extend(rules.table(&table));
        at = table
            .parent
            .as_deref()
            .and_then(|p| catalog.table(p))
            .cloned();
    }
    if let Some(base) = t
        .name
        .strip_suffix("_aud")
        .and_then(|b| catalog.table(&format!("{}.{b}", t.schema)))
    {
        if base.name != t.name {
            chain.extend(rules_chain(catalog, rules, base));
        }
    }
    chain
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
    for h in &catalog.hypertables {
        problems.push(format!(
            "{h} is a TimescaleDB hypertable, which pgman can't mask yet: the copy would \
             come back a plain table, without the rows of its compressed chunks. \
             --schema-only copies its shape, or a raw dump where the environment allows one"
        ));
    }
    for t in catalog.tables.iter().filter(|t| t.has_rows()) {
        let q = t.qualified();
        let chain = rules_chain(catalog, rules, t);
        let preset = preset(&t.name);
        if let Some((false, what)) = preset {
            drops.insert(q.clone(), format!("{what}: no rows"));
        }
        if chain.iter().find_map(|tr| tr.rows) == Some(Rows::Drop) {
            drops.insert(q.clone(), "the rules file says rows = \"drop\"".into());
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
            if let Some(r) = chain.iter().find_map(|tr| tr.rule_for(&c.name)) {
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
    // A CHECK bounding a column's length is a length too: text with
    // char_length(name) <= 63 fits its masked values in 63.
    for k in catalog.checks.iter().filter(|k| k.columns.len() == 1) {
        if let Some(n) = length_bound(&k.definition, &k.columns[0]) {
            let root = groups.find(&(k.table.clone(), k.columns[0].clone()));
            let e = fit_of.entry(root).or_insert(n);
            *e = (*e).min(n);
        }
    }
    // What a joined group must keep as one: unique if any member is (so
    // every member's fake carries the same suffix), NOT NULL if any is (so
    // a NULLed group is hashed, or the referencing side's empty value
    // references nothing), and its integer types (a hash differs by them).
    #[derive(Default)]
    struct Joined {
        members: usize,
        unique: bool,
        not_null: bool,
        ints: Vec<String>,
    }
    let mut joined: HashMap<(String, String), Joined> = HashMap::new();
    for t in catalog.tables.iter().filter(|t| t.has_rows()) {
        for c in t.columns.iter().filter(|c| !c.generated) {
            let g = joined
                .entry(groups.find(&(t.qualified(), c.name.clone())))
                .or_default();
            g.members += 1;
            g.unique |= c.unique;
            g.not_null |= c.ty.not_null;
            if matches!(c.ty.base.as_str(), "int2" | "int4" | "int8")
                && !g.ints.contains(&c.ty.base)
            {
                g.ints.push(c.ty.base.clone());
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
            let root = groups.find(&key);
            let fit = fit_of.get(&root).copied();
            let g = &joined[&root];
            let unique = c.unique || g.unique;
            // NULL where it must stay unique and can't be NULL — or where a
            // join can't be NULL: a hash, so every row still differs and
            // every reference still finds its row.
            if rule == Rule::Null
                && hashable(&c.ty.base)
                && c.ty.array_of.is_none()
                && ((c.unique && c.ty.not_null) || (g.members > 1 && g.not_null))
            {
                rule = Rule::Hash;
            }
            // After the hash it may have become: that must fit too.
            check(t, c, &rule, &origin, unique, fit, &mut problems);
            if rule == Rule::Hash && g.ints.len() > 1 {
                problems.push(format!(
                    "{q}.{} is joined to columns of other integer types ({}), and a hash comes \
                     out differently in each: keep it, or give the columns one type",
                    c.name,
                    g.ints.join(", ")
                ));
            }
            columns.push(Decision {
                column: c.name.clone(),
                rule,
                origin,
                fit,
                not_null: c.ty.not_null,
                unique,
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
    // A restore loads every row past the table's CHECK constraints, so a
    // masked value that may fail one stops it part-way. Which values pass
    // can't be told from the catalog: name each and let the team decide.
    for k in &catalog.checks {
        let Some(tp) = tables.iter().find(|t| t.table == k.table && !t.drop_rows) else {
            continue;
        };
        for d in tp.columns.iter().filter(|d| k.columns.contains(&d.column)) {
            let what = match &d.rule {
                // Made to look like the real thing, or the team's own.
                Rule::Keep
                | Rule::Fixed(_)
                | Rule::Email
                | Rule::Name(_)
                | Rule::Phone
                | Rule::Ip => continue,
                // NULL passes a CHECK that doesn't ask for a value.
                Rule::Null if !d.not_null => continue,
                Rule::Null => "the empty value standing in for NULL".to_string(),
                r => r.name().to_string(),
            };
            if masked_shape(d).is_some_and(|v| sure_to_pass(&k.definition, &d.column, &v)) {
                continue;
            }
            let mut def = k.definition.clone();
            if def.chars().count() > 120 {
                def = def.chars().take(119).collect::<String>() + "…";
            }
            problems.push(format!(
                "{}.{}: {} may fail its CHECK constraint {}, {def}, and stop the restore: \
                 keep it with a reason, or give it a fixed value that passes",
                tp.table, d.column, what, k.name
            ));
        }
    }
    Plan {
        tables,
        large_objects: rules.large_objects_kept,
        problems,
    }
}

/// A value of the shape `d`'s rule makes, where a CHECK can only look at
/// its shape: the empty value standing in for NULL, or a text hash (hex
/// digits, as long as the column's fit). Pure.
fn masked_shape(d: &Decision) -> Option<String> {
    match d.rule {
        // Where its COPY text is what the CHECK sees: not `f` for false.
        Rule::Null if d.array || !matches!(d.base.as_str(), "bool" | "interval" | "money") => {
            empty_value(&d.base, d.enum_first.as_deref(), d.array)
        }
        // Cut to the fit after it's made (16 digits at least only where
        // it must stay unique, which check() says).
        Rule::Hash if text_like(&d.base) && !d.array => {
            Some("0".repeat(d.fit.unwrap_or(64).min(64)))
        }
        _ => None,
    }
}

/// Whether `value` in `column` passes the CHECK `definition` for certain,
/// from the few one-clause forms whose answer needs no evaluating: an
/// upper bound on its length or cardinality, and a JSON object's type.
/// Anything else may fail. Pure.
pub fn sure_to_pass(definition: &str, column: &str, value: &str) -> bool {
    if let Some(n) = length_bound(definition, column) {
        return value.chars().count() <= n;
    }
    // An empty array has no elements, and `{}` is an object.
    let Some(clause) = definition
        .strip_prefix("CHECK ((")
        .and_then(|d| d.strip_suffix("))"))
    else {
        return false;
    };
    let on = |f: &str| {
        [column.to_string(), format!("\"{column}\"")]
            .iter()
            .find_map(|c| clause.strip_prefix(&format!("{f}({c}) ")))
    };
    if let Some(rest) = on("cardinality") {
        return value == "{}"
            && rest
                .strip_prefix("<= ")
                .is_some_and(|n| n.parse::<usize>().is_ok());
    }
    if let Some(rest) = on("octet_length") {
        return rest
            .strip_prefix("<= ")
            .and_then(|n| n.parse::<usize>().ok())
            .is_some_and(|n| value.len() <= n);
    }
    on("jsonb_typeof").is_some_and(|rest| value == "{}" && rest == "= 'object'::text")
}

/// The most characters a CHECK `definition` of one clause lets `column`
/// hold, if it is an upper bound on its `char_length` or `length`. Pure.
pub fn length_bound(definition: &str, column: &str) -> Option<usize> {
    let clause = definition.strip_prefix("CHECK ((")?.strip_suffix("))")?;
    let rest = ["char_length(", "length("]
        .iter()
        .find_map(|f| clause.strip_prefix(f))?;
    let (arg, bound) = rest.split_once(") ")?;
    let arg = arg.strip_suffix("::text").unwrap_or(arg);
    let arg = arg
        .strip_prefix('(')
        .and_then(|a| a.strip_suffix(')'))
        .unwrap_or(arg);
    if arg != column && arg != format!("\"{column}\"") {
        return None;
    }
    match bound.split_once(' ')? {
        ("<=", n) => n.parse().ok(),
        ("<", n) => n.parse::<usize>().ok()?.checked_sub(1),
        _ => None,
    }
}

/// What `rule` can't do to `c` of `t`, added to `problems`. Pure.
fn hashable(base: &str) -> bool {
    text_like(base)
        || matches!(
            base,
            "int2" | "int4" | "int8" | "uuid" | "numeric" | "bytea"
        )
}

/// What `rule` can't do to `c` of `t` — which must stay `unique` (it, or
/// a column joined to it) and fit `fit` characters — added to
/// `problems`. Pure.
fn check(
    t: &Table,
    c: &Column,
    rule: &Rule,
    origin: &Origin,
    unique: bool,
    fit: Option<usize>,
    problems: &mut Vec<String>,
) {
    let at = format!("{}.{}", t.qualified(), c.name);
    let base = c.ty.base.as_str();
    // The shortest a unique masked value can be and still differ from
    // every other: a hash's 16 hex digits, a name's 8-digit tag after a
    // letter, an email's tag@example.test, an IPv6 documentation address.
    let shortest = match rule {
        Rule::Hash if text_like(base) => Some(16),
        Rule::Name(_) => Some(10),
        Rule::Email => Some(29),
        Rule::Ip if text_like(base) => Some(29),
        _ => None,
    };
    if let (true, Some(need), Some(n)) = (unique, shortest, fit) {
        if n < need {
            problems.push(format!(
                "{at} must stay unique and takes {n} characters; {} needs {need} to keep values \
                 apart: keep it, or let it hold more (its type, or a CHECK on its length)",
                rule.name()
            ));
        }
    }
    if c.ty.array_of.is_some() && !matches!(rule, Rule::Keep | Rule::Null | Rule::Fixed(_)) {
        problems.push(format!(
            "{at} is an array: {} makes one value, not an array of them. Null it, or keep it",
            rule.name()
        ));
    }
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
    if *rule == Rule::Hash && !hashable(base) {
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
    use super::super::catalog::{Check, ColumnType, ForeignKey};
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
        // Postgres folds an unquoted dateOfBirth to dateofbirth.
        assert_eq!(
            words("customer_dateofbirth"),
            ["customer", "date", "of", "birth"]
        );
        assert_eq!(words("payee_banksortcode"), ["payee", "bank", "sortcode"]);
        assert_eq!(
            words("sms_senttomobilenumber"),
            ["sms", "sentto", "mobile", "number"]
        );
        // Known words stay whole; words with nothing pgman knows in them too.
        assert_eq!(words("ipaddress"), ["ipaddress"]);
        assert_eq!(words("cancellation"), ["cancellation"]);
        assert_eq!(words("warehouse"), ["warehouse"]);
        assert_eq!(words("company"), ["company"]);
    }

    #[test]
    fn folded_camel_case_names_are_judged_by_their_words() {
        for (name, base, want) in [
            ("customer_dateofbirth", "date", Rule::Year),
            ("customer_placeofbirth", "varchar", Rule::Null),
            ("member_mobilenumber", "varchar", Rule::Phone),
            ("member_nextofkintelephone", "varchar", Rule::Phone),
            ("member_emailaddress", "varchar", Rule::Email),
            (
                "member_passwordhash",
                "varchar",
                Rule::Password(String::new()),
            ),
            ("customer_socialsecuritynumber", "varchar", Rule::Null),
            ("customer_sexualorientation", "varchar", Rule::Null),
            ("customer_criminalrecord", "bool", Rule::Null),
            ("payee_banksortcode", "varchar", Rule::Null),
            ("payee_bankaccountnumber", "varchar", Rule::Null),
            ("payee_passportnumber", "varchar", Rule::Null),
            ("customer_basepostcode", "varchar", Rule::Postcode),
            ("ticket_staffcomment", "varchar", Rule::Null),
            ("order_customercomments", "varchar", Rule::Null),
            ("job_workdescription", "varchar", Rule::Null),
            // Not personal, and not made so by a word inside them.
            ("order_cancellationreasoncode", "varchar", Rule::Keep),
            ("order_shippingmethod", "varchar", Rule::Keep),
            ("warehouse_company", "varchar", Rule::Keep),
            ("member_emailenabled", "bool", Rule::Keep),
            ("member_contactbyemail", "bool", Rule::Keep),
            ("document_contenttype", "varchar", Rule::Keep),
            ("sms_messagetype", "varchar", Rule::Keep),
            ("member_receivesmessages", "bool", Rule::Keep),
            ("chatmessage_createdon", "timestamp", Rule::Keep),
            ("ticket_customercommentson", "timestamp", Rule::Keep),
            ("notificationmessagetype_desc", "varchar", Rule::Keep),
            ("integrations_note_events", "bool", Rule::Keep),
            ("payment_failure_reason", "int2", Rule::Keep),
        ] {
            assert_eq!(rule("t", name, base, Some(64)), want, "{name}");
        }
    }

    #[test]
    fn a_phrase_matches_its_words_run_together() {
        let w = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert!(has(&w("bank accountnumber"), "account number"));
        assert!(has(&w("bank account number"), "account number"));
        assert!(!has(&w("accountnumbers"), "account number"));
        assert!(!has(&w("account"), "account number"));
    }

    #[test]
    fn the_rules_files_words_match_a_name_as_written() {
        // codeversion is code|version to pgman, kept unless the file says.
        let col = c("release_codeversion", "varchar", Some(64));
        assert_eq!(default_rule("t", &col, &[], &[]).0, Rule::Keep);
        let (r, _) = default_rule("t", &col, &["codeversion".to_string()], &[]);
        assert_eq!(r, Rule::Null);
        let (r, _) = default_rule("t", &col, &["code version".to_string()], &[]);
        assert_eq!(r, Rule::Null);
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
    fn personal_words_from_open_source_schemas() {
        for (table, name, base, want) in [
            ("hr_employee", "identification_id", "varchar", Rule::Null),
            ("employee", "visa_no", "varchar", Rule::Null),
            (
                "employee",
                "work_permit_expiration_date",
                "date",
                Rule::Null,
            ),
            ("res_partner_bank", "acc_number", "varchar", Rule::Null),
            ("salesperson", "bonus", "numeric", Rule::Null),
            ("salesperson", "commission_pct", "numeric", Rule::Null),
            ("user_details", "linkedin", "varchar", Rule::Null),
            ("oauth_device_grants", "user_code", "varchar", Rule::Null),
            ("settings", "aws_access_key", "varchar", Rule::Null),
            ("employee", "sick_leave_hours", "int2", Rule::Null),
            ("user_details", "pronouns", "varchar", Rule::Null),
            ("employee", "login_id", "varchar", Rule::Email),
            ("scim_identities", "extern_uid", "varchar", Rule::Hash),
            ("user_external_ids", "external_id", "varchar", Rule::Hash),
            (
                "product_review",
                "reviewer_name",
                "varchar",
                Rule::Name(NamePart::Full),
            ),
            ("res_partner", "name", "varchar", Rule::Name(NamePart::Full)),
            ("mail_message", "reply_to", "varchar", Rule::Email),
            ("sms_resend", "sms_number", "varchar", Rule::Phone),
            ("device_log", "browser", "varchar", Rule::Null),
            ("tls_sessions", "client_ja3", "varchar", Rule::Null),
            ("cards", "card_last4", "varchar", Rule::Null),
            ("logins", "ipv4", "varchar", Rule::Ip),
            ("card_validations", "holder_name_hash", "text", Rule::Hash),
            ("card_validations", "last_digits_hash", "text", Rule::Hash),
            ("users", "email_digest", "bytea", Rule::Hash),
            // A hash of what isn't personal, or a file's, is still kept.
            ("uploads", "content_hash", "varchar", Rule::Keep),
            ("attributes", "long_value_hash", "bytea", Rule::Keep),
            ("lfs_objects", "file_sha_hash", "varchar", Rule::Keep),
        ] {
            assert_eq!(rule(table, name, base, Some(255)), want, "{table}.{name}");
        }
    }

    #[test]
    fn settings_about_personal_things_are_kept() {
        for (name, base, want) in [
            ("emails_disabled", "bool", Rule::Keep),
            ("mentions_disabled", "bool", Rule::Keep),
            ("login_theme", "varchar", Rule::Keep),
            ("email_primary_color", "varchar", Rule::Keep),
            ("mobile_view_mode", "varchar", Rule::Keep),
            ("require_signature", "bool", Rule::Keep),
            ("eye_colour", "text", Rule::Null),
            ("skin_color", "varchar", Rule::Null),
            ("hair_colour", "varchar", Rule::Null),
            ("skin_tone", "varchar", Rule::Null),
            ("mail_subject", "varchar", Rule::Null),
            ("email_body", "varchar", Rule::Null),
            // Still the thing itself.
            ("disability", "varchar", Rule::Null),
            ("disabled", "bool", Rule::Null),
            ("disabled_person", "varchar", Rule::Null),
            ("registered_disabled", "bool", Rule::Null),
            ("health_disabled", "bool", Rule::Null),
            ("signature", "varchar", Rule::Null),
            ("contact_email", "varchar", Rule::Email),
        ] {
            assert_eq!(rule("settings", name, base, Some(255)), want, "{name}");
        }
    }

    #[test]
    fn the_rules_files_words_reach_a_hash() {
        let plain = ["digest".to_string()];
        let c = c("email_digest", "bytea", None);
        assert_eq!(default_rule("t", &c, &[], &[]).0, Rule::Hash);
        assert_eq!(default_rule("t", &c, &[], &plain).0, Rule::Keep);
        let personal = ["digest".to_string()];
        let weekly = super::tests::c("weekly_digest", "varchar", Some(255));
        assert_eq!(default_rule("t", &weekly, &personal, &[]).0, Rule::Null);
        let named = ["content hash".to_string()];
        let content = super::tests::c("content_hash", "varchar", Some(255));
        assert_eq!(default_rule("t", &content, &named, &[]).0, Rule::Null);
        // A digest that isn't of something personal is judged as any column.
        let activity = super::tests::c("activity_digest", "text", None);
        assert_eq!(default_rule("t", &activity, &[], &[]).0, Rule::Null);
        let sent = super::tests::c("activity_digest", "varchar", Some(255));
        assert_eq!(default_rule("t", &sent, &[], &[]).0, Rule::Keep);
    }

    #[test]
    fn a_date_or_flag_about_a_secret_is_kept() {
        for (name, base) in [
            ("password_changed_at", "timestamptz"),
            ("user_passwordlastchanged", "timestamp"),
            ("token_expires", "timestamptz"),
            ("password_reset_required", "bool"),
            ("api_key_rotated_on", "date"),
        ] {
            assert_eq!(rule("u", name, base, None), Rule::Keep, "{name}");
        }
        // The secret itself, in any other type, is still masked.
        assert_eq!(rule("u", "password", "bytea", None), Rule::Null);
        assert_eq!(rule("u", "session_token", "uuid", None), Rule::Null);
        assert_eq!(rule("u", "pin", "int4", None), Rule::Null);
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
        for table in ["app_user", "tbl_customer", "UserProfile", "user_account"] {
            assert_eq!(
                rule(table, "name", "varchar", Some(255)),
                Rule::Name(NamePart::Full),
                "{table}"
            );
        }
        assert_eq!(rule("user_role", "name", "varchar", Some(64)), Rule::Keep);
    }

    #[test]
    fn a_hypertable_stops_the_plan() {
        let cat = Catalog {
            tables: vec![table("metrics", vec![c("v", "float8", None)])],
            hypertables: vec!["public.metrics".into()],
            ..Default::default()
        };
        let p = plan(&cat, &RuleSet::default());
        assert_eq!(p.problems.len(), 1, "{:?}", p.problems);
        assert!(
            p.problems[0].starts_with("public.metrics is a TimescaleDB hypertable"),
            "{:?}",
            p.problems
        );
    }

    #[test]
    fn a_check_only_an_upper_bound_or_an_objects_type_is_sure_to_pass() {
        let hash = "0".repeat(64);
        for (def, value) in [
            ("CHECK ((char_length(title) <= 255))", ""),
            ("CHECK ((char_length(title) <= 255))", hash.as_str()),
            ("CHECK ((char_length(title) < 65))", hash.as_str()),
            ("CHECK ((length((title)::text) <= 64))", hash.as_str()),
            ("CHECK ((octet_length(title) <= 1024))", ""),
            ("CHECK ((char_length(\"title\") <= 10))", ""),
            ("CHECK ((cardinality(title) <= 100))", "{}"),
            ("CHECK ((jsonb_typeof(title) = 'object'::text))", "{}"),
        ] {
            assert!(sure_to_pass(def, "title", value), "{def} {value:?}");
        }
        for (def, value) in [
            // Too long for it, or a lower bound, or another column.
            ("CHECK ((char_length(title) <= 32))", hash.as_str()),
            ("CHECK ((char_length(title) < 64))", hash.as_str()),
            ("CHECK ((char_length(title) >= 1))", ""),
            ("CHECK ((char_length(other) <= 255))", ""),
            ("CHECK ((octet_length(title) = 20))", ""),
            // More than one clause, or one pgman doesn't read.
            (
                "CHECK (((char_length(title) <= 255) AND (title <> ''::text)))",
                "",
            ),
            ("CHECK ((jsonb_typeof(title) = 'array'::text))", "{}"),
            ("CHECK ((title > (0)::numeric))", "0"),
            ("CHECK ((cardinality(title) <= 100))", ""),
            ("CHECK ((title)::text !~~ '% %'::text)", ""),
        ] {
            assert!(!sure_to_pass(def, "title", value), "{def} {value:?}");
        }
    }

    #[test]
    fn a_hash_promoted_onto_a_short_unique_column_is_a_problem() {
        let short = |base: &str, len: Option<usize>, check: Option<&str>| Catalog {
            tables: vec![table(
                "people",
                vec![Column {
                    unique: true,
                    ty: ty(base, len, true),
                    ..c("ssn", base, len)
                }],
            )],
            checks: check
                .map(|d| Check {
                    table: "public.people".into(),
                    name: "c_ssn".into(),
                    columns: vec!["ssn".into()],
                    definition: d.into(),
                })
                .into_iter()
                .collect(),
            ..Default::default()
        };
        // NULL can't keep it unique, so it's hashed, and 6 can't hold one.
        let p = plan(&short("varchar", Some(6), None), &RuleSet::default());
        assert_eq!(p.tables[0].columns[0].rule, Rule::Hash);
        assert_eq!(p.problems.len(), 1, "{:?}", p.problems);
        assert!(
            p.problems[0].contains("must stay unique and takes 6"),
            "{:?}",
            p.problems
        );
        // A CHECK bounding it says the same, once.
        let p = plan(
            &short("text", None, Some("CHECK ((char_length(ssn) <= 12))")),
            &RuleSet::default(),
        );
        assert_eq!(p.problems.len(), 1, "{:?}", p.problems);
        assert!(
            p.problems[0].contains("must stay unique and takes 12"),
            "{:?}",
            p.problems
        );
        // Not unique, a hash is cut to fit the CHECK and passes it.
        let mut cat = short("text", None, Some("CHECK ((char_length(ssn) <= 8))"));
        cat.tables[0].columns[0].unique = false;
        let hashed = parse("[tables.people]\nssn = \"hash\"\n");
        let p = plan(&cat, &hashed);
        assert!(p.problems.is_empty(), "{:?}", p.problems);
    }

    #[test]
    fn a_check_on_a_columns_length_bounds_it() {
        let b = |def: &str| length_bound(def, "name");
        assert_eq!(b("CHECK ((char_length(name) <= 63))"), Some(63));
        assert_eq!(b("CHECK ((char_length(name) < 64))"), Some(63));
        assert_eq!(b("CHECK ((length((name)::text) <= 20))"), Some(20));
        assert_eq!(b("CHECK ((char_length(\"name\") <= 8))"), Some(8));
        assert_eq!(b("CHECK ((char_length(name) >= 1))"), None);
        assert_eq!(b("CHECK ((char_length(other) <= 63))"), None);
        assert_eq!(b("CHECK ((octet_length(name) = 20))"), None);
        assert_eq!(
            b("CHECK (((char_length(name) <= 63) AND (name <> ''::text)))"),
            None
        );
        // So a hash of it fits, and stops nothing.
        let cat = Catalog {
            tables: vec![table(
                "buckets",
                vec![Column {
                    unique: true,
                    ..c("customer_ref", "text", None)
                }],
            )],
            checks: vec![Check {
                table: "public.buckets".into(),
                name: "c_len".into(),
                columns: vec!["customer_ref".into()],
                definition: "CHECK ((char_length(customer_ref) <= 40))".into(),
            }],
            ..Default::default()
        };
        let p = plan(&cat, &RuleSet::default());
        let d = &p.tables[0].columns[0];
        assert_eq!(
            (&d.rule, d.fit),
            (&Rule::Hash, Some(40)),
            "{:?}",
            p.problems
        );
        assert!(p.problems.is_empty(), "{:?}", p.problems);
    }

    #[test]
    fn a_masked_value_under_a_check_constraint_is_a_problem() {
        let not_null = |name: &str, base: &str| Column {
            ty: ty(base, None, true),
            ..c(name, base, None)
        };
        let cat = |cols: Vec<Column>, check_cols: &[&str]| Catalog {
            tables: vec![table("pay_rates", cols)],
            checks: vec![Check {
                table: "public.pay_rates".into(),
                name: "c_rate".into(),
                columns: check_cols.iter().map(|s| s.to_string()).collect(),
                definition: "CHECK ((wage > (0)::numeric))".into(),
            }],
            ..Default::default()
        };
        // wage is pay, nulled; NOT NULL makes it 0, which fails wage > 0.
        let p = plan(
            &cat(vec![not_null("wage", "numeric")], &["wage"]),
            &RuleSet::default(),
        );
        assert_eq!(p.problems.len(), 1, "{:?}", p.problems);
        assert!(
            p.problems[0].starts_with("public.pay_rates.wage: the empty value standing in for NULL may fail its CHECK constraint c_rate, CHECK ((wage > (0)::numeric))"),
            "{:?}",
            p.problems
        );
        // NULL itself passes it.
        let p = plan(
            &cat(vec![c("wage", "numeric", None)], &["wage"]),
            &RuleSet::default(),
        );
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        // A column the constraint doesn't name, or one kept, isn't one.
        let p = plan(
            &cat(vec![not_null("wage", "numeric")], &["other"]),
            &RuleSet::default(),
        );
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        let kept =
            parse("[tables.pay_rates]\nwage = { rule = \"keep\", reason = \"published rates\" }\n");
        let p = plan(&cat(vec![not_null("wage", "numeric")], &["wage"]), &kept);
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        // A realistic fake is left to pass.
        let p = plan(
            &cat(vec![not_null("email", "varchar")], &["email"]),
            &RuleSet::default(),
        );
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        // No rows, nothing to fail.
        let dropped = parse("[tables.pay_rates]\nrows = \"drop\"\n");
        let p = plan(&cat(vec![not_null("wage", "numeric")], &["wage"]), &dropped);
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        // An empty string under an upper bound on its length passes.
        let bounded = |def: &str| Catalog {
            checks: vec![Check {
                definition: def.into(),
                ..cat(vec![], &["notes"]).checks[0].clone()
            }],
            ..cat(vec![not_null("notes", "text")], &["notes"])
        };
        let p = plan(
            &bounded("CHECK ((char_length(notes) <= 255))"),
            &RuleSet::default(),
        );
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        let p = plan(
            &bounded("CHECK ((char_length(notes) >= 1))"),
            &RuleSet::default(),
        );
        assert_eq!(p.problems.len(), 1, "{:?}", p.problems);
    }

    #[test]
    fn an_array_of_personal_values_is_nulled_not_transformed() {
        let mut emails = c("emails", "varchar", Some(100));
        emails.ty.array_of = Some("varchar".into());
        let (rule, why) = default_rule("t", &emails, &[], &[]);
        assert_eq!(rule, Rule::Null);
        assert_eq!(
            why,
            Origin::Default("an email address, by name, in an array".into())
        );
        let mut tags = c("tags", "varchar", Some(100));
        tags.ty.array_of = Some("varchar".into());
        assert_eq!(default_rule("t", &tags, &[], &[]).0, Rule::Keep);
        // And the rules file can't ask for one.
        let cat = Catalog {
            tables: vec![table("people", vec![emails])],
            large_objects: 0,
            ..Default::default()
        };
        let p = plan(
            &cat,
            &parse("version = 1\n[tables.people]\nemails = \"email\"\n"),
        );
        assert!(
            p.problems
                .iter()
                .any(|m| m.contains("public.people.emails is an array")),
            "{:?}",
            p.problems
        );
    }

    #[test]
    fn whose_name_and_the_usual_abbreviations() {
        for (name, want) in [
            ("employee_name", Rule::Name(NamePart::Full)),
            ("patient_name", Rule::Name(NamePart::Full)),
            ("account_holder_name", Rule::Name(NamePart::Full)),
            ("cardholder_name", Rule::Name(NamePart::Full)),
            ("billing_name", Rule::Name(NamePart::Full)),
            ("fname", Rule::Name(NamePart::First)),
            ("lname", Rule::Name(NamePart::Last)),
            ("middlename", Rule::Name(NamePart::Full)),
            ("initials", Rule::Name(NamePart::Full)),
            ("mob", Rule::Phone),
            ("addr1", Rule::Null),
            ("born_on", Rule::Null),
            ("next_of_kin", Rule::Null),
            ("emergency_contact", Rule::Null),
            ("payload", Rule::Null),
            ("line1", Rule::Null),
            ("line_2", Rule::Null),
            ("addressLine3", Rule::Null),
            ("subject", Rule::Null),
        ] {
            assert_eq!(rule("t", name, "varchar", Some(255)), want, "{name}");
        }
        for name in ["product_name", "company_name", "file_name"] {
            assert_eq!(rule("t", name, "varchar", Some(255)), Rule::Keep, "{name}");
        }
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
            parent: None,
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
            ..Default::default()
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
    fn framework_tables_have_presets() {
        for name in [
            "schema_migrations",
            "ar_internal_metadata",
            "django_migrations",
            "alembic_version",
            "_prisma_migrations",
        ] {
            assert!(matches!(preset(name), Some((true, _))), "{name}");
        }
        assert!(matches!(preset("django_session"), Some((false, _))));
        assert_eq!(preset("migrations"), None);
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
            ..Default::default()
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
            ..Default::default()
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

    fn joined(parent: (&str, Column), child: (&str, Column)) -> Catalog {
        let (pcol, ccol) = (parent.1.name.clone(), child.1.name.clone());
        let mut child_t = table(child.0, vec![c("id", "int4", None), child.1]);
        child_t.foreign_keys.push(ForeignKey {
            columns: vec![ccol],
            ref_table: format!("public.{}", parent.0),
            ref_columns: vec![pcol],
        });
        Catalog {
            tables: vec![
                table(parent.0, vec![c("id", "int4", None), parent.1]),
                child_t,
            ],
            large_objects: 0,
            ..Default::default()
        }
    }

    #[test]
    fn a_joined_group_is_unique_and_not_null_together() {
        // A unique username and the posts that reference it: both carry
        // the suffix, or no post finds its user.
        let mut username = c("username", "varchar", Some(100));
        username.unique = true;
        let cat = joined(
            ("users", username),
            ("posts", c("username", "varchar", Some(100))),
        );
        let p = plan(&cat, &RuleSet::default());
        let (u, f) = (
            &p.table("public.users").unwrap().columns[1],
            &p.table("public.posts").unwrap().columns[1],
        );
        assert_eq!((u.unique, f.unique), (true, true));
        // A NULLed key that can't be NULL on the referencing side: both
        // hashed, so each reference still finds its row.
        let mut nino = c("nino", "varchar", Some(9));
        nino.unique = true;
        nino.ty.not_null = true;
        let mut claim = c("nino", "varchar", Some(9));
        claim.ty.not_null = true;
        let p = plan(
            &joined(("people", nino), ("claims", claim)),
            &RuleSet::default(),
        );
        assert_eq!(
            p.table("public.people").unwrap().columns[1].rule,
            Rule::Hash
        );
        assert_eq!(
            p.table("public.claims").unwrap().columns[1].rule,
            Rule::Hash
        );
        // Nullable on both sides: NULL is fine.
        let mut nino = c("nino", "varchar", Some(9));
        nino.unique = true;
        let p = plan(
            &joined(("people", nino), ("claims", c("nino", "varchar", Some(9)))),
            &RuleSet::default(),
        );
        assert_eq!(
            p.table("public.claims").unwrap().columns[1].rule,
            Rule::Null
        );
    }

    #[test]
    fn a_unique_column_too_short_to_keep_values_apart_is_a_problem() {
        let mut email = c("email", "varchar", Some(20));
        email.unique = true;
        let cat = Catalog {
            tables: vec![table("users", vec![c("id", "int4", None), email])],
            large_objects: 0,
            ..Default::default()
        };
        let p = plan(&cat, &RuleSet::default());
        assert!(
            p.problems
                .iter()
                .any(|m| m.contains("public.users.email must stay unique and takes 20 characters")),
            "{:?}",
            p.problems
        );
        // Long enough: no problem.
        let mut email = c("email", "varchar", Some(100));
        email.unique = true;
        let cat = Catalog {
            tables: vec![table("users", vec![c("id", "int4", None), email])],
            large_objects: 0,
            ..Default::default()
        };
        assert!(plan(&cat, &RuleSet::default()).problems.is_empty());
    }

    #[test]
    fn a_hash_across_integer_types_is_a_problem() {
        let cat = joined(
            ("accounts", c("legacy_no", "int8", None)),
            ("ledger", c("legacy_no", "int4", None)),
        );
        let p = plan(
            &cat,
            &parse("version = 1\n[tables.accounts]\nlegacy_no = \"hash\"\n"),
        );
        assert!(
            p.problems
                .iter()
                .any(|m| m.contains("other integer types (int4, int8)")
                    || m.contains("other integer types (int8, int4)")),
            "{:?}",
            p.problems
        );
    }

    #[test]
    fn the_rules_reach_partitions_children_and_audit_tables() {
        let mut parent = table(
            "orders",
            vec![c("id", "int4", None), c("free_field", "varchar", Some(40))],
        );
        parent.kind = "p".into();
        let mut part = table(
            "orders_2026",
            vec![c("id", "int4", None), c("free_field", "varchar", Some(40))],
        );
        part.parent = Some("public.orders".into());
        let customer = table(
            "customer",
            vec![c("id", "int4", None), c("email", "varchar", Some(100))],
        );
        let aud = table(
            "customer_aud",
            vec![c("id", "int4", None), c("email", "varchar", Some(100))],
        );
        let cat = Catalog {
            tables: vec![parent, part, customer, aud],
            large_objects: 0,
            ..Default::default()
        };
        let p = plan(
            &cat,
            &parse(
                "version = 1\n[tables.orders]\nfree_field = \"null\"\n\
                 [tables.customer]\nemail = \"hash\"\n",
            ),
        );
        let rule = |t: &str, col: &str| {
            p.table(t)
                .unwrap()
                .columns
                .iter()
                .find(|d| d.column == col)
                .unwrap()
                .rule
                .clone()
        };
        assert_eq!(rule("public.orders_2026", "free_field"), Rule::Null);
        assert_eq!(
            rule("public.customer_aud", "email"),
            Rule::Hash,
            "as its base"
        );
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        // The child's own block wins over its parent's.
        let p = plan(
            &cat,
            &parse(
                "version = 1\n[tables.orders]\nfree_field = \"null\"\n\
                 [tables.orders_2026]\nfree_field = { rule = \"fixed\", value = \"x\" }\n",
            ),
        );
        assert_eq!(
            p.table("public.orders_2026").unwrap().columns[1].rule,
            Rule::Fixed("x".into())
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
            ..Default::default()
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
