//! The transformers: each column's value becomes its rule's output, keyed
//! by the dump's masking key so that the same value always becomes the
//! same output in one copy (joins still join) and nothing outside it.
//! The key is never sent to the server. Pure but for the key.
//!
//! Outputs keep their column's type and fit its length; a value that
//! isn't the shape a rule expects (an email with no `@`, a date that is
//! `infinity`) is still replaced, never passed through.

use super::classify::{empty_value, Decision, NamePart, Rule};
use ring::hmac;

/// The test password every masked account gets, and its bcrypt hash
/// (cost 10), made once with a standard bcrypt: pgman holds no bcrypt
/// of its own.
pub const TEST_PASSWORD: &str = "pgman-test";
const TEST_BCRYPT: &str = "$2a$10$vFkrQjFkBDB7WEUaqvRW1ekmEUUYMJ302H9GB.0/lfTtaERV1Y2Me";

/// The fake-name lists' version, recorded in each dump: a later pgman
/// with other lists says so rather than silently renaming everyone.
pub const WORDS_VERSION: u32 = 1;

const FIRST_NAMES: &[&str] = &[
    "Ava", "Ben", "Chloe", "Dev", "Ella", "Femi", "Grace", "Hamid", "Isla", "Jon", "Kemi", "Leo",
    "Maya", "Nico", "Olu", "Priya", "Quinn", "Rosa", "Sami", "Tara", "Uma", "Vik", "Wren", "Xavi",
    "Yara", "Zac", "Aisha", "Bea", "Cai", "Dara", "Emre", "Freya", "Gus", "Hana", "Ivo", "Jada",
    "Kit", "Lina", "Max", "Nell", "Omar", "Pia", "Ravi", "Sofia", "Theo", "Una", "Vera", "Will",
    "Yusuf", "Zoe", "Alex", "Blair", "Cam", "Drew", "Eli", "Frankie", "Jess", "Kai", "Lou",
    "Morgan", "Noor", "Rio", "Sky", "Toni",
];
const LAST_NAMES: &[&str] = &[
    "Adams",
    "Begum",
    "Clarke",
    "Dhillon",
    "Evans",
    "Fernandes",
    "Green",
    "Hughes",
    "Iqbal",
    "Jones",
    "Khan",
    "Lewis",
    "Mensah",
    "Nowak",
    "Okafor",
    "Patel",
    "Quinn",
    "Reid",
    "Singh",
    "Taylor",
    "Usman",
    "Vance",
    "Walsh",
    "Young",
    "Zhou",
    "Ali",
    "Brown",
    "Chen",
    "Davies",
    "Ellis",
    "Fox",
    "Gill",
    "Hall",
    "Ivanov",
    "Jackson",
    "Kaur",
    "Lopez",
    "Murphy",
    "Nash",
    "Owen",
    "Price",
    "Rossi",
    "Shah",
    "Thomas",
    "Ueda",
    "Vargas",
    "Wood",
    "Yilmaz",
    "Zielinski",
    "Ahmed",
    "Baker",
    "Cole",
    "Doyle",
    "Egan",
    "Ford",
    "Grant",
    "Hart",
    "Ito",
    "Kelly",
    "Moore",
    "Nguyen",
    "Obi",
    "Park",
    "Silva",
];

/// A masking key: 32 bytes, made fresh for each dump unless a stable one
/// is configured.
pub struct Masker {
    key: hmac::Key,
}

impl Masker {
    pub fn new(key: &[u8; 32]) -> Masker {
        Masker {
            key: hmac::Key::new(hmac::HMAC_SHA256, key),
        }
    }

    /// The keyed hash of `value` for `purpose`: the same everywhere in a
    /// copy for the same value and purpose (so a joined column comes out
    /// the same), unrelated otherwise.
    fn mac(&self, purpose: &str, value: &[u8]) -> [u8; 32] {
        let mut ctx = hmac::Context::with_key(&self.key);
        ctx.update(purpose.as_bytes());
        ctx.update(&[0]);
        ctx.update(value);
        let tag = ctx.sign();
        let mut out = [0u8; 32];
        out.copy_from_slice(tag.as_ref());
        out
    }

    /// What `value` (a field as COPY gave it; `None` is NULL) becomes
    /// under `d`. `row_key` is the row's primary key, for a date shift.
    pub fn apply(&self, d: &Decision, value: Option<&[u8]>, row_key: &[u8]) -> Option<Vec<u8>> {
        if d.rule == Rule::Null {
            return if d.not_null {
                empty_value(&d.base, d.enum_first.as_deref(), d.array).map(String::into_bytes)
            } else {
                None
            };
        }
        let v = value?;
        let text = String::from_utf8_lossy(v);
        let out: String = match &d.rule {
            Rule::Keep => return Some(v.to_vec()),
            Rule::Null => unreachable!("handled above"),
            Rule::Fixed(f) => f.clone(),
            Rule::Password(given) => password(given, &text),
            Rule::Hash => self.hash(d, v),
            Rule::Email => self.email(v, d.fit),
            Rule::Name(part) => self.name(*part, v, d.unique, d.fit),
            Rule::Phone => self.phone(&text, d.unique),
            Rule::Ip => self.ip(&text, v, d.unique),
            Rule::Postcode => postcode(&text),
            Rule::Year => year(&text),
            Rule::DateShift(days) => {
                let m = self.mac("date.shift", row_key);
                let span = u64::from(*days) * 2 + 1;
                let delta = (u64::from_le_bytes(m[..8].try_into().unwrap_or([0; 8])) % span) as i64
                    - i64::from(*days);
                shift(&text, delta)
            }
        };
        Some(fit(out, d.fit).into_bytes())
    }

    fn hex(&self, purpose: &str, v: &[u8], n: usize) -> String {
        self.mac(purpose, v)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()[..n.min(64)]
            .to_string()
    }

    /// `n` decimal digits from the hash.
    fn digits(&self, purpose: &str, v: &[u8], n: usize) -> String {
        self.mac(purpose, v)
            .iter()
            .flat_map(|b| [b % 10, (b / 10) % 10])
            .take(n)
            .map(|d| char::from(b'0' + d))
            .collect()
    }

    fn pick(&self, purpose: &str, v: &[u8], list: &[&str]) -> String {
        let m = self.mac(purpose, v);
        let n = u64::from_le_bytes(m[..8].try_into().unwrap_or([0; 8]));
        list[(n % list.len() as u64) as usize].to_string()
    }

    fn hash(&self, d: &Decision, v: &[u8]) -> String {
        let m = self.mac("hash", v);
        let n = u64::from_le_bytes(m[..8].try_into().unwrap_or([0; 8]));
        match d.base.as_str() {
            "int2" => (n % 32_767).to_string(),
            "int4" => (n % 2_147_483_647).to_string(),
            "int8" => (n % 9_223_372_036_854_775_807).to_string(),
            "numeric" => (n % 1_000_000_000_000_000).to_string(),
            "uuid" => {
                let mut b = [0u8; 16];
                b.copy_from_slice(&m[..16]);
                b[6] = (b[6] & 0x0f) | 0x40;
                b[8] = (b[8] & 0x3f) | 0x80;
                let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
                format!(
                    "{}-{}-{}-{}-{}",
                    &h[..8],
                    &h[8..12],
                    &h[12..16],
                    &h[16..20],
                    &h[20..]
                )
            }
            "bytea" => format!("\\x{}", self.hex("hash", v, 64)),
            _ => self.hex("hash", v, d.fit.unwrap_or(64).clamp(16, 64)),
        }
    }

    fn email(&self, v: &[u8], fit: Option<usize>) -> String {
        let first = self.pick("email.first", v, FIRST_NAMES).to_lowercase();
        let last = self.pick("email.last", v, LAST_NAMES).to_lowercase();
        let tag = self.hex("email", v, 16);
        let full = format!("{first}.{last}.{tag}@example.test");
        match fit {
            Some(n) if full.len() > n => format!("{tag}@example.test"),
            _ => full,
        }
    }

    /// A fake name; where the column must stay unique, with a tag from
    /// the hash after it, the name shortened (never the tag) to `fit`.
    fn name(&self, part: NamePart, v: &[u8], unique: bool, fit: Option<usize>) -> String {
        let first = self.pick("name.first", v, FIRST_NAMES);
        let last = self.pick("name.last", v, LAST_NAMES);
        let base = match part {
            NamePart::First => first,
            NamePart::Last => last,
            NamePart::Full => format!("{first} {last}"),
        };
        if unique {
            let room = fit.map_or(usize::MAX, |n| n.saturating_sub(9).max(1));
            let base: String = base.chars().take(room).collect();
            format!("{} {}", base.trim_end(), self.hex("name", v, 8))
        } else {
            base
        }
    }

    /// The input's shape — its `+`, spaces and punctuation, its length,
    /// its first two digits — with the other digits from the hash; a UK
    /// number in Ofcom's drama ranges where the column needn't be unique,
    /// so no masked number is anyone's. A value with letters in it
    /// ("mum: 07…", "ask for Alice") isn't a number's shape to keep: it
    /// becomes a bare UK mobile number.
    fn phone(&self, text: &str, unique: bool) -> String {
        if text.chars().any(char::is_alphabetic) {
            return if unique {
                format!("07{}", self.digits("phone", text.as_bytes(), 9))
            } else {
                format!("07700900{}", self.digits("phone", text.as_bytes(), 3))
            };
        }
        let digits: String = text.chars().filter(char::is_ascii_digit).collect();
        let m = self.mac("phone", text.as_bytes());
        let mut pool = m.iter().flat_map(|b| [b % 10, (b / 10) % 10]).cycle();
        let drama = if unique {
            None
        } else if digits.len() == 11 && digits.starts_with("07") {
            Some("07700900")
        } else if digits.len() == 12 && digits.starts_with("447") {
            Some("447700900")
        } else if digits.len() == 11 && (digits.starts_with("01") || digits.starts_with("02")) {
            Some("01632960")
        } else {
            None
        };
        let mut want: Vec<char> = match drama {
            Some(prefix) => prefix.chars().collect(),
            None => digits.chars().take(2).collect(),
        };
        while want.len() < digits.len() {
            want.push(char::from(b'0' + pool.next().unwrap_or(0)));
        }
        let mut next = want.into_iter();
        let out: String = text
            .chars()
            .map(|c| {
                if c.is_ascii_digit() {
                    next.next().unwrap_or('0')
                } else {
                    c
                }
            })
            .collect();
        if digits.is_empty() {
            "07700900000".to_string()
        } else {
            out
        }
    }

    /// An address in a documentation range, keeping a `/prefix`. IPv4's
    /// ranges hold 762 addresses, so where the column must stay unique a
    /// bare address becomes an IPv6 one in 2001:db8::/32.
    fn ip(&self, text: &str, v: &[u8], unique: bool) -> String {
        let m = self.mac("ip", v);
        let (addr, suffix) = match text.split_once('/') {
            Some((a, p)) => (a, format!("/{p}")),
            None => (text, String::new()),
        };
        if addr.contains(':') || (unique && suffix.is_empty()) {
            format!(
                "2001:db8:{:x}:{:x}::{:x}{suffix}",
                u16::from_le_bytes([m[0], m[1]]),
                u16::from_le_bytes([m[2], m[3]]),
                u16::from_le_bytes([m[4], m[5]]).max(1)
            )
        } else {
            let net = ["192.0.2", "198.51.100", "203.0.113"][usize::from(m[0]) % 3];
            format!("{net}.{}{suffix}", 1 + m[1] % 254)
        }
    }
}

/// The test password as `given`'s format: a bcrypt hash (with the
/// input's `$2a$`/`$2b$`/`$2y$`), Spring's `{bcrypt}` or `{noop}` prefix
/// kept as it was. A rules file's own `value` is used as it is written.
/// Pure.
pub fn password(given: &str, input: &str) -> String {
    if !given.is_empty() {
        return given.to_string();
    }
    let (prefix, rest) = match input.strip_prefix('{').and_then(|r| r.split_once('}')) {
        Some((id, rest)) => (format!("{{{id}}}"), rest),
        None => (String::new(), input),
    };
    if prefix == "{noop}" {
        return format!("{{noop}}{TEST_PASSWORD}");
    }
    let variant = ["$2a$", "$2b$", "$2y$"]
        .into_iter()
        .find(|v| rest.starts_with(v))
        .unwrap_or("$2a$");
    let hash = TEST_BCRYPT.replacen("$2a$", variant, 1);
    // Another algorithm's prefix (`{pbkdf2}`) says what the app reads:
    // the hash is bcrypt, so the prefix says bcrypt.
    let prefix = if prefix.is_empty() {
        prefix
    } else {
        "{bcrypt}".to_string()
    };
    format!("{prefix}{hash}")
}

/// A postcode's district: what comes before the space, else all but the
/// last three characters. Pure.
pub fn postcode(text: &str) -> String {
    let t = text.trim().to_uppercase();
    match t.split_once(' ') {
        Some((out, _)) => out.to_string(),
        None if t.chars().count() > 4 => t.chars().take(t.chars().count() - 3).collect(),
        None => t,
    }
}

/// A date's (or timestamp's) year, still of its type: `1987-06-05` is
/// `1987-01-01`. What isn't a date (`infinity`) is the epoch's year.
/// Pure.
pub fn year(text: &str) -> String {
    let y: String = text.chars().take(4).collect();
    let ok = y.len() == 4
        && y.chars().all(|c| c.is_ascii_digit())
        && text.as_bytes().get(4) == Some(&b'-');
    let y = if ok { y } else { "1970".to_string() };
    if text.len() > 10 {
        format!("{y}-01-01 00:00:00")
    } else {
        format!("{y}-01-01")
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `text` (a date, or a timestamp whose time is kept) moved by `delta`
/// days. What isn't one is the epoch. Pure.
pub fn shift(text: &str, delta: i64) -> String {
    let parsed = (|| {
        let y: i64 = text.get(0..4)?.parse().ok()?;
        let m: i64 = text.get(5..7)?.parse().ok()?;
        let d: i64 = text.get(8..10)?.parse().ok()?;
        (text.as_bytes().get(4) == Some(&b'-') && text.as_bytes().get(7) == Some(&b'-'))
            .then_some((y, m, d))
    })();
    let Some((y, m, d)) = parsed else {
        return if text.len() > 10 {
            "1970-01-01 00:00:00".into()
        } else {
            "1970-01-01".into()
        };
    };
    let (y, m, d) = civil_from_days(days_from_civil(y, m, d) + delta);
    format!("{y:04}-{m:02}-{d:02}{}", &text[10..])
}

/// `s` cut to `n` characters, when there is a limit. Pure.
fn fit(s: String, n: Option<usize>) -> String {
    match n {
        Some(n) if s.chars().count() > n => s.chars().take(n).collect(),
        _ => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(rule: Rule, base: &str) -> Decision {
        Decision {
            column: "c".into(),
            rule,
            origin: super::super::classify::Origin::Plain,
            fit: None,
            not_null: false,
            unique: false,
            base: base.into(),
            enum_first: None,
            array: false,
        }
    }

    fn m() -> Masker {
        Masker::new(&[7; 32])
    }

    fn run(dec: &Decision, v: &str) -> String {
        String::from_utf8(m().apply(dec, Some(v.as_bytes()), b"").unwrap()).unwrap()
    }

    #[test]
    fn the_same_value_masks_the_same_and_another_key_differently() {
        let e = d(Rule::Email, "varchar");
        assert_eq!(run(&e, "alice@example.com"), run(&e, "alice@example.com"));
        assert_ne!(run(&e, "alice@example.com"), run(&e, "bob@example.com"));
        let other = Masker::new(&[8; 32]);
        assert_ne!(
            other.apply(&e, Some(b"alice@example.com"), b""),
            m().apply(&e, Some(b"alice@example.com"), b"")
        );
    }

    #[test]
    fn emails_look_real_and_shorten_to_fit() {
        let mut e = d(Rule::Email, "varchar");
        let out = run(&e, "alice@example.com");
        assert!(
            out.ends_with("@example.test") && out.matches('.').count() >= 3,
            "{out}"
        );
        e.fit = Some(30);
        let short = run(&e, "alice@example.com");
        assert_eq!(short.len(), 29, "{short}");
        assert!(!short.contains("alice"));
    }

    #[test]
    fn nulls_become_empty_values_where_they_must() {
        let mut n = d(Rule::Null, "text");
        assert_eq!(m().apply(&n, Some(b"secret"), b""), None);
        n.not_null = true;
        assert_eq!(m().apply(&n, Some(b"secret"), b""), Some(Vec::new()));
        n.base = "jsonb".into();
        assert_eq!(m().apply(&n, Some(b"{\"a\":1}"), b""), Some(b"{}".to_vec()));
        // NULL in, NULL out, for every other rule.
        assert_eq!(m().apply(&d(Rule::Email, "text"), None, b""), None);
    }

    #[test]
    fn hashes_keep_their_type() {
        assert!(run(&d(Rule::Hash, "int4"), "42").parse::<i32>().is_ok());
        assert!(run(&d(Rule::Hash, "int2"), "42").parse::<i16>().is_ok());
        assert!(run(&d(Rule::Hash, "int8"), "42").parse::<i64>().is_ok());
        let u = run(&d(Rule::Hash, "uuid"), "x");
        assert_eq!((u.len(), &u[14..15]), (36, "4"));
        assert!(run(&d(Rule::Hash, "bytea"), "x").starts_with("\\x"));
        let mut t = d(Rule::Hash, "varchar");
        t.fit = Some(20);
        assert_eq!(run(&t, "cus_123").len(), 20);
    }

    #[test]
    fn phones_keep_their_shape_and_uk_ones_are_drama_numbers() {
        let p = d(Rule::Phone, "varchar");
        let out = run(&p, "07911 123456");
        assert!(out.starts_with("07700 900"), "{out}");
        assert_eq!(out.len(), 12);
        let out = run(&p, "+44 7911 123456");
        assert!(out.starts_with("+44 7700 900"), "{out}");
        let out = run(&p, "+1 (415) 555-0132");
        assert!(
            out.starts_with("+1 (4") && out.contains(") ") && out.contains('-'),
            "{out}"
        );
        let mut u = p.clone();
        u.unique = true;
        assert!(
            !run(&u, "07911 123456").starts_with("07700 900"),
            "unique: hashed digits"
        );
        // Letters aren't a number's shape: nothing of them survives.
        for given in ["ask for Alice on 07911 123456", "N/A"] {
            let out = run(&p, given);
            assert!(out.chars().all(|c| c.is_ascii_digit()), "{given}: {out}");
            assert!(out.starts_with("07700900") && out.len() == 11, "{out}");
            let out = run(&u, given);
            assert!(
                out.chars().all(|c| c.is_ascii_digit()) && out.len() == 11,
                "{out}"
            );
        }
    }

    #[test]
    fn names_ips_postcodes_and_years() {
        let full = run(&d(Rule::Name(NamePart::Full), "varchar"), "Alice Smith");
        assert_eq!(full.split(' ').count(), 2, "{full}");
        let mut u = d(Rule::Name(NamePart::First), "varchar");
        u.unique = true;
        assert_eq!(
            run(&u, "Alice").split(' ').count(),
            2,
            "a suffix when unique"
        );
        let ip = run(&d(Rule::Ip, "inet"), "81.2.69.160");
        assert!(
            ["192.0.2.", "198.51.100.", "203.0.113."]
                .iter()
                .any(|n| ip.starts_with(n)),
            "{ip}"
        );
        assert!(run(&d(Rule::Ip, "inet"), "10.0.0.0/8").ends_with("/8"));
        // 762 IPv4 documentation addresses can't keep a unique column
        // apart; IPv6's range can.
        let mut u = d(Rule::Ip, "inet");
        u.unique = true;
        assert!(run(&u, "81.2.69.160").starts_with("2001:db8:"));
        // A unique name shortened to fit keeps its tag.
        let mut u = d(Rule::Name(NamePart::Full), "varchar");
        u.unique = true;
        u.fit = Some(14);
        let short = run(&u, "Alice Smith");
        assert!(short.chars().count() <= 14, "{short}");
        assert_eq!(
            short.rsplit(' ').next().unwrap().len(),
            8,
            "the tag survives: {short}"
        );
        assert!(run(&d(Rule::Ip, "inet"), "2a00:1450::1").starts_with("2001:db8:"));
        assert_eq!(postcode("sw1a 1aa"), "SW1A");
        assert_eq!(postcode("EC1A1BB"), "EC1A");
        assert_eq!(year("1987-06-05"), "1987-01-01");
        assert_eq!(year("1987-06-05 10:00:00+01"), "1987-01-01 00:00:00");
        assert_eq!(year("infinity"), "1970-01-01");
    }

    #[test]
    fn a_rows_dates_shift_together() {
        let s = d(Rule::DateShift(30), "date");
        let a = String::from_utf8(m().apply(&s, Some(b"2026-03-01"), b"row 1").unwrap()).unwrap();
        let b = String::from_utf8(m().apply(&s, Some(b"2026-03-05"), b"row 1").unwrap()).unwrap();
        let day = |t: &str| {
            days_from_civil(
                t[..4].parse().unwrap(),
                t[5..7].parse().unwrap(),
                t[8..10].parse().unwrap(),
            )
        };
        assert_eq!(
            day(&b) - day(&a),
            4,
            "the interval within a row survives: {a} {b}"
        );
        assert!((day(&a) - day("2026-03-01")).abs() <= 30);
        assert_eq!(shift("2024-02-28 13:45:00+00", 1), "2024-02-29 13:45:00+00");
        assert_eq!(shift("2024-12-31", 1), "2025-01-01");
        assert_eq!(shift("infinity", 3), "1970-01-01");
    }

    #[test]
    fn passwords_take_the_inputs_format() {
        assert_eq!(
            password("", "$2b$12$abc"),
            TEST_BCRYPT.replacen("$2a$", "$2b$", 1)
        );
        assert_eq!(
            password("", "{bcrypt}$2a$10$abc"),
            format!("{{bcrypt}}{TEST_BCRYPT}")
        );
        assert_eq!(password("", "{noop}secret"), "{noop}pgman-test");
        assert_eq!(
            password("", "{pbkdf2}abcd"),
            format!("{{bcrypt}}{TEST_BCRYPT}")
        );
        assert_eq!(
            password("", "5f4dcc3b5aa765d61d8327deb882cf99"),
            TEST_BCRYPT
        );
        assert_eq!(password("$2a$10$given", "x"), "$2a$10$given");
    }
}
