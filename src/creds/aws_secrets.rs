//! Database credentials from AWS Secrets Manager.
//!
//! A connection names a secret; the value is fetched at connect time and
//! goes no further than the DSN handed to the driver — never `tracing`,
//! never a tool result, never `Debug` output ([`SecretCreds`] redacts).
//!
//! Subprocess-based, for the reasons `tunnel.rs` gives for `ssh`: the
//! system `aws` CLI already honours the operator's profiles, SSO cache and
//! `credential_process`, and it adds no crate to the dependency tree. The
//! ambient profile is used unless the connection names one. Every argument
//! pgman passes is validated first, so a config value can never be read by
//! the CLI as an option.

use std::process::{Command, Stdio};

/// A connection's reference to its secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRef {
    /// Secret name or ARN.
    pub id: String,
    pub region: Option<String>,
    pub profile: Option<String>,
    /// When set, the caller identity's account must match before the
    /// secret is read — a wrong ambient profile then fails, rather than
    /// reading a same-named secret in another account.
    pub account: Option<String>,
}

impl SecretRef {
    /// Check every field is safe to hand the CLI as an argument value.
    pub fn validate(&self) -> Result<(), String> {
        let ok = |v: &str, extra: &str, max: usize| {
            !v.is_empty()
                && v.len() <= max
                && !v.starts_with('-')
                && v.chars()
                    .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
        };
        if !ok(&self.id, "/_+=.@-:", 2048) {
            return Err(format!(
                "secret {:?} is not a valid secret name or ARN",
                self.id
            ));
        }
        if let Some(r) = &self.region {
            if !ok(r, "-", 32) {
                return Err(format!("aws_region {r:?} is not a region name"));
            }
        }
        if let Some(p) = &self.profile {
            if !ok(p, "_.-", 128) {
                return Err(format!("aws_profile {p:?} is not a profile name"));
            }
        }
        if let Some(a) = &self.account {
            if a.len() != 12 || !a.chars().all(|c| c.is_ascii_digit()) {
                return Err(format!("aws_account {a:?} is not a 12-digit account id"));
            }
        }
        Ok(())
    }

    /// How the secret is described wherever its value can't be shown.
    pub fn provenance(&self) -> String {
        let mut s = format!("AWS Secrets Manager secret {}", self.id);
        let mut bits = Vec::new();
        if let Some(r) = &self.region {
            bits.push(format!("region {r}"));
        }
        bits.push(match &self.profile {
            Some(p) => format!("profile {p}"),
            None => "ambient profile".to_string(),
        });
        if let Some(a) = &self.account {
            bits.push(format!("account {a} checked first"));
        }
        s.push_str(&format!(" ({}), read at connect", bits.join(", ")));
        s
    }
}

/// What a secret yields. `Debug` never shows the password.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretCreds {
    pub username: Option<String>,
    pub password: String,
}

impl std::fmt::Debug for SecretCreds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretCreds")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Parse a `SecretString`: a JSON object with `password` and optionally
/// `username` (or `user`) — the RDS-managed shape and the usual hand-made
/// one. Errors name the fields, never the text.
pub fn parse_secret_string(text: &str) -> Result<SecretCreds, String> {
    let v: serde_json::Value = serde_json::from_str(text.trim())
        .map_err(|_| "the secret is not a JSON object".to_string())?;
    let obj = v
        .as_object()
        .ok_or_else(|| "the secret is not a JSON object".to_string())?;
    let password = obj
        .get("password")
        .and_then(|p| p.as_str())
        .filter(|p| !p.is_empty())
        .ok_or_else(|| "the secret has no non-empty string \"password\" field".to_string())?
        .to_string();
    let username = obj
        .get("username")
        .or_else(|| obj.get("user"))
        .and_then(|u| u.as_str())
        .filter(|u| !u.is_empty())
        .map(str::to_string);
    Ok(SecretCreds { username, password })
}

/// Region and profile flags, shared by both calls.
fn common_args(r: &SecretRef) -> Vec<String> {
    let mut args = vec![
        "--output".to_string(),
        "text".to_string(),
        "--cli-connect-timeout".to_string(),
        "10".to_string(),
        "--cli-read-timeout".to_string(),
        "10".to_string(),
    ];
    if let Some(region) = &r.region {
        args.extend(["--region".to_string(), region.clone()]);
    }
    if let Some(profile) = &r.profile {
        args.extend(["--profile".to_string(), profile.clone()]);
    }
    args
}

/// `aws secretsmanager get-secret-value …` arguments. Pure so the shape
/// is tested without spawning anything.
pub fn get_secret_args(r: &SecretRef) -> Vec<String> {
    let mut args = vec![
        "secretsmanager".to_string(),
        "get-secret-value".to_string(),
        "--secret-id".to_string(),
        r.id.clone(),
        "--query".to_string(),
        "SecretString".to_string(),
    ];
    args.extend(common_args(r));
    args
}

/// `aws sts get-caller-identity …` arguments.
pub fn caller_account_args(r: &SecretRef) -> Vec<String> {
    let mut args = vec![
        "sts".to_string(),
        "get-caller-identity".to_string(),
        "--query".to_string(),
        "Account".to_string(),
    ];
    args.extend(common_args(r));
    args
}

/// Run `aws` with `args`; stdout on success. A failure carries the CLI's
/// own stderr (trimmed, capped), which names the problem — expired SSO,
/// access denied, no such secret — and never holds a secret value.
fn run_aws(args: &[String]) -> Result<String, String> {
    let out = Command::new("aws")
        .args(args)
        .env("AWS_PAGER", "")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("could not run the aws CLI ({e}); is it installed and on PATH?"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err: String = err.trim().chars().take(500).collect();
        return Err(format!("aws {} failed: {err}", args[..2].join(" ")));
    }
    String::from_utf8(out.stdout).map_err(|_| "the aws CLI returned non-UTF-8 output".to_string())
}

/// Fetch and parse the secret, checking the account first when the
/// reference names one. Blocking: call it from `spawn_blocking`.
pub fn fetch(r: &SecretRef) -> Result<SecretCreds, String> {
    r.validate()?;
    if let Some(want) = &r.account {
        let got = run_aws(&caller_account_args(r))?;
        let got = got.trim();
        if got != want {
            return Err(format!(
                "the AWS credentials in use are for account {got}, not {want}; \
                 refusing to read {}",
                r.id
            ));
        }
    }
    let text = run_aws(&get_secret_args(r))?;
    parse_secret_string(&text).map_err(|e| format!("secret {}: {e}", r.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(id: &str) -> SecretRef {
        SecretRef {
            id: id.to_string(),
            region: Some("us-west-1".into()),
            profile: None,
            account: None,
        }
    }

    #[test]
    fn rds_and_hand_made_secret_shapes_parse() {
        let rds =
            r#"{"username":"ledger","password":"p@ss","engine":"postgres","host":"x","port":5432}"#;
        assert_eq!(
            parse_secret_string(rds),
            Ok(SecretCreds {
                username: Some("ledger".into()),
                password: "p@ss".into()
            })
        );
        let hand = "{\"user\": \"ro\", \"password\": \"x\"}\n";
        assert_eq!(
            parse_secret_string(hand).unwrap().username.as_deref(),
            Some("ro")
        );
        let bare = r#"{"password":"x"}"#;
        assert_eq!(parse_secret_string(bare).unwrap().username, None);
    }

    #[test]
    fn a_bad_secret_is_described_without_its_text() {
        for text in [
            "hunter2",
            "[\"hunter2\"]",
            "{\"pass\":\"hunter2\"}",
            "{\"password\":\"\"}",
            "{\"password\":42}",
        ] {
            let err = parse_secret_string(text).unwrap_err();
            assert!(!err.contains("hunter2"), "{text}: {err}");
        }
    }

    #[test]
    fn debug_never_shows_the_password() {
        let c = SecretCreds {
            username: Some("u".into()),
            password: "hunter2".into(),
        };
        let shown = format!("{c:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("redacted"), "{shown}");
    }

    #[test]
    fn nothing_the_config_says_can_become_a_cli_option() {
        assert!(r("demo/database/ledger_replica/credentials")
            .validate()
            .is_ok());
        assert!(
            r("arn:aws:secretsmanager:us-west-1:210987654321:secret:prod/x-AbCd")
                .validate()
                .is_ok()
        );
        for bad in [
            "",
            "--endpoint-url=http://evil",
            "-x",
            "a b",
            "a;b",
            "a$(b)",
        ] {
            assert!(r(bad).validate().is_err(), "{bad:?}");
        }
        let mut x = r("ok");
        x.region = Some("--debug".into());
        assert!(x.validate().is_err());
        let mut x = r("ok");
        x.profile = Some("-p".into());
        assert!(x.validate().is_err());
        let mut x = r("ok");
        x.account = Some("54084755703".into());
        assert!(x.validate().is_err());
        x.account = Some("210987654321".into());
        assert!(x.validate().is_ok());
    }

    #[test]
    fn the_cli_is_asked_for_exactly_the_secret_string() {
        let mut x = r("demo/database/ledger/credentials");
        x.profile = Some("examplecorp".into());
        let args = get_secret_args(&x);
        assert_eq!(
            args[..6],
            [
                "secretsmanager",
                "get-secret-value",
                "--secret-id",
                "demo/database/ledger/credentials",
                "--query",
                "SecretString"
            ]
        );
        let tail = args[6..].join(" ");
        assert!(tail.contains("--output text"), "{tail}");
        assert!(tail.contains("--region us-west-1"), "{tail}");
        assert!(tail.contains("--profile examplecorp"), "{tail}");
        assert_eq!(
            caller_account_args(&x)[..4],
            ["sts", "get-caller-identity", "--query", "Account"]
        );
    }

    #[test]
    fn provenance_names_the_secret_and_where_it_is_read() {
        let mut x = r("demo/database/ledger/credentials");
        x.account = Some("210987654321".into());
        let p = x.provenance();
        assert!(p.contains("demo/database/ledger/credentials"), "{p}");
        assert!(
            p.contains("ambient profile") && p.contains("210987654321"),
            "{p}"
        );
    }
}
